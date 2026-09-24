//! Accepting a SecureMesh event that arrived over LoRa.
//!
//! ```text
//! LoraFrame            CRC-checked bytes — corruption detection only
//!     ↓
//! trust lookup         source_node_id must already be TRUSTED with
//!                      INCIDENT_SYNC; its public key comes from local state
//!     ↓
//! lora_event_codec     rebuild the exact MeshEvent the origin signed
//!     ↓
//! MeshEvent::verify    the existing Ed25519 check — the authentication step
//!     ↓
//! Database::apply_event  existing duplicate / equivocation handling
//! ```
//!
//! # One trust system
//!
//! This is the same gate the sync engine applies to a QUIC `EVENT_BATCH`
//! (`authorize` then `apply_batch` in `sync/mod.rs`), expressed through the
//! same public storage APIs. There is no LoRa trust store, no LoRa identity,
//! and no LoRa event table: trust was always keyed by node ID, never by the
//! transport a message used, so a LoRa event from a trusted node is judged
//! exactly as a QUIC one would be.
//!
//! # What this deliberately does not do
//!
//! - **Enroll anyone.** An unknown sender is refused and nothing about it is
//!   recorded — no node row, no PENDING state. Enrollment stays an operator
//!   decision made over an authenticated QUIC session.
//! - **Trust a key from the air.** The frame never carries a public key; the
//!   key used for verification is the one this node already holds for that
//!   node ID, and it is re-checked against the ID before use.
//! - **Relay.** The event's origin *is* the frame's sender. An event authored
//!   by a third node fails verification under the sender's key.
//! - **Treat the CRC as authentication.** The Ed25519 signature is the only
//!   thing that establishes who wrote an event.
//!
//! Nothing calls [`ingest_event_frame`] yet in Phase 3A; wiring it into the
//! running node is a later phase.

use super::lora_event_codec;
use super::lora_transport::{LoraFrame, LoraMessageType};
use crate::domain::trust::Capability;
use crate::error::{CoreError, CoreResult};
use crate::identity;
use crate::security::{audit, AuditEvent, AuditOutcome};
use crate::storage::events::ApplyOutcome;
use crate::storage::Database;

/// The result of ingesting one LoRa event frame.
///
/// `origin_seq` is carried alongside the storage outcome so a caller — the
/// Phase 6 step 4 historical-sync requester — can reason about a gap in the
/// origin's sequence without re-decoding a frame this function has already
/// verified. It is populated for every outcome, including `Duplicate` and
/// `Conflict`, since the event's own sequence number is known as soon as it
/// decodes, independent of what storage does with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IngestedEvent {
    pub outcome: ApplyOutcome,
    pub origin_seq: u64,
}

/// Authenticates one LoRa event frame and offers it to the event log.
///
/// Returns the existing storage outcome — `Stored`, `Duplicate`, or
/// `Conflict` — so duplicate delivery and equivocation are handled by exactly
/// the code that handles them for QUIC. Any failure before storage leaves the
/// database untouched.
pub fn ingest_event_frame(
    database: &Database,
    local_node_id: &str,
    frame: &LoraFrame,
) -> CoreResult<IngestedEvent> {
    if frame.message_type != LoraMessageType::SecureMeshEvent {
        return Err(CoreError::validation(
            "LoRa frame does not carry a SecureMesh event",
        ));
    }

    let source = hex::encode(frame.source_node_id);
    if source == local_node_id {
        return Err(CoreError::validation(
            "LoRa event frame claims to originate from this node",
        ));
    }

    // Only an identity this node has already authorized may deliver events.
    authorize(database, &source)?;

    // The verifying key comes from local state, never from the frame, and is
    // re-bound to the node ID before use rather than assumed consistent.
    let node = database.get_node(&source)?;
    if !identity::node_id_matches_key(&source, &node.public_key) {
        return Err(CoreError::validation(
            "the stored public key for this LoRa sender does not match its node ID",
        ));
    }

    let event = lora_event_codec::decode(&frame.payload, &frame.source_node_id, &node.public_key)?;

    // The authentication step. Identical to the check a QUIC event receives.
    event.verify()?;

    let outcome = database.apply_event(&event, local_node_id, Some(&source))?;
    eprintln!("{}", outcome_log_line(&source, event.origin_seq, outcome));
    Ok(IngestedEvent {
        outcome,
        origin_seq: event.origin_seq,
    })
}

/// The sync engine's authorization rule, for a LoRa sender.
///
/// `pub(crate)` so [`super::lora_sync_responder`] can gate historical sync
/// with the exact same trust rule a live event frame is held to — one
/// authorization path for both, rather than a second copy of it.
pub(crate) fn authorize(database: &Database, node_id: &str) -> CoreResult<()> {
    let capability = Capability::IncidentSync;

    let state = database.trust_state_of(node_id)?;
    if !state.permits_authorized_operations() {
        audit(
            AuditEvent::AuthorizationDenied,
            AuditOutcome::Failure,
            &format!("transport=lora peer={node_id} state={state} capability={capability}"),
        );
        return Err(CoreError::validation(format!(
            "LoRa sender is {state} and is not authorized for {capability}"
        )));
    }

    let role = database.role_of(node_id)?;
    if !role.grants(capability) {
        audit(
            AuditEvent::AuthorizationDenied,
            AuditOutcome::Failure,
            &format!("transport=lora peer={node_id} role={role} capability={capability}"),
        );
        return Err(CoreError::validation(format!(
            "LoRa sender's role {role} does not grant {capability}"
        )));
    }

    Ok(())
}

/// The single log line written for an accepted frame. Carries public
/// identifiers and counters only.
fn outcome_log_line(source: &str, origin_seq: u64, outcome: ApplyOutcome) -> String {
    let label = match outcome {
        ApplyOutcome::Stored => "stored",
        ApplyOutcome::Duplicate => "duplicate",
        ApplyOutcome::Conflict => "conflict",
    };
    let short = &source[..source.len().min(16)];
    format!("[securemesh] lora event accepted origin={short} seq={origin_seq} outcome={label}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::event::{EventKind, IncidentCreatedPayload, MeshEvent};
    use crate::domain::{LocationSource, TrustState};
    use crate::identity::keystore::FileKeyStore;
    use crate::identity::NodeIdentity;
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

    /// Registers `peer` with its genuine key and approves it.
    fn trust(f: &Fixture, peer: &NodeIdentity) {
        f.db.register_peer(peer.node_id(), &peer.public_key_hex(), None)
            .unwrap();
        f.db.approve_peer(&f.local, peer.node_id(), None).unwrap();
    }

    fn incident(signer: &NodeIdentity, seq: u64, description: &str) -> MeshEvent {
        MeshEvent::create(
            signer,
            seq,
            EventKind::IncidentCreated,
            IncidentCreatedPayload {
                incident_id: Uuid::new_v4().to_string(),
                description: description.to_string(),
                severity: "HIGH".to_string(),
                latitude: None,
                longitude: None,
                accuracy_meters: None,
                location_source: LocationSource::Unknown,
                location_captured_at: None,
            },
        )
        .unwrap()
    }

    /// The frame `sender` would put on the air for `event`.
    fn frame_from(sender: &NodeIdentity, event: &MeshEvent) -> LoraFrame {
        let mut source = [0u8; 32];
        source.copy_from_slice(&hex::decode(sender.node_id()).unwrap());
        LoraFrame {
            message_type: LoraMessageType::SecureMeshEvent,
            source_node_id: source,
            sequence: 0,
            payload: lora_event_codec::encode(event).unwrap(),
        }
    }

    fn ingest(f: &Fixture, frame: &LoraFrame) -> CoreResult<ApplyOutcome> {
        ingest_event_frame(&f.db, f.local.node_id(), frame).map(|r| r.outcome)
    }

    // --- The accepted path --------------------------------------------------

    #[test]
    fn an_event_from_a_trusted_peer_is_verified_and_stored() {
        let f = fixture();
        let peer_dir = TempDir::new().unwrap();
        let peer = identity_in(&peer_dir);
        trust(&f, &peer);

        let event = incident(&peer, 1, "Bridge out on NH44");
        let incident_id = event.incident_created_payload().unwrap().incident_id;

        assert_eq!(
            ingest(&f, &frame_from(&peer, &event)).unwrap(),
            ApplyOutcome::Stored
        );
        assert!(f.db.has_event(&event.event_id).unwrap());
        // Projected through the existing path, exactly as a QUIC event would be.
        assert_eq!(
            f.db.get_incident(&incident_id).unwrap().description,
            "Bridge out on NH44"
        );
    }

    // --- Replay and equivocation stay with storage (tests 10, 11) ---------

    #[test]
    fn a_duplicate_event_follows_existing_storage_behaviour() {
        let f = fixture();
        let peer_dir = TempDir::new().unwrap();
        let peer = identity_in(&peer_dir);
        trust(&f, &peer);

        let frame = frame_from(&peer, &incident(&peer, 1, "dup"));
        assert_eq!(ingest(&f, &frame).unwrap(), ApplyOutcome::Stored);
        assert_eq!(ingest(&f, &frame).unwrap(), ApplyOutcome::Duplicate);
        assert_eq!(f.db.count_events().unwrap(), 1);
    }

    #[test]
    fn origin_sequence_equivocation_follows_existing_storage_behaviour() {
        let f = fixture();
        let peer_dir = TempDir::new().unwrap();
        let peer = identity_in(&peer_dir);
        trust(&f, &peer);

        // Two different events the peer signed at the same sequence number.
        let first = incident(&peer, 1, "first");
        let second = incident(&peer, 1, "second");

        assert_eq!(
            ingest(&f, &frame_from(&peer, &first)).unwrap(),
            ApplyOutcome::Stored
        );
        assert_eq!(
            ingest(&f, &frame_from(&peer, &second)).unwrap(),
            ApplyOutcome::Conflict
        );
        assert_eq!(f.db.count_event_conflicts().unwrap(), 1);
        assert!(!f.db.has_event(&second.event_id).unwrap());
    }

    // --- Trust (test 9) -------------------------------------------------------

    #[test]
    fn an_unknown_sender_is_rejected_and_not_enrolled() {
        let f = fixture();
        let stranger_dir = TempDir::new().unwrap();
        let stranger = identity_in(&stranger_dir);
        let event = incident(&stranger, 1, "unknown");

        let err = ingest(&f, &frame_from(&stranger, &event)).unwrap_err();
        assert!(err.message().contains("UNKNOWN"));

        assert!(!f.db.has_event(&event.event_id).unwrap());
        // No enrollment over LoRa: nothing about the stranger was recorded.
        assert_eq!(
            f.db.trust_state_of(stranger.node_id()).unwrap(),
            TrustState::Unknown
        );
        assert!(f.db.get_node(stranger.node_id()).is_err());
        assert!(f
            .db
            .trust_audit_log(Some(stranger.node_id()), 100)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn a_pending_sender_is_rejected() {
        let f = fixture();
        let peer_dir = TempDir::new().unwrap();
        let peer = identity_in(&peer_dir);
        f.db.register_peer(peer.node_id(), &peer.public_key_hex(), None)
            .unwrap();
        f.db.record_enrollment_request(&f.local, peer.node_id(), "SM-PEER", &[])
            .unwrap();

        let event = incident(&peer, 1, "pending");
        let err = ingest(&f, &frame_from(&peer, &event)).unwrap_err();
        assert!(err.message().contains("PENDING"));
        assert!(!f.db.has_event(&event.event_id).unwrap());
    }

    #[test]
    fn a_revoked_sender_is_rejected() {
        let f = fixture();
        let peer_dir = TempDir::new().unwrap();
        let peer = identity_in(&peer_dir);
        trust(&f, &peer);
        f.db.revoke_peer(&f.local, peer.node_id(), None).unwrap();

        let event = incident(&peer, 1, "revoked");
        let err = ingest(&f, &frame_from(&peer, &event)).unwrap_err();
        assert!(err.message().contains("REVOKED"));
        assert!(!f.db.has_event(&event.event_id).unwrap());
    }

    // --- Key binding (tests 7, 8) --------------------------------------------

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

        let event = incident(&peer, 1, "mismatch");
        let err = ingest(&f, &frame_from(&peer, &event)).unwrap_err();
        assert!(err.message().contains("does not match its node ID"));
        assert!(!f.db.has_event(&event.event_id).unwrap());
    }

    #[test]
    fn an_event_signed_by_a_different_key_is_rejected() {
        // A trusted sender relaying (or forging) an event another node signed:
        // the receiver verifies under the *sender's* key, which fails.
        let f = fixture();
        let peer_dir = TempDir::new().unwrap();
        let author_dir = TempDir::new().unwrap();
        let peer = identity_in(&peer_dir);
        let author = identity_in(&author_dir);
        trust(&f, &peer);

        let event = incident(&author, 1, "relayed");
        let err = ingest(&f, &frame_from(&peer, &event)).unwrap_err();
        assert!(err.message().contains("signature"));
        assert!(!f.db.has_event(&event.event_id).unwrap());
    }

    // --- Tampering in transit (tests 4, 5, 6) --------------------------------

    fn assert_tamper_rejected(offset_from_end: Option<usize>, offset: usize) {
        let f = fixture();
        let peer_dir = TempDir::new().unwrap();
        let peer = identity_in(&peer_dir);
        trust(&f, &peer);

        let event = incident(&peer, 2, "tamper target");
        let mut frame = frame_from(&peer, &event);
        let index = match offset_from_end {
            Some(back) => frame.payload.len() - back,
            None => offset,
        };
        frame.payload[index] ^= 0x01;

        assert!(ingest(&f, &frame).is_err());
        assert_eq!(f.db.count_events().unwrap(), 0);
    }

    #[test]
    fn a_modified_payload_is_rejected_before_storage() {
        assert_tamper_rejected(Some(1), 0); // last description byte
    }

    #[test]
    fn a_modified_origin_seq_is_rejected_before_storage() {
        assert_tamper_rejected(None, 1 + 1 + 16 + 7);
    }

    #[test]
    fn a_modified_event_id_is_rejected_before_storage() {
        assert_tamper_rejected(None, 2);
    }

    // --- Frame-level guards ---------------------------------------------------

    #[test]
    fn a_frame_claiming_to_come_from_this_node_is_rejected() {
        let f = fixture();
        let event = incident(&f.local, 1, "echo");
        let err = ingest(&f, &frame_from(&f.local, &event)).unwrap_err();
        assert!(err.message().contains("this node"));
    }

    #[test]
    fn a_diagnostic_frame_is_not_treated_as_an_event() {
        let f = fixture();
        let peer_dir = TempDir::new().unwrap();
        let peer = identity_in(&peer_dir);
        trust(&f, &peer);

        let mut frame = frame_from(&peer, &incident(&peer, 1, "diag"));
        frame.message_type = LoraMessageType::Diagnostic;
        assert!(ingest(&f, &frame).is_err());
        assert_eq!(f.db.count_events().unwrap(), 0);
    }

    // --- No private key material in what leaves this node (test 15) --------

    #[test]
    fn no_private_key_material_reaches_the_wire_or_the_log() {
        let f = fixture();
        let peer_dir = TempDir::new().unwrap();
        let peer = identity_in(&peer_dir);
        trust(&f, &peer);

        // Read the real secrets straight off disk, as the identity tests do.
        let secret_of = |dir: &TempDir| {
            let raw = std::fs::read_to_string(dir.path().join("id.json")).unwrap();
            let file: serde_json::Value = serde_json::from_str(&raw).unwrap();
            file["secret_key"].as_str().unwrap().to_string()
        };
        let peer_secret = secret_of(&peer_dir);
        let local_secret = secret_of(&f._dir);

        let event = incident(&peer, 1, "secrets");
        let frame = frame_from(&peer, &event);
        let wire = frame.encode().unwrap();
        let outcome = ingest(&f, &frame).unwrap();
        let log_line = outcome_log_line(peer.node_id(), event.origin_seq, outcome);

        for secret_hex in [&peer_secret, &local_secret] {
            let secret_bytes = hex::decode(secret_hex).unwrap();
            assert!(!wire
                .windows(secret_bytes.len())
                .any(|w| w == secret_bytes.as_slice()));
            assert!(!hex::encode(&wire).contains(secret_hex.as_str()));
            assert!(!log_line.contains(secret_hex.as_str()));
        }
        assert!(log_line.contains("outcome=stored"));
    }
}
