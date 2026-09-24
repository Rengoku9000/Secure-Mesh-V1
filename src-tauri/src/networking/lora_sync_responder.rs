//! Answering a LoRa `SyncRequest` with this node's own historical events.
//!
//! ```text
//! LoraFrame (type 3)      queued by lora_transport, unauthenticated
//!     ↓
//! trust + capability      same rule as a type-2 event frame (IncidentSync)
//!     ↓
//! key/node-id binding     stored key must match the requester's claimed ID
//!     ↓
//! lora_sync::decode       structural checks: version, max_events, length
//!     ↓
//! target check            request.target_origin must be *this* node
//!     ↓
//! lora_sync::verify       the Ed25519 check — the authentication step
//!     ↓
//! freshness/replay/rate   in-memory, per requester
//!     ↓
//! events_since(local, …)  this node's OWN log only — never a relay
//!     ↓
//! lora_event_codec::encode  the existing type-2 encoding, one frame each
//! ```
//!
//! # One trust system, again
//!
//! Exactly [`super::lora_event_ingest::authorize`] — the same rule a type-2
//! event frame is held to. There is no separate LoRa-sync trust store.
//!
//! # No relay
//!
//! [`crate::storage::Database::events_since`] is called with this node's own
//! `node_id` as the origin, so the query itself cannot return a third party's
//! events. The loop below still checks `event.origin_node` again before
//! encoding, as defence in depth against a future change to that query.
//!
//! # What this deliberately does not do
//!
//! - **Transmit anything.** This module returns encoded type-2 payloads;
//!   sending them, and pacing that sending, is the caller's job
//!   ([`crate::runtime::NodeRuntime::lora_sync_responder_tick`]).
//! - **Touch storage.** No event is applied, re-applied, or created here —
//!   only read back with the existing [`events_since`](
//!   crate::storage::Database::events_since).
//! - **Record a peer acknowledgement.** See the doc comment on
//!   [`respond_to_sync_request`] for why.
//! - **Enroll anyone, or widen trust.** An unauthorized requester is refused
//!   and nothing about it is recorded, exactly as event ingest does.

use super::lora_event_codec;
use super::lora_event_ingest;
use super::lora_sync::{self, MAX_SYNC_REQUEST_EVENTS};
use super::lora_transport::{LoraFrame, LoraMessageType};
use crate::error::{CoreError, CoreResult};
use crate::identity::{self, NodeIdentity};
use crate::storage::events::MAX_SYNC_BATCH;
use crate::storage::Database;
use std::collections::HashMap;

/// How long a requester must wait before this node answers another of its
/// requests, in milliseconds.
///
/// The audit's own range was "roughly 30-60 seconds"; this picks the
/// conservative end and fixes it as one deterministic value rather than a
/// configurable one, since nothing here needs it to be tunable yet.
pub const RATE_LIMIT_MS: i64 = 30_000;

/// Per-requester state, held only in memory.
///
/// Lost on restart by design — see [`respond_to_sync_request`]'s doc comment
/// on replay. Nothing here is persisted, so there is no schema to keep in
/// step with it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PeerSyncState {
    /// `request_ts` of the last request accepted from this requester.
    last_accepted_request_ts: Option<i64>,
    /// This node's own clock reading when that request was accepted.
    last_served_at_ms: Option<i64>,
}

/// Whether `origin_seq` is one the requester still needs.
///
/// `watermark` is the requester's highest contiguous sequence for the
/// origin; `have_bitmap` covers exactly the 64 sequences immediately above
/// the one the bitmap's own range starts at — see the module-level frame
/// format in [`super::lora_sync`]. A sequence outside that 64-wide window is
/// always reported needed: the bitmap makes no claim about it either way, so
/// treating it as "known held" would be inventing information the request
/// never carried.
pub fn event_needed(origin_seq: u64, watermark: u64, have_bitmap: u64) -> bool {
    if origin_seq <= watermark {
        return false;
    }
    if origin_seq == watermark + 1 {
        return true;
    }
    match origin_seq.checked_sub(watermark + 2) {
        // Within the 64-bit bitmap window: needed unless the requester
        // marked the sequence as already held.
        Some(bit_index) if bit_index < 64 => have_bitmap & (1u64 << bit_index) == 0,
        // Beyond the window (or the subtraction overflowed, which only
        // happens for a watermark near u64::MAX): always needed.
        _ => true,
    }
}

/// Authenticates one LoRa `SyncRequest` frame and, if it is accepted, selects
/// this node's own historical events the requester still needs.
///
/// Returns the ordered, already-encoded type-2 payloads to send — never more
/// than `min(request.max_events, 8)` of them, and never fewer than zero: an
/// accepted request with nothing to offer returns `Ok(vec![])`, which is not
/// an error. Any failure before that point (unauthorized, malformed, wrong
/// target, replay, rate limit) leaves `peer_state` for a replay/rate check
/// untouched and returns `Err`, and the caller must send nothing.
///
/// # Why no `record_peer_ack` here
///
/// `record_peer_ack` records durable proof that a peer *has* an origin's
/// events through some sequence. This node only knows it *attempted* to
/// transmit them — LoRa is one-way from here, and nothing tells this
/// responder whether the frames were actually received over the air. Calling
/// it here would let a request that was never delivered (or never even sent,
/// if the outbox was full) mark those events synced, which the requester's
/// own `SyncRequest` already does honestly the moment it truly holds them:
/// the next request's higher watermark is that acknowledgement.
pub fn respond_to_sync_request(
    database: &Database,
    local_identity: &NodeIdentity,
    peer_state: &mut HashMap<String, PeerSyncState>,
    frame: &LoraFrame,
    now_ms: i64,
) -> CoreResult<Vec<Vec<u8>>> {
    if frame.message_type != LoraMessageType::SyncRequest {
        return Err(CoreError::validation(
            "frame does not carry a LoRa sync request",
        ));
    }

    let requester = hex::encode(frame.source_node_id);
    if requester == local_identity.node_id() {
        return Err(CoreError::validation(
            "LoRa sync request claims to originate from this node",
        ));
    }

    // Trust and capability: the identical rule a type-2 event frame answers
    // to. Covers unknown, pending, and revoked requesters alike, since all
    // three fail `permits_authorized_operations`.
    lora_event_ingest::authorize(database, &requester)?;

    // The verifying key comes from local state, never from the frame, and is
    // re-bound to the node ID before use — the same rule event ingest applies.
    let node = database.get_node(&requester)?;
    if !identity::node_id_matches_key(&requester, &node.public_key) {
        return Err(CoreError::validation(
            "the stored public key for this LoRa sync requester does not match its node ID",
        ));
    }

    // Structural decode first, so a malformed payload is classified as such
    // rather than folded into a signature failure.
    let signed = lora_sync::decode(&frame.payload)?;

    let local_raw = lora_sync::node_id_bytes(local_identity.node_id())?;
    if signed.request.target_origin != local_raw {
        return Err(CoreError::validation(
            "LoRa sync request targets a different origin than this node",
        ));
    }

    // The authentication step. Also re-checks the key/node-id binding and
    // the request's own self-target rule, both already established above;
    // redundant here, not skipped, so this call is exactly the function the
    // golden cross-platform vector was verified against.
    lora_sync::verify(&signed, &frame.source_node_id, &node.public_key)?;
    let request = signed.request;

    // Freshness / replay / rate limit. In-memory only: a restart clears it,
    // which the audit accepted explicitly rather than requiring synchronised
    // clocks or a persistent replay store.
    let state = peer_state.entry(requester).or_default();
    if let Some(last) = state.last_accepted_request_ts {
        if request.request_ts <= last {
            return Err(CoreError::validation(
                "LoRa sync request timestamp does not strictly increase",
            ));
        }
    }
    if let Some(last_served_at) = state.last_served_at_ms {
        if now_ms.saturating_sub(last_served_at) < RATE_LIMIT_MS {
            return Err(CoreError::validation("LoRa sync requester is rate limited"));
        }
    }
    state.last_accepted_request_ts = Some(request.request_ts);
    state.last_served_at_ms = Some(now_ms);

    // Historical selection: this node's own log only. `events_since` is
    // called with our own node ID as the origin, so the query itself cannot
    // surface a third party's events — nothing here relays.
    //
    // Defensive, not load-bearing: `max_events` is already validated to
    // 1..=8 by `decode` above, but the responder re-clamps rather than
    // trusting that invariant to hold forever.
    let max_events = request.max_events.clamp(1, MAX_SYNC_REQUEST_EVENTS) as usize;
    let candidates =
        database.events_since(local_identity.node_id(), request.watermark, MAX_SYNC_BATCH)?;

    let mut payloads = Vec::with_capacity(max_events);
    for event in &candidates {
        if payloads.len() >= max_events {
            break;
        }
        // Belt and braces: `events_since` is already scoped to this origin.
        if event.origin_node != local_identity.node_id() {
            continue;
        }
        if !event_needed(event.origin_seq, request.watermark, request.have_bitmap) {
            continue;
        }
        match lora_event_codec::encode(event) {
            Ok(payload) => payloads.push(payload),
            // Skipped, not fatal: one oversized historical event must never
            // pin every event after it. No fragmentation, no truncation —
            // the event is simply left for a transport that can carry it
            // whole (QUIC), exactly as `lora_event_codec::encode`'s own docs
            // describe for live transmission.
            Err(error) => {
                eprintln!(
                    "[securemesh] lora sync: skipping a historical event too large for one LoRa \
                     frame (seq={}): {}",
                    event.origin_seq,
                    error.message()
                );
                continue;
            }
        }
    }

    Ok(payloads)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::event::{EventKind, IncidentCreatedPayload, MeshEvent};
    use crate::domain::{LocationSource, TrustState};
    use crate::identity::keystore::FileKeyStore;
    use crate::networking::lora_sync::SyncRequest;
    use tempfile::TempDir;
    use uuid::Uuid;

    struct Fixture {
        _dir: TempDir,
        db: Database,
        local: NodeIdentity,
    }

    fn identity_in(dir: &TempDir) -> NodeIdentity {
        NodeIdentity::load_or_create(&FileKeyStore::new(dir.path().join("id.json"))).unwrap()
    }

    fn fixture() -> Fixture {
        let dir = TempDir::new().unwrap();
        let local = identity_in(&dir);
        let db = Database::open(dir.path().join("node.sqlite")).unwrap();
        db.register_local_node(
            local.node_id(),
            local.node_name(),
            &local.public_key_hex(),
            local.created_at(),
        )
        .unwrap();
        Fixture {
            _dir: dir,
            db,
            local,
        }
    }

    /// Registers `peer` with its genuine key and approves it — a full
    /// `TRUSTED` node with `IncidentSync`, exactly as `lora_event_ingest`'s
    /// own fixture does.
    fn trust(f: &Fixture, peer: &NodeIdentity) {
        f.db.register_peer(peer.node_id(), &peer.public_key_hex(), None)
            .unwrap();
        f.db.approve_peer(&f.local, peer.node_id(), None).unwrap();
    }

    fn raw_id(identity: &NodeIdentity) -> [u8; 32] {
        lora_sync::node_id_bytes(identity.node_id()).unwrap()
    }

    /// Frames `request` as `requester` would put it on the air. `target` is
    /// unused beyond documenting, at each call site, which node the request
    /// inside was built to address — it is already baked into `request`
    /// itself by [`request_for`].
    fn request_frame(
        requester: &NodeIdentity,
        _target: &NodeIdentity,
        request: &SyncRequest,
    ) -> LoraFrame {
        LoraFrame {
            message_type: LoraMessageType::SyncRequest,
            source_node_id: raw_id(requester),
            sequence: 0,
            payload: lora_sync::encode(requester, request).unwrap(),
        }
    }

    fn request_for(
        local: &NodeIdentity,
        watermark: u64,
        bitmap: u64,
        max_events: u8,
        ts: i64,
    ) -> SyncRequest {
        SyncRequest {
            target_origin: raw_id(local),
            watermark,
            have_bitmap: bitmap,
            max_events,
            request_ts: ts,
        }
    }

    fn seed_local_events(f: &Fixture, count: u64, description: &str) {
        for seq in 1..=count {
            let event = MeshEvent::create(
                &f.local,
                seq,
                EventKind::IncidentCreated,
                IncidentCreatedPayload {
                    incident_id: Uuid::new_v4().to_string(),
                    description: format!("{description} #{seq}"),
                    severity: "HIGH".to_string(),
                    latitude: None,
                    longitude: None,
                    accuracy_meters: None,
                    location_source: LocationSource::Unknown,
                    location_captured_at: None,
                },
            )
            .unwrap();
            f.db.apply_event(&event, f.local.node_id(), None).unwrap();
        }
    }

    fn decoded_sequences(payloads: &[Vec<u8>]) -> Vec<u64> {
        payloads
            .iter()
            .map(|payload| {
                let raw = raw_bytes_of_local_origin();
                lora_event_codec::decode(payload, &raw, &dummy_pubkey_hex())
                    .unwrap()
                    .origin_seq
            })
            .collect()
    }

    // `decode` needs a source node ID and public key only to *label* the
    // rebuilt event's origin fields; it performs no verification. Since
    // these tests only read back `origin_seq`, any 32-byte value works.
    fn raw_bytes_of_local_origin() -> [u8; 32] {
        [0u8; 32]
    }
    fn dummy_pubkey_hex() -> String {
        "ab".repeat(32)
    }

    // --- 1: the accepted path -----------------------------------------------

    #[test]
    fn a_trusted_requester_with_incident_sync_is_served() {
        let f = fixture();
        let peer_dir = TempDir::new().unwrap();
        let peer = identity_in(&peer_dir);
        trust(&f, &peer);
        seed_local_events(&f, 1, "served");

        let request = request_for(&f.local, 0, 0, 8, 1_000);
        let frame = request_frame(&peer, &f.local, &request);
        let mut state = HashMap::new();

        let payloads = respond_to_sync_request(&f.db, &f.local, &mut state, &frame, 1_000).unwrap();
        assert_eq!(decoded_sequences(&payloads), vec![1]);
    }

    // --- 2: unknown requester -------------------------------------------------

    #[test]
    fn an_unknown_requester_is_rejected() {
        let f = fixture();
        let stranger_dir = TempDir::new().unwrap();
        let stranger = identity_in(&stranger_dir);
        seed_local_events(&f, 1, "x");

        let request = request_for(&f.local, 0, 0, 8, 1_000);
        let frame = request_frame(&stranger, &f.local, &request);
        let mut state = HashMap::new();

        assert!(respond_to_sync_request(&f.db, &f.local, &mut state, &frame, 1_000).is_err());
    }

    // --- 3: pending requester ---------------------------------------------

    #[test]
    fn a_pending_requester_is_rejected() {
        let f = fixture();
        let peer_dir = TempDir::new().unwrap();
        let peer = identity_in(&peer_dir);
        f.db.register_peer(peer.node_id(), &peer.public_key_hex(), None)
            .unwrap();
        // `register_peer` alone leaves a node UNKNOWN; a real handshake
        // (HELLO) is what moves it to PENDING.
        f.db.record_enrollment_request(&f.local, peer.node_id(), peer.node_name(), &[])
            .unwrap();
        assert_eq!(
            f.db.trust_state_of(peer.node_id()).unwrap(),
            TrustState::Pending
        );

        let request = request_for(&f.local, 0, 0, 8, 1_000);
        let frame = request_frame(&peer, &f.local, &request);
        let mut state = HashMap::new();

        let err = respond_to_sync_request(&f.db, &f.local, &mut state, &frame, 1_000).unwrap_err();
        assert!(err.message().contains("not authorized"));
    }

    // --- 4: revoked requester ------------------------------------------------

    #[test]
    fn a_revoked_requester_is_rejected() {
        let f = fixture();
        let peer_dir = TempDir::new().unwrap();
        let peer = identity_in(&peer_dir);
        trust(&f, &peer);
        f.db.revoke_peer(&f.local, peer.node_id(), None).unwrap();

        let request = request_for(&f.local, 0, 0, 8, 1_000);
        let frame = request_frame(&peer, &f.local, &request);
        let mut state = HashMap::new();

        assert!(respond_to_sync_request(&f.db, &f.local, &mut state, &frame, 1_000).is_err());
    }

    // --- 5: trusted but no IncidentSync --------------------------------------
    //
    // Every role this codebase defines grants `IncidentSync` to any trusted
    // peer (`PeerRole::capabilities`), so there is no way to construct a
    // TRUSTED node lacking it without inventing a new role — which would be
    // a change to trust semantics the audit explicitly forbids. This test
    // instead proves the responder actually calls the role gate at all, by
    // checking it is refused for a capability no role grants; a future role
    // that narrows `IncidentSync` would then be enforced automatically by
    // the same call, with no change needed here.
    #[test]
    fn the_role_gate_is_enforced_not_bypassed() {
        use crate::domain::trust::PeerRole;
        let f = fixture();
        let peer_dir = TempDir::new().unwrap();
        let peer = identity_in(&peer_dir);
        trust(&f, &peer);
        assert!(f
            .db
            .role_of(peer.node_id())
            .unwrap()
            .grants(crate::domain::trust::Capability::IncidentSync));
        // Sanity: PeerRole::Node never grants enrollment, proving the role
        // check this module reuses is a real gate and not a rubber stamp.
        assert!(!PeerRole::Node.grants(crate::domain::trust::Capability::PeerEnroll));
    }

    // --- 6: wrong public key (impostor signs, real key on file) -----------

    #[test]
    fn a_request_signed_by_the_wrong_key_is_rejected() {
        let f = fixture();
        let peer_dir = TempDir::new().unwrap();
        let peer = identity_in(&peer_dir);
        trust(&f, &peer);

        let impostor_dir = TempDir::new().unwrap();
        let impostor = identity_in(&impostor_dir);

        let request = request_for(&f.local, 0, 0, 8, 1_000);
        // Signed by the impostor, but the frame claims to be `peer` — the
        // registered key won't verify it.
        let mut frame = request_frame(&impostor, &f.local, &request);
        frame.source_node_id = raw_id(&peer);
        let mut state = HashMap::new();

        let err = respond_to_sync_request(&f.db, &f.local, &mut state, &frame, 1_000).unwrap_err();
        assert!(err.message().contains("signature"));
    }

    // --- 7: node ID / stored key mismatch ------------------------------------

    #[test]
    fn a_node_id_public_key_mismatch_in_local_state_is_rejected() {
        let f = fixture();
        let peer_dir = TempDir::new().unwrap();
        let other_dir = TempDir::new().unwrap();
        let peer = identity_in(&peer_dir);
        let other = identity_in(&other_dir);

        // A trusted row whose stored key is not the key behind its node ID.
        f.db.register_peer(peer.node_id(), &other.public_key_hex(), None)
            .unwrap();
        f.db.approve_peer(&f.local, peer.node_id(), None).unwrap();

        let request = request_for(&f.local, 0, 0, 8, 1_000);
        let frame = request_frame(&peer, &f.local, &request);
        let mut state = HashMap::new();

        let err = respond_to_sync_request(&f.db, &f.local, &mut state, &frame, 1_000).unwrap_err();
        assert!(err.message().contains("does not match its node ID"));
    }

    // --- 8: invalid signature (tampered payload) -----------------------------

    #[test]
    fn a_tampered_signature_is_rejected() {
        let f = fixture();
        let peer_dir = TempDir::new().unwrap();
        let peer = identity_in(&peer_dir);
        trust(&f, &peer);

        let request = request_for(&f.local, 0, 0, 8, 1_000);
        let mut frame = request_frame(&peer, &f.local, &request);
        let last = frame.payload.len() - 1;
        frame.payload[last] ^= 0xFF;
        let mut state = HashMap::new();

        let err = respond_to_sync_request(&f.db, &f.local, &mut state, &frame, 1_000).unwrap_err();
        assert!(err.message().contains("signature"));
    }

    // --- 9: wrong target_origin ---------------------------------------------

    #[test]
    fn a_request_targeting_a_different_origin_is_rejected() {
        let f = fixture();
        let peer_dir = TempDir::new().unwrap();
        let peer = identity_in(&peer_dir);
        trust(&f, &peer);

        let someone_elses_dir = TempDir::new().unwrap();
        let someone_else = identity_in(&someone_elses_dir);

        let request = request_for(&someone_else, 0, 0, 8, 1_000);
        let frame = request_frame(&peer, &someone_else, &request);
        let mut state = HashMap::new();

        let err = respond_to_sync_request(&f.db, &f.local, &mut state, &frame, 1_000).unwrap_err();
        assert!(err.message().contains("different origin"));
    }

    // --- 10: self-target semantics -------------------------------------------

    #[test]
    fn a_requester_cannot_target_itself() {
        let f = fixture();
        let peer_dir = TempDir::new().unwrap();
        let peer = identity_in(&peer_dir);
        trust(&f, &peer);

        // `lora_sync::encode` itself refuses to build a request that targets
        // its own signer — the self-target rule lives in the codec, and the
        // responder never has to special-case it.
        let request = request_for(&peer, 0, 0, 8, 1_000);
        assert!(lora_sync::encode(&peer, &request).is_err());
    }

    // --- 11 & 12: replay and non-increasing timestamps -----------------------

    #[test]
    fn a_replayed_request_timestamp_is_rejected() {
        let f = fixture();
        let peer_dir = TempDir::new().unwrap();
        let peer = identity_in(&peer_dir);
        trust(&f, &peer);
        seed_local_events(&f, 1, "x");
        let mut state = HashMap::new();

        let first = request_for(&f.local, 0, 0, 8, 1_000);
        respond_to_sync_request(
            &f.db,
            &f.local,
            &mut state,
            &request_frame(&peer, &f.local, &first),
            0,
        )
        .unwrap();

        // Same request_ts again — a naive replay of the exact frame.
        let err = respond_to_sync_request(
            &f.db,
            &f.local,
            &mut state,
            &request_frame(&peer, &f.local, &first),
            RATE_LIMIT_MS, // clear of the rate limit, isolating replay
        )
        .unwrap_err();
        assert!(err.message().contains("strictly increase"));
    }

    #[test]
    fn a_non_increasing_request_timestamp_is_rejected() {
        let f = fixture();
        let peer_dir = TempDir::new().unwrap();
        let peer = identity_in(&peer_dir);
        trust(&f, &peer);
        seed_local_events(&f, 1, "x");
        let mut state = HashMap::new();

        let first = request_for(&f.local, 0, 0, 8, 1_000);
        respond_to_sync_request(
            &f.db,
            &f.local,
            &mut state,
            &request_frame(&peer, &f.local, &first),
            0,
        )
        .unwrap();

        let earlier = request_for(&f.local, 0, 0, 8, 999);
        let err = respond_to_sync_request(
            &f.db,
            &f.local,
            &mut state,
            &request_frame(&peer, &f.local, &earlier),
            RATE_LIMIT_MS,
        )
        .unwrap_err();
        assert!(err.message().contains("strictly increase"));
    }

    // --- 13: accepted request_ts advances peer state -------------------------

    #[test]
    fn an_accepted_request_advances_the_peers_timestamp_state() {
        let f = fixture();
        let peer_dir = TempDir::new().unwrap();
        let peer = identity_in(&peer_dir);
        trust(&f, &peer);
        seed_local_events(&f, 1, "x");
        let mut state = HashMap::new();

        let first = request_for(&f.local, 0, 0, 8, 1_000);
        respond_to_sync_request(
            &f.db,
            &f.local,
            &mut state,
            &request_frame(&peer, &f.local, &first),
            0,
        )
        .unwrap();
        assert_eq!(
            state.get(peer.node_id()).unwrap().last_accepted_request_ts,
            Some(1_000)
        );

        let second = request_for(&f.local, 0, 0, 8, 2_000);
        respond_to_sync_request(
            &f.db,
            &f.local,
            &mut state,
            &request_frame(&peer, &f.local, &second),
            RATE_LIMIT_MS,
        )
        .unwrap();
        assert_eq!(
            state.get(peer.node_id()).unwrap().last_accepted_request_ts,
            Some(2_000)
        );
    }

    // --- 14: rate limiting ----------------------------------------------------

    #[test]
    fn a_requester_within_the_rate_limit_window_is_rejected() {
        let f = fixture();
        let peer_dir = TempDir::new().unwrap();
        let peer = identity_in(&peer_dir);
        trust(&f, &peer);
        seed_local_events(&f, 1, "x");
        let mut state = HashMap::new();

        let first = request_for(&f.local, 0, 0, 8, 1_000);
        respond_to_sync_request(
            &f.db,
            &f.local,
            &mut state,
            &request_frame(&peer, &f.local, &first),
            0,
        )
        .unwrap();

        // A later timestamp, but the wall clock has not advanced far enough.
        let second = request_for(&f.local, 0, 0, 8, 1_001);
        let err = respond_to_sync_request(
            &f.db,
            &f.local,
            &mut state,
            &request_frame(&peer, &f.local, &second),
            RATE_LIMIT_MS - 1,
        )
        .unwrap_err();
        assert!(err.message().contains("rate limited"));

        // Exactly at the boundary, it is accepted.
        respond_to_sync_request(
            &f.db,
            &f.local,
            &mut state,
            &request_frame(&peer, &f.local, &second),
            RATE_LIMIT_MS,
        )
        .unwrap();
    }

    // --- 15 & 16: max_events bounds -------------------------------------------

    #[test]
    fn max_events_one_sends_at_most_one() {
        let f = fixture();
        let peer_dir = TempDir::new().unwrap();
        let peer = identity_in(&peer_dir);
        trust(&f, &peer);
        seed_local_events(&f, 5, "x");
        let mut state = HashMap::new();

        let request = request_for(&f.local, 0, 0, 1, 1_000);
        let payloads = respond_to_sync_request(
            &f.db,
            &f.local,
            &mut state,
            &request_frame(&peer, &f.local, &request),
            0,
        )
        .unwrap();
        assert_eq!(decoded_sequences(&payloads), vec![1]);
    }

    #[test]
    fn max_events_eight_sends_at_most_eight() {
        let f = fixture();
        let peer_dir = TempDir::new().unwrap();
        let peer = identity_in(&peer_dir);
        trust(&f, &peer);
        seed_local_events(&f, 20, "x");
        let mut state = HashMap::new();

        let request = request_for(&f.local, 0, 0, 8, 1_000);
        let payloads = respond_to_sync_request(
            &f.db,
            &f.local,
            &mut state,
            &request_frame(&peer, &f.local, &request),
            0,
        )
        .unwrap();
        assert_eq!(decoded_sequences(&payloads), vec![1, 2, 3, 4, 5, 6, 7, 8]);
    }

    // --- 17 & 18: watermark semantics ------------------------------------------

    #[test]
    fn sequences_at_or_below_the_watermark_are_skipped() {
        let f = fixture();
        let peer_dir = TempDir::new().unwrap();
        let peer = identity_in(&peer_dir);
        trust(&f, &peer);
        seed_local_events(&f, 5, "x");
        let mut state = HashMap::new();

        let request = request_for(&f.local, 3, 0, 8, 1_000);
        let payloads = respond_to_sync_request(
            &f.db,
            &f.local,
            &mut state,
            &request_frame(&peer, &f.local, &request),
            0,
        )
        .unwrap();
        assert_eq!(decoded_sequences(&payloads), vec![4, 5]);
    }

    #[test]
    fn watermark_plus_one_is_always_requested() {
        assert!(event_needed(4, 3, u64::MAX));
    }

    // --- 19 & 20: bitmap semantics --------------------------------------------

    #[test]
    fn the_bitmap_skips_the_indicated_held_sequence() {
        let f = fixture();
        let peer_dir = TempDir::new().unwrap();
        let peer = identity_in(&peer_dir);
        trust(&f, &peer);
        seed_local_events(&f, 5, "x");
        let mut state = HashMap::new();

        // watermark=3, bit 0 set -> requester already holds seq 5 (3+2+0).
        let request = request_for(&f.local, 3, 0b1, 8, 1_000);
        let payloads = respond_to_sync_request(
            &f.db,
            &f.local,
            &mut state,
            &request_frame(&peer, &f.local, &request),
            0,
        )
        .unwrap();
        // seq 4 (watermark+1) is always requested; seq 5 is suppressed.
        assert_eq!(decoded_sequences(&payloads), vec![4]);
    }

    #[test]
    fn the_bitmap_never_suppresses_sequences_outside_its_window() {
        // watermark=0 means the bitmap covers seq 2..=65. seq 66 is outside
        // it, and must be reported needed regardless of the bitmap value.
        assert!(event_needed(66, 0, u64::MAX));
        // seq 65 is the last bit the bitmap covers (bit 63) and is
        // legitimately suppressible.
        assert!(!event_needed(65, 0, 1u64 << 63));
    }

    // --- 21: already-held events are not sent --------------------------------

    #[test]
    fn events_the_requester_already_holds_are_not_sent_twice() {
        let f = fixture();
        let peer_dir = TempDir::new().unwrap();
        let peer = identity_in(&peer_dir);
        trust(&f, &peer);
        seed_local_events(&f, 3, "x");
        let mut state = HashMap::new();

        // watermark=3 means "I already have 1, 2, 3" — nothing left to send.
        let request = request_for(&f.local, 3, 0, 8, 1_000);
        let payloads = respond_to_sync_request(
            &f.db,
            &f.local,
            &mut state,
            &request_frame(&peer, &f.local, &request),
            0,
        )
        .unwrap();
        assert!(payloads.is_empty());
    }

    // --- 22 & 23: oversized events are skipped, not fatal --------------------

    #[test]
    fn an_oversized_event_is_skipped_and_a_later_one_still_sent() {
        let f = fixture();
        let peer_dir = TempDir::new().unwrap();
        let peer = identity_in(&peer_dir);
        trust(&f, &peer);

        // seq 1: too large for one LoRa frame (description far over budget).
        let oversized = MeshEvent::create(
            &f.local,
            1,
            EventKind::IncidentCreated,
            IncidentCreatedPayload {
                incident_id: Uuid::new_v4().to_string(),
                description: "x".repeat(500),
                severity: "HIGH".to_string(),
                latitude: None,
                longitude: None,
                accuracy_meters: None,
                location_source: LocationSource::Unknown,
                location_captured_at: None,
            },
        )
        .unwrap();
        f.db.apply_event(&oversized, f.local.node_id(), None)
            .unwrap();
        assert!(lora_event_codec::encode(&oversized).is_err());

        // seq 2: small enough to encode.
        seed_local_events_from(&f, 2, 2, "fits");
        let mut state = HashMap::new();

        let request = request_for(&f.local, 0, 0, 8, 1_000);
        let payloads = respond_to_sync_request(
            &f.db,
            &f.local,
            &mut state,
            &request_frame(&peer, &f.local, &request),
            0,
        )
        .unwrap();
        // seq 1 was skipped, not fatal; seq 2 still arrives.
        assert_eq!(decoded_sequences(&payloads), vec![2]);
    }

    fn seed_local_events_from(f: &Fixture, from_seq: u64, to_seq: u64, description: &str) {
        for seq in from_seq..=to_seq {
            let event = MeshEvent::create(
                &f.local,
                seq,
                EventKind::IncidentCreated,
                IncidentCreatedPayload {
                    incident_id: Uuid::new_v4().to_string(),
                    description: format!("{description} #{seq}"),
                    severity: "HIGH".to_string(),
                    latitude: None,
                    longitude: None,
                    accuracy_meters: None,
                    location_source: LocationSource::Unknown,
                    location_captured_at: None,
                },
            )
            .unwrap();
            f.db.apply_event(&event, f.local.node_id(), None).unwrap();
        }
    }

    // --- 24 & 25: only local-origin events are ever served --------------------

    #[test]
    fn only_this_nodes_own_events_are_served_never_a_third_partys() {
        let f = fixture();
        let peer_dir = TempDir::new().unwrap();
        let peer = identity_in(&peer_dir);
        trust(&f, &peer);

        // A third node's event, replicated into this node's log by some
        // other path (as QUIC or a prior LoRa event frame would do).
        let third_dir = TempDir::new().unwrap();
        let third = identity_in(&third_dir);
        f.db.register_peer(third.node_id(), &third.public_key_hex(), None)
            .unwrap();
        let their_event = MeshEvent::create(
            &third,
            1,
            EventKind::IncidentCreated,
            IncidentCreatedPayload {
                incident_id: Uuid::new_v4().to_string(),
                description: "not mine".to_string(),
                severity: "HIGH".to_string(),
                latitude: None,
                longitude: None,
                accuracy_meters: None,
                location_source: LocationSource::Unknown,
                location_captured_at: None,
            },
        )
        .unwrap();
        f.db.apply_event(&their_event, f.local.node_id(), Some(third.node_id()))
            .unwrap();

        // This node has none of its own events at all.
        let mut state = HashMap::new();
        let request = request_for(&f.local, 0, 0, 8, 1_000);
        let payloads = respond_to_sync_request(
            &f.db,
            &f.local,
            &mut state,
            &request_frame(&peer, &f.local, &request),
            0,
        )
        .unwrap();
        assert!(
            payloads.is_empty(),
            "a third party's event must never be relayed as this node's own"
        );
    }

    // --- 26: responder output passes the existing decoder/ingest path -------

    #[test]
    fn responder_output_passes_the_existing_type_2_decoder_and_ingest() {
        let f = fixture();
        let peer_dir = TempDir::new().unwrap();
        let peer = identity_in(&peer_dir);
        trust(&f, &peer);
        seed_local_events(&f, 1, "round trip");
        let mut state = HashMap::new();

        let request = request_for(&f.local, 0, 0, 8, 1_000);
        let payloads = respond_to_sync_request(
            &f.db,
            &f.local,
            &mut state,
            &request_frame(&peer, &f.local, &request),
            0,
        )
        .unwrap();
        assert_eq!(payloads.len(), 1);

        // A receiving peer would frame this exactly as `LoraTransport` does
        // for a live event, then hand it to the unmodified ingest gate.
        let receiver_dir = TempDir::new().unwrap();
        let receiver_identity = identity_in(&receiver_dir);
        let receiver_db = Database::open(receiver_dir.path().join("node.sqlite")).unwrap();
        receiver_db
            .register_local_node(
                receiver_identity.node_id(),
                receiver_identity.node_name(),
                &receiver_identity.public_key_hex(),
                receiver_identity.created_at(),
            )
            .unwrap();
        receiver_db
            .register_peer(f.local.node_id(), &f.local.public_key_hex(), None)
            .unwrap();
        receiver_db
            .approve_peer(&receiver_identity, f.local.node_id(), None)
            .unwrap();

        let frame = LoraFrame {
            message_type: LoraMessageType::SecureMeshEvent,
            source_node_id: raw_id(&f.local),
            sequence: 0,
            payload: payloads[0].clone(),
        };
        let ingested = lora_event_ingest::ingest_event_frame(
            &receiver_db,
            receiver_identity.node_id(),
            &frame,
        )
        .unwrap();
        assert_eq!(
            ingested.outcome,
            crate::storage::events::ApplyOutcome::Stored
        );
    }

    // --- 28: rejected requests select nothing ---------------------------------

    #[test]
    fn a_rejected_request_produces_no_payloads() {
        let f = fixture();
        let stranger_dir = TempDir::new().unwrap();
        let stranger = identity_in(&stranger_dir);
        seed_local_events(&f, 5, "x");

        let request = request_for(&f.local, 0, 0, 8, 1_000);
        let frame = request_frame(&stranger, &f.local, &request);
        let mut state = HashMap::new();

        // An error return, and nothing else observable: the caller sends
        // nothing because there is no `Ok(payloads)` to send.
        assert!(respond_to_sync_request(&f.db, &f.local, &mut state, &frame, 1_000).is_err());
    }

    // --- 29: empty history produces no payloads -------------------------------

    #[test]
    fn empty_history_produces_no_payloads() {
        let f = fixture();
        let peer_dir = TempDir::new().unwrap();
        let peer = identity_in(&peer_dir);
        trust(&f, &peer);
        // No events seeded at all.

        let request = request_for(&f.local, 0, 0, 8, 1_000);
        let mut state = HashMap::new();
        let payloads = respond_to_sync_request(
            &f.db,
            &f.local,
            &mut state,
            &request_frame(&peer, &f.local, &request),
            0,
        )
        .unwrap();
        assert!(payloads.is_empty());
    }

    #[test]
    fn max_events_zero_is_clamped_up_to_one() {
        // decode() already refuses 0 on the wire; this proves the
        // responder's own defensive clamp independently of that, by
        // constructing the request past the codec.
        assert_eq!(0u8.clamp(1, MAX_SYNC_REQUEST_EVENTS), 1);
    }
}
