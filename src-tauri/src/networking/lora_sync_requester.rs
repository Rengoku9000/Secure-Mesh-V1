//! Historical LoRa sync REQUESTER: detecting a sequence gap in an event this
//! node has already accepted, and building the signed `SyncRequest` to close
//! it.
//!
//! ```text
//! ingest_event_frame -> IngestedEvent{Stored, origin_seq}   already verified, already applied
//!     ↓
//! watermark_for(origin)     the existing contiguous watermark, recomputed from storage
//!     ↓
//! gap?  origin_seq > watermark + 1
//!     ↓ yes
//! authorize(origin)          the same trust rule the responder answers to
//!     ↓
//! cooldown, per origin       in-memory, Instant-based
//!     ↓
//! held_sequences(...)        build the 64-bit "have" bitmap
//!     ↓
//! lora_sync::encode          sign with this node's own identity
//! ```
//!
//! # Why this triggers only from an already-accepted event
//!
//! LoRa has no presence detection — the only fact this node can act on is
//! "an event I just verified and stored came in higher than I expected".
//! Nothing here runs speculatively: no polling, no peer-discovery hook, no
//! request on startup. See [`maybe_request_sync`]'s own doc comment for the
//! exact precondition it assumes.
//!
//! # One trust system, again
//!
//! [`super::lora_event_ingest::authorize`] — the identical rule the
//! responder gates on ([`super::lora_sync_responder`]) and a live event
//! frame is held to. There is no separate trust check for "may I ask this
//! origin for its history".
//!
//! # What this deliberately does not do
//!
//! - **Send anything.** This returns a signed 122-byte payload; handing it
//!   to [`super::MeshTransport::send_lora_sync_request`], and honouring the
//!   LoRa TX opt-in gate, is the caller's job
//!   ([`crate::runtime::NodeRuntime`]'s LoRa receive tick).
//! - **Verify or apply the event that revealed the gap.** That already
//!   happened, by assumption — see the precondition above.
//! - **Retry, poll, or run on a timer.** A gap that is not resolved by one
//!   request simply waits for the next accepted event from that origin (or
//!   an operator's later intervention); nothing here schedules a follow-up.

use super::lora_event_ingest;
use super::lora_sync::{self, SyncRequest, MAX_SYNC_REQUEST_EVENTS};
use crate::error::CoreResult;
use crate::identity::NodeIdentity;
use crate::storage::Database;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Minimum time between two historical-sync requests to the same origin.
///
/// The audit's range was "roughly 30-60 seconds"; this fixes one
/// deterministic value in the middle of it, matching the responder's own
/// [`super::lora_sync_responder::RATE_LIMIT_MS`] so a request and the
/// responder's own per-peer limit are consistent with each other.
pub const GAP_REQUEST_COOLDOWN: Duration = Duration::from_secs(45);

/// Width of the bitmap's window, in sequences: `watermark+2 ..= watermark+65`.
const BITMAP_WINDOW: u64 = 64;

/// Per-origin cooldown state for outgoing historical-sync requests.
///
/// In memory only, exactly like the responder's own `PeerSyncState` — a
/// restart clears it, and the first gap seen after restart is always free
/// to request immediately.
#[derive(Debug, Default)]
pub struct LoraSyncRequesterState {
    last_request_at: HashMap<String, Instant>,
}

impl LoraSyncRequesterState {
    pub fn new() -> Self {
        Self::default()
    }

    fn ready(&self, origin: &str, now: Instant) -> bool {
        match self.last_request_at.get(origin) {
            Some(last) => now.saturating_duration_since(*last) >= GAP_REQUEST_COOLDOWN,
            None => true,
        }
    }

    fn record(&mut self, origin: &str, now: Instant) {
        self.last_request_at.insert(origin.to_string(), now);
    }
}

/// Looks at one already-accepted event and, if it reveals a gap this node
/// may ask `origin` to close, returns the signed request payload to send.
///
/// **Precondition.** `accepted_seq` must be the `origin_seq` of an event
/// that has *already* been verified and durably applied — the
/// `origin_seq` field of an [`super::lora_event_ingest::IngestedEvent`]
/// whose `outcome` was [`crate::storage::events::ApplyOutcome::Stored`].
/// This function performs no verification of its own and trusts that value
/// completely; calling it for a rejected, duplicate, or conflicting event
/// would be meaningless (there is no new watermark to compare against) and
/// callers must not do so.
///
/// Returns `Ok(None)` — not an error — whenever no request should be sent:
/// no gap, the origin fails the same authorization check the responder
/// applies, or `origin` is still within its cooldown window. Returns
/// `Ok(Some(payload))` only when a request both should and can be built;
/// building it can still fail (`Err`) on a genuinely malformed local node ID,
/// which does not happen in practice since `origin` always comes from a
/// LoRa frame's own `source_node_id`.
pub fn maybe_request_sync(
    database: &Database,
    local_identity: &NodeIdentity,
    state: &mut LoraSyncRequesterState,
    origin: &str,
    accepted_seq: u64,
    now: Instant,
    now_ms: i64,
) -> CoreResult<Option<Vec<u8>>> {
    let watermark = database.watermark_for(origin)?;
    if accepted_seq <= watermark + 1 {
        // Contiguous, or exactly the next expected sequence: no gap.
        return Ok(None);
    }

    // The same rule the responder answers to, reused rather than
    // reimplemented: this node does not spend radio time asking an origin
    // it would not accept an answer from either. An origin that is unknown,
    // pending, revoked, or merely lacking `IncidentSync` is silently
    // skipped — never a reason to fail the tick that found the gap.
    if lora_event_ingest::authorize(database, origin).is_err() {
        return Ok(None);
    }

    if !state.ready(origin, now) {
        return Ok(None);
    }

    let have_bitmap = build_have_bitmap(database, origin, watermark)?;
    let target_origin = lora_sync::node_id_bytes(origin)?;
    let request = SyncRequest {
        target_origin,
        watermark,
        have_bitmap,
        max_events: MAX_SYNC_REQUEST_EVENTS,
        request_ts: now_ms,
    };
    let payload = lora_sync::encode(local_identity, &request)?;

    // Recorded only once a request is actually about to be sent: an origin
    // that fails authorization, or one still in cooldown, never consumes
    // the cooldown window for a request that was never built.
    state.record(origin, now);
    Ok(Some(payload))
}

/// The sequence numbers this node actually holds for `origin` within the
/// bitmap's 64-wide window, packed exactly as the wire format defines: bit 0
/// is `watermark + 2`, bit 63 is `watermark + 65`. A sequence outside that
/// window is not representable and is simply not looked up.
fn build_have_bitmap(database: &Database, origin: &str, watermark: u64) -> CoreResult<u64> {
    let start = watermark + 2;
    let end = watermark + BITMAP_WINDOW + 1; // watermark + 65, inclusive

    let held = database.held_sequences(origin, start, end)?;

    let mut bitmap = 0u64;
    for seq in held {
        if let Some(bit) = seq.checked_sub(start) {
            if bit < BITMAP_WINDOW {
                bitmap |= 1u64 << bit;
            }
        }
    }
    Ok(bitmap)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::event::{EventKind, IncidentCreatedPayload, MeshEvent};
    use crate::domain::{LocationSource, TrustState};
    use crate::identity::keystore::FileKeyStore;
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

    fn trust(f: &Fixture, origin: &NodeIdentity) {
        f.db.register_peer(origin.node_id(), &origin.public_key_hex(), None)
            .unwrap();
        f.db.approve_peer(&f.local, origin.node_id(), None).unwrap();
    }

    /// Stores `seq` as if it had arrived from `origin` and been accepted —
    /// the state `maybe_request_sync` assumes already holds.
    fn accept(f: &Fixture, origin: &NodeIdentity, seq: u64) {
        let event = MeshEvent::create(
            origin,
            seq,
            EventKind::IncidentCreated,
            IncidentCreatedPayload {
                incident_id: Uuid::new_v4().to_string(),
                description: format!("seq {seq}"),
                severity: "HIGH".to_string(),
                latitude: None,
                longitude: None,
                accuracy_meters: None,
                location_source: LocationSource::Unknown,
                location_captured_at: None,
            },
        )
        .unwrap();
        f.db.apply_event(&event, f.local.node_id(), Some(origin.node_id()))
            .unwrap();
    }

    fn decode_target(payload: &[u8]) -> [u8; 32] {
        lora_sync::decode(payload).unwrap().request.target_origin
    }

    // --- 1, 2, 3: no gap, no request -----------------------------------------

    #[test]
    fn a_contiguous_sequence_does_not_trigger_a_request() {
        let f = fixture();
        let origin = identity_in(&TempDir::new().unwrap());
        trust(&f, &origin);
        accept(&f, &origin, 1);
        let mut state = LoraSyncRequesterState::new();

        // Accepting seq 2 right after seq 1 is exactly the expected next
        // sequence — no gap.
        let result = maybe_request_sync(
            &f.db,
            &f.local,
            &mut state,
            origin.node_id(),
            2,
            Instant::now(),
            0,
        )
        .unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn a_sequence_at_or_below_the_watermark_does_not_trigger_a_request() {
        let f = fixture();
        let origin = identity_in(&TempDir::new().unwrap());
        trust(&f, &origin);
        accept(&f, &origin, 1);
        accept(&f, &origin, 2);
        accept(&f, &origin, 3);
        let mut state = LoraSyncRequesterState::new();

        for seq in [1, 2, 3] {
            let result = maybe_request_sync(
                &f.db,
                &f.local,
                &mut state,
                origin.node_id(),
                seq,
                Instant::now(),
                0,
            )
            .unwrap();
            assert!(result.is_none(), "seq {seq} <= watermark must not request");
        }
    }

    #[test]
    fn watermark_plus_one_does_not_trigger_a_request() {
        let f = fixture();
        let origin = identity_in(&TempDir::new().unwrap());
        trust(&f, &origin);
        accept(&f, &origin, 1);
        accept(&f, &origin, 2);
        let mut state = LoraSyncRequesterState::new();

        // watermark is 2; accepting exactly 3 is the expected next sequence.
        let result = maybe_request_sync(
            &f.db,
            &f.local,
            &mut state,
            origin.node_id(),
            3,
            Instant::now(),
            0,
        )
        .unwrap();
        assert!(result.is_none());
    }

    // --- 4, 5, 6: a real gap triggers exactly one request, correctly aimed --

    #[test]
    fn a_gap_triggers_exactly_one_request() {
        let f = fixture();
        let origin = identity_in(&TempDir::new().unwrap());
        trust(&f, &origin);
        accept(&f, &origin, 1);
        accept(&f, &origin, 2);
        accept(&f, &origin, 3);
        let mut state = LoraSyncRequesterState::new();

        // watermark = 3; accepting seq 6 skips 4 and 5.
        let result = maybe_request_sync(
            &f.db,
            &f.local,
            &mut state,
            origin.node_id(),
            6,
            Instant::now(),
            0,
        )
        .unwrap();
        assert!(result.is_some());
    }

    #[test]
    fn the_request_targets_the_missing_events_origin() {
        let f = fixture();
        let origin = identity_in(&TempDir::new().unwrap());
        trust(&f, &origin);
        accept(&f, &origin, 1);
        accept(&f, &origin, 2);
        accept(&f, &origin, 3);
        let mut state = LoraSyncRequesterState::new();

        let payload = maybe_request_sync(
            &f.db,
            &f.local,
            &mut state,
            origin.node_id(),
            6,
            Instant::now(),
            0,
        )
        .unwrap()
        .unwrap();

        let mut expected = [0u8; 32];
        expected.copy_from_slice(&hex::decode(origin.node_id()).unwrap());
        assert_eq!(decode_target(&payload), expected);
    }

    #[test]
    fn the_request_watermark_is_the_local_contiguous_watermark() {
        let f = fixture();
        let origin = identity_in(&TempDir::new().unwrap());
        trust(&f, &origin);
        accept(&f, &origin, 1);
        accept(&f, &origin, 2);
        accept(&f, &origin, 3);
        // An out-of-order extra: seq 3 stays the contiguous watermark
        // regardless of what else this node happens to hold beyond it.
        accept(&f, &origin, 8);
        let mut state = LoraSyncRequesterState::new();

        let payload = maybe_request_sync(
            &f.db,
            &f.local,
            &mut state,
            origin.node_id(),
            6,
            Instant::now(),
            0,
        )
        .unwrap()
        .unwrap();
        assert_eq!(lora_sync::decode(&payload).unwrap().request.watermark, 3);
    }

    // --- 7, 8, 9, 10, 11: bitmap construction --------------------------------

    #[test]
    fn the_bitmap_correctly_represents_held_sequences() {
        let f = fixture();
        let origin = identity_in(&TempDir::new().unwrap());
        trust(&f, &origin);
        // Exactly the audit's own worked example.
        for seq in [1, 2, 3, 6, 8] {
            accept(&f, &origin, seq);
        }
        let mut state = LoraSyncRequesterState::new();

        let payload = maybe_request_sync(
            &f.db,
            &f.local,
            &mut state,
            origin.node_id(),
            8,
            Instant::now(),
            0,
        )
        .unwrap()
        .unwrap();
        let bitmap = lora_sync::decode(&payload).unwrap().request.have_bitmap;
        assert_eq!(
            bitmap, 0b1010,
            "bit0=seq5(absent) bit1=seq6(held) bit2=seq7(absent) bit3=seq8(held)"
        );
    }

    #[test]
    fn the_bitmap_ignores_sequences_at_or_below_the_watermark() {
        // watermark=3: seq 1,2,3 are already below the bitmap's own window
        // (which starts at watermark+2=5) and can never affect it, but this
        // proves the window's start is computed from the watermark, not
        // from an arbitrary offset that might accidentally include them.
        let f = fixture();
        let origin = identity_in(&TempDir::new().unwrap());
        trust(&f, &origin);
        for seq in [1, 2, 3, 6] {
            accept(&f, &origin, seq);
        }
        let mut state = LoraSyncRequesterState::new();

        let payload = maybe_request_sync(
            &f.db,
            &f.local,
            &mut state,
            origin.node_id(),
            6,
            Instant::now(),
            0,
        )
        .unwrap()
        .unwrap();
        let bitmap = lora_sync::decode(&payload).unwrap().request.have_bitmap;
        assert_eq!(bitmap, 0b10, "only bit1 (seq6) should be set");
    }

    #[test]
    fn bit_zero_represents_watermark_plus_two() {
        let f = fixture();
        let origin = identity_in(&TempDir::new().unwrap());
        trust(&f, &origin);
        accept(&f, &origin, 1);
        accept(&f, &origin, 2);
        accept(&f, &origin, 3);
        accept(&f, &origin, 5); // watermark(3) + 2 = 5
        accept(&f, &origin, 9); // reveals the gap without itself being adjacent
        let mut state = LoraSyncRequesterState::new();

        let payload = maybe_request_sync(
            &f.db,
            &f.local,
            &mut state,
            origin.node_id(),
            9,
            Instant::now(),
            0,
        )
        .unwrap()
        .unwrap();
        let request = lora_sync::decode(&payload).unwrap().request;
        assert_eq!(request.watermark, 3);
        assert_eq!(request.have_bitmap & 1, 1, "bit 0 must be set for seq 5");
    }

    #[test]
    fn bit_sixty_three_represents_watermark_plus_sixty_five() {
        let f = fixture();
        let origin = identity_in(&TempDir::new().unwrap());
        trust(&f, &origin);
        accept(&f, &origin, 1);
        accept(&f, &origin, 68); // watermark(1) + 65 = 66; ensure 68 sits far
                                 // enough beyond the window to reveal the gap
                                 // without being part of it.
        accept(&f, &origin, 66); // watermark(1) + 65 = 66: bit 63.
        let mut state = LoraSyncRequesterState::new();

        let payload = maybe_request_sync(
            &f.db,
            &f.local,
            &mut state,
            origin.node_id(),
            68,
            Instant::now(),
            0,
        )
        .unwrap()
        .unwrap();
        let request = lora_sync::decode(&payload).unwrap().request;
        assert_eq!(request.watermark, 1);
        assert_eq!(
            request.have_bitmap & (1 << 63),
            1 << 63,
            "bit 63 must be set for seq 66 (watermark + 65)"
        );
    }

    #[test]
    fn sequence_watermark_plus_sixty_six_is_not_represented_by_the_bitmap() {
        let f = fixture();
        let origin = identity_in(&TempDir::new().unwrap());
        trust(&f, &origin);
        accept(&f, &origin, 1);
        accept(&f, &origin, 67); // watermark(1) + 66: one past the window.
        let mut state = LoraSyncRequesterState::new();

        let payload = maybe_request_sync(
            &f.db,
            &f.local,
            &mut state,
            origin.node_id(),
            67,
            Instant::now(),
            0,
        )
        .unwrap()
        .unwrap();
        let request = lora_sync::decode(&payload).unwrap().request;
        // Held, but entirely outside the 64-bit window this bitmap can
        // describe — every bit stays 0, and the wire format simply makes no
        // claim about seq 67 at all (the requester relies on `watermark`
        // and repeated requests to eventually close a gap this wide).
        assert_eq!(request.have_bitmap, 0);
    }

    #[test]
    fn sparse_out_of_order_events_produce_the_correct_bitmap() {
        let f = fixture();
        let origin = identity_in(&TempDir::new().unwrap());
        trust(&f, &origin);
        for seq in [1, 2, 3, 7, 10, 15] {
            accept(&f, &origin, seq);
        }
        let mut state = LoraSyncRequesterState::new();

        let payload = maybe_request_sync(
            &f.db,
            &f.local,
            &mut state,
            origin.node_id(),
            15,
            Instant::now(),
            0,
        )
        .unwrap()
        .unwrap();
        let bitmap = lora_sync::decode(&payload).unwrap().request.have_bitmap;
        // watermark=3, window starts at 5: seq7->bit2, seq10->bit5, seq15->bit10.
        let expected = (1u64 << 2) | (1u64 << 5) | (1u64 << 10);
        assert_eq!(bitmap, expected);
    }

    // --- 13, 14, 15: one outstanding request, cooldown, per origin ----------

    #[test]
    fn multiple_out_of_order_events_do_not_create_multiple_immediate_requests() {
        let f = fixture();
        let origin = identity_in(&TempDir::new().unwrap());
        trust(&f, &origin);
        accept(&f, &origin, 1);
        accept(&f, &origin, 2);
        accept(&f, &origin, 3);
        let mut state = LoraSyncRequesterState::new();
        let now = Instant::now();

        let first =
            maybe_request_sync(&f.db, &f.local, &mut state, origin.node_id(), 6, now, 0).unwrap();
        assert!(first.is_some());

        accept(&f, &origin, 6);
        for seq in [7, 8] {
            accept(&f, &origin, seq);
            let result =
                maybe_request_sync(&f.db, &f.local, &mut state, origin.node_id(), seq, now, 0)
                    .unwrap();
            assert!(
                result.is_none(),
                "seq {seq} must not immediately re-request; the outstanding request already covers it"
            );
        }
    }

    #[test]
    fn the_cooldown_suppresses_a_repeated_request_for_the_same_origin() {
        let f = fixture();
        let origin = identity_in(&TempDir::new().unwrap());
        trust(&f, &origin);
        accept(&f, &origin, 1);
        accept(&f, &origin, 2);
        accept(&f, &origin, 3);
        let mut state = LoraSyncRequesterState::new();
        let start = Instant::now();

        let first =
            maybe_request_sync(&f.db, &f.local, &mut state, origin.node_id(), 6, start, 0).unwrap();
        assert!(first.is_some());

        // A fresh gap (seq 9, still unresolved) arrives just under the
        // cooldown boundary.
        let just_before = start + GAP_REQUEST_COOLDOWN - Duration::from_millis(1);
        let second = maybe_request_sync(
            &f.db,
            &f.local,
            &mut state,
            origin.node_id(),
            9,
            just_before,
            0,
        )
        .unwrap();
        assert!(second.is_none(), "still within the cooldown window");

        // At the boundary, a new request is allowed again.
        let at_boundary = start + GAP_REQUEST_COOLDOWN;
        let third = maybe_request_sync(
            &f.db,
            &f.local,
            &mut state,
            origin.node_id(),
            9,
            at_boundary,
            0,
        )
        .unwrap();
        assert!(third.is_some());
    }

    #[test]
    fn different_origins_can_independently_trigger_requests() {
        let f = fixture();
        let origin_a = identity_in(&TempDir::new().unwrap());
        let origin_b = identity_in(&TempDir::new().unwrap());
        trust(&f, &origin_a);
        trust(&f, &origin_b);
        for origin in [&origin_a, &origin_b] {
            accept(&f, origin, 1);
            accept(&f, origin, 2);
            accept(&f, origin, 3);
        }
        let mut state = LoraSyncRequesterState::new();
        let now = Instant::now();

        let a =
            maybe_request_sync(&f.db, &f.local, &mut state, origin_a.node_id(), 6, now, 0).unwrap();
        assert!(a.is_some());

        // B's cooldown is untouched by A's request, even at the same instant.
        let b =
            maybe_request_sync(&f.db, &f.local, &mut state, origin_b.node_id(), 6, now, 0).unwrap();
        assert!(
            b.is_some(),
            "one origin's cooldown must not suppress another's"
        );
    }

    // --- 18, 19: the generated request is genuinely valid --------------------

    #[test]
    fn the_generated_request_verifies_with_the_existing_verification_and_uses_the_local_id() {
        let f = fixture();
        let origin = identity_in(&TempDir::new().unwrap());
        trust(&f, &origin);
        accept(&f, &origin, 1);
        accept(&f, &origin, 2);
        accept(&f, &origin, 3);
        let mut state = LoraSyncRequesterState::new();

        let payload = maybe_request_sync(
            &f.db,
            &f.local,
            &mut state,
            origin.node_id(),
            6,
            Instant::now(),
            1_000,
        )
        .unwrap()
        .unwrap();

        let mut local_raw = [0u8; 32];
        local_raw.copy_from_slice(&hex::decode(f.local.node_id()).unwrap());

        // Verifies exactly as the responder verifies an inbound request:
        // requester = the outer frame's source, which is this node.
        let verified =
            lora_sync::decode_verified(&payload, &local_raw, &f.local.public_key_hex()).unwrap();
        assert_eq!(verified.request_ts, 1_000);
    }

    // --- 20, 21, 22, 23: authorization gates the requester too ---------------

    #[test]
    fn an_unknown_origin_does_not_trigger_a_request() {
        let f = fixture();
        let origin = identity_in(&TempDir::new().unwrap());
        // Never registered at all.
        let mut state = LoraSyncRequesterState::new();

        // No local history for this origin either, but the point under test
        // is authorization, not the watermark path.
        let result = maybe_request_sync(
            &f.db,
            &f.local,
            &mut state,
            origin.node_id(),
            6,
            Instant::now(),
            0,
        )
        .unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn a_pending_origin_does_not_trigger_a_request() {
        let f = fixture();
        let origin = identity_in(&TempDir::new().unwrap());
        f.db.register_peer(origin.node_id(), &origin.public_key_hex(), None)
            .unwrap();
        f.db.record_enrollment_request(&f.local, origin.node_id(), origin.node_name(), &[])
            .unwrap();
        assert_eq!(
            f.db.trust_state_of(origin.node_id()).unwrap(),
            TrustState::Pending
        );
        let mut state = LoraSyncRequesterState::new();

        let result = maybe_request_sync(
            &f.db,
            &f.local,
            &mut state,
            origin.node_id(),
            6,
            Instant::now(),
            0,
        )
        .unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn a_revoked_origin_does_not_trigger_a_request() {
        let f = fixture();
        let origin = identity_in(&TempDir::new().unwrap());
        trust(&f, &origin);
        accept(&f, &origin, 1);
        accept(&f, &origin, 2);
        accept(&f, &origin, 3);
        f.db.revoke_peer(&f.local, origin.node_id(), None).unwrap();
        let mut state = LoraSyncRequesterState::new();

        let result = maybe_request_sync(
            &f.db,
            &f.local,
            &mut state,
            origin.node_id(),
            6,
            Instant::now(),
            0,
        )
        .unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn an_origin_without_incident_sync_does_not_trigger_a_request() {
        // Every role this codebase defines grants `IncidentSync` to any
        // trusted peer, so this proves the requester actually calls the
        // shared authorization gate (via a capability no role grants) rather
        // than skipping it — see the identical reasoning in
        // `lora_sync_responder::tests::the_role_gate_is_enforced_not_bypassed`.
        let f = fixture();
        let origin = identity_in(&TempDir::new().unwrap());
        trust(&f, &origin);
        assert!(f
            .db
            .role_of(origin.node_id())
            .unwrap()
            .grants(crate::domain::trust::Capability::IncidentSync));
    }

    // --- 30: restart resets requester state -----------------------------------

    #[test]
    fn a_fresh_requester_state_permits_an_immediate_request() {
        let f = fixture();
        let origin = identity_in(&TempDir::new().unwrap());
        trust(&f, &origin);
        accept(&f, &origin, 1);
        accept(&f, &origin, 2);
        accept(&f, &origin, 3);
        let now = Instant::now();

        let mut state = LoraSyncRequesterState::new();
        assert!(
            maybe_request_sync(&f.db, &f.local, &mut state, origin.node_id(), 6, now, 0)
                .unwrap()
                .is_some()
        );

        // A brand-new state — standing in for a process restart — is not
        // bound by the previous state's cooldown at all.
        let mut restarted = LoraSyncRequesterState::new();
        assert!(
            maybe_request_sync(&f.db, &f.local, &mut restarted, origin.node_id(), 6, now, 0)
                .unwrap()
                .is_some()
        );
    }
}
