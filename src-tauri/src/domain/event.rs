//! The replicated event log.
//!
//! Everything a SecureMesh node replicates is an **immutable, signed event**.
//! Incidents are not synchronised directly; they are a *projection* of the
//! events that created them. Local creation and remote receipt run the same
//! apply path, which is what makes replication idempotent and restart-safe.
//!
//! # Ordering model
//!
//! Each node stamps its own events with a **monotonic sequence number**
//! starting at 1. A node's knowledge of a peer is therefore a single integer:
//! "I hold every event from node X up to sequence N". The set of these
//! integers across all origins is a version vector in its minimal form — it is
//! worth naming that honestly rather than presenting it as something new.
//!
//! ```text
//!   origin A: 1 2 3 4 5          watermark(A) = 5
//!   origin B: 1 2 _ 4            watermark(B) = 2   (3 is missing; 4 is held
//!                                                    but does not count)
//! ```
//!
//! The watermark only advances over a **contiguous** run, so a gap is
//! self-healing: the next sync request asks from the gap onward, and an event
//! received out of order is stored immediately and folded in when the hole
//! fills.
//!
//! **Wall-clock time is never used for ordering.** `created_at` is recorded for
//! human display only. Field devices without GNSS have unreliable clocks, and a
//! design that sorted by timestamp would reorder history whenever a clock
//! stepped.
//!
//! # Why not a CRDT, Lamport clocks, or causal parents
//!
//! Phase 2 events are append-only and immutable, so merging two nodes' logs is
//! **set union**. Union is associative, commutative and idempotent on its own —
//! it needs no causality metadata to converge. Lamport timestamps or causal
//! parent references would buy a happens-before ordering that the incident
//! domain does not currently need, at the cost of metadata on every record.
//!
//! The tradeoff is explicit: **this model is correct only while events are
//! immutable.** Introducing unrestricted mutable editing later would require
//! revisiting it, and that is recorded in `docs/architecture/ARCHITECTURE.md`.
//!
//! # Self-contained verification
//!
//! An event carries its origin's public key. A receiver can therefore verify a
//! relayed event without having ever met its author:
//!
//! 1. `origin_node == SHA-256(origin_public_key)` — the claimed identity must
//!    match the embedded key, so a forged origin is detectable;
//! 2. the signature verifies against that key.
//!
//! This is what makes multi-hop safe: B relaying A's event does not require C
//! to trust B, only to check A's signature.

use crate::error::{CoreError, CoreResult};
use crate::identity::NodeIdentity;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;
use std::str::FromStr;
use uuid::Uuid;

/// Domain-separation prefix for every application-level signature.
///
/// The node's Ed25519 key is also used as its libp2p identity key, which signs
/// transport handshake material. Prefixing application signatures with a
/// distinct, protocol-specific string ensures a signature produced in one
/// context can never be replayed as a valid signature in the other.
pub const EVENT_SIGNING_DOMAIN: &[u8] = b"securemesh-event-v1:";

/// Largest accepted serialised payload, in bytes.
///
/// Bounds both the database row and the sync batch, so a hostile peer cannot
/// force unbounded allocation with a single event.
pub const MAX_PAYLOAD_BYTES: usize = 64 * 1024;

/// What an event asserts.
///
/// Phase 2 is deliberately append-only: an event either brings an incident into
/// existence or appends an observation to one. Nothing mutates or deletes, so
/// no two events can contradict each other and there is no merge function to
/// get wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum EventKind {
    /// Brings an incident into existence. Immutable once created.
    IncidentCreated,
    /// Appends an observation to an existing incident.
    IncidentObservation,
}

impl EventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            EventKind::IncidentCreated => "INCIDENT_CREATED",
            EventKind::IncidentObservation => "INCIDENT_OBSERVATION",
        }
    }
}

impl fmt::Display for EventKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for EventKind {
    type Err = CoreError;

    fn from_str(value: &str) -> CoreResult<Self> {
        match value {
            "INCIDENT_CREATED" => Ok(EventKind::IncidentCreated),
            "INCIDENT_OBSERVATION" => Ok(EventKind::IncidentObservation),
            other => Err(CoreError::validation(format!(
                "unsupported event kind: {other}"
            ))),
        }
    }
}

/// The body of an `INCIDENT_CREATED` event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IncidentCreatedPayload {
    pub incident_id: String,
    pub description: String,
    pub severity: String,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
}

/// The body of an `INCIDENT_OBSERVATION` event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IncidentObservationPayload {
    pub observation_id: String,
    pub incident_id: String,
    pub note: String,
}

/// An event before it has been signed.
#[derive(Debug, Clone)]
pub struct UnsignedEvent {
    pub event_id: String,
    pub origin_node: String,
    pub origin_public_key: String,
    pub origin_seq: u64,
    pub kind: EventKind,
    /// The payload exactly as serialised by the origin.
    pub payload: String,
    pub created_at: DateTime<Utc>,
}

/// A signed, replicable event.
///
/// The `payload` is kept as the **origin's own serialisation**, never
/// re-encoded. Verifying a signature requires hashing exactly the bytes the
/// author signed; deserialising and re-serialising would risk a different byte
/// sequence — from field reordering or number formatting — and break
/// verification for reasons unrelated to authenticity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshEvent {
    /// Globally unique. Two nodes cannot mint the same ID while partitioned.
    pub event_id: String,
    /// `node_id` of the authoring node.
    pub origin_node: String,
    /// Hex-encoded Ed25519 public key of the author, so the event verifies
    /// standalone.
    pub origin_public_key: String,
    /// Per-origin monotonic counter, starting at 1.
    pub origin_seq: u64,
    pub kind: EventKind,
    /// Serialised payload, byte-identical to what the origin signed.
    pub payload: String,
    /// Author's wall clock. **Display only — never used for ordering.**
    pub created_at: DateTime<Utc>,
    /// Hex-encoded Ed25519 signature over [`canonical_bytes`].
    pub signature: String,
}

/// Builds the exact byte sequence that gets signed.
///
/// Every variable-length field is length-prefixed, so no combination of field
/// contents can be re-parsed as a different event. Concatenating the fields
/// without lengths would let `origin_node="ab", event_id="c"` and
/// `origin_node="a", event_id="bc"` produce identical bytes, making one
/// signature valid for two distinct events.
fn canonical_bytes(
    event_id: &str,
    origin_node: &str,
    origin_public_key: &str,
    origin_seq: u64,
    kind: EventKind,
    payload: &str,
    created_at: &str,
) -> Vec<u8> {
    let mut buffer = Vec::with_capacity(payload.len() + 256);
    buffer.extend_from_slice(EVENT_SIGNING_DOMAIN);

    let mut push = |field: &[u8]| {
        buffer.extend_from_slice(&(field.len() as u64).to_be_bytes());
        buffer.extend_from_slice(field);
    };

    push(event_id.as_bytes());
    push(origin_node.as_bytes());
    push(origin_public_key.as_bytes());
    push(&origin_seq.to_be_bytes());
    push(kind.as_str().as_bytes());
    push(payload.as_bytes());
    push(created_at.as_bytes());

    buffer
}

/// Renders a timestamp in the single form used for both storage and signing.
fn timestamp_text(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

impl UnsignedEvent {
    /// Bytes the origin must sign.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        canonical_bytes(
            &self.event_id,
            &self.origin_node,
            &self.origin_public_key,
            self.origin_seq,
            self.kind,
            &self.payload,
            &timestamp_text(self.created_at),
        )
    }
}

impl MeshEvent {
    /// Creates and signs an event authored by this node.
    ///
    /// `origin_seq` must come from the storage layer, which allocates it under
    /// the same transaction that stores the event so a crash cannot leave a gap
    /// or a reused number.
    pub fn create(
        identity: &NodeIdentity,
        origin_seq: u64,
        kind: EventKind,
        payload: impl Serialize,
    ) -> CoreResult<Self> {
        let payload = serde_json::to_string(&payload)?;
        if payload.len() > MAX_PAYLOAD_BYTES {
            return Err(CoreError::validation("event payload is too large"));
        }

        let unsigned = UnsignedEvent {
            event_id: Uuid::new_v4().to_string(),
            origin_node: identity.node_id().to_string(),
            origin_public_key: identity.public_key_hex(),
            origin_seq,
            kind,
            payload,
            created_at: super::now(),
        };

        let signature = identity.sign(&unsigned.canonical_bytes());

        Ok(Self {
            event_id: unsigned.event_id,
            origin_node: unsigned.origin_node,
            origin_public_key: unsigned.origin_public_key,
            origin_seq: unsigned.origin_seq,
            kind: unsigned.kind,
            payload: unsigned.payload,
            created_at: unsigned.created_at,
            signature: hex::encode(signature),
        })
    }

    /// Bytes this event's signature covers.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        canonical_bytes(
            &self.event_id,
            &self.origin_node,
            &self.origin_public_key,
            self.origin_seq,
            self.kind,
            &self.payload,
            &timestamp_text(self.created_at),
        )
    }

    /// SHA-256 over the signed bytes.
    ///
    /// Used to detect **equivocation**: two events sharing an
    /// `(origin_node, origin_seq)` pair but differing here means the origin
    /// forked its own log.
    pub fn content_hash(&self) -> String {
        hex::encode(Sha256::digest(self.canonical_bytes()))
    }

    /// Fully validates an event received from the network.
    ///
    /// Every input is hostile until this returns `Ok`. The checks run cheapest
    /// first so a malformed event is rejected before any signature arithmetic.
    pub fn verify(&self) -> CoreResult<()> {
        if self.origin_seq == 0 {
            return Err(CoreError::validation("event sequence numbers start at 1"));
        }
        if self.origin_seq > i64::MAX as u64 {
            // SQLite stores integers as i64; anything larger cannot round-trip.
            return Err(CoreError::validation(
                "event sequence number is out of range",
            ));
        }
        if self.payload.len() > MAX_PAYLOAD_BYTES {
            return Err(CoreError::validation("event payload is too large"));
        }
        if Uuid::parse_str(&self.event_id).is_err() {
            return Err(CoreError::validation("event ID is not a valid UUID"));
        }

        let key_bytes = hex::decode(&self.origin_public_key)
            .map_err(|_| CoreError::validation("origin public key is not valid hex"))?;
        if key_bytes.len() != 32 {
            return Err(CoreError::validation(
                "origin public key has a wrong length",
            ));
        }

        // The claimed identity must match the key that will verify the
        // signature, otherwise an attacker could attach someone else's node ID
        // to their own key.
        let derived_node_id = hex::encode(Sha256::digest(&key_bytes));
        if derived_node_id != self.origin_node {
            return Err(CoreError::validation(
                "origin node ID does not match its public key",
            ));
        }

        let signature = hex::decode(&self.signature)
            .map_err(|_| CoreError::validation("event signature is not valid hex"))?;

        if !crate::identity::verify_with_public_key(
            &self.origin_public_key,
            &self.canonical_bytes(),
            &signature,
        ) {
            return Err(CoreError::validation("event signature is invalid"));
        }

        Ok(())
    }

    /// Parses the payload of an `INCIDENT_CREATED` event.
    pub fn incident_created_payload(&self) -> CoreResult<IncidentCreatedPayload> {
        if self.kind != EventKind::IncidentCreated {
            return Err(CoreError::validation("event is not an incident creation"));
        }
        Ok(serde_json::from_str(&self.payload)?)
    }

    /// Parses the payload of an `INCIDENT_OBSERVATION` event.
    pub fn incident_observation_payload(&self) -> CoreResult<IncidentObservationPayload> {
        if self.kind != EventKind::IncidentObservation {
            return Err(CoreError::validation(
                "event is not an incident observation",
            ));
        }
        Ok(serde_json::from_str(&self.payload)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::keystore::FileKeyStore;
    use tempfile::TempDir;

    fn identity(dir: &TempDir) -> NodeIdentity {
        NodeIdentity::load_or_create(&FileKeyStore::new(dir.path().join("identity.json"))).unwrap()
    }

    fn payload() -> IncidentCreatedPayload {
        IncidentCreatedPayload {
            incident_id: Uuid::new_v4().to_string(),
            description: "Flooding at the north gate".to_string(),
            severity: "HIGH".to_string(),
            latitude: None,
            longitude: None,
        }
    }

    fn signed_event(dir: &TempDir, seq: u64) -> MeshEvent {
        MeshEvent::create(&identity(dir), seq, EventKind::IncidentCreated, payload()).unwrap()
    }

    #[test]
    fn a_created_event_verifies() {
        let dir = TempDir::new().unwrap();
        let event = signed_event(&dir, 1);

        assert!(event.verify().is_ok());
        assert_eq!(event.origin_seq, 1);
        assert_eq!(event.kind, EventKind::IncidentCreated);
    }

    #[test]
    fn the_origin_node_is_bound_to_the_embedded_public_key() {
        let dir = TempDir::new().unwrap();
        let event = signed_event(&dir, 1);

        let key = hex::decode(&event.origin_public_key).unwrap();
        assert_eq!(event.origin_node, hex::encode(Sha256::digest(&key)));
    }

    #[test]
    fn events_from_different_nodes_are_distinguishable() {
        let dir_a = TempDir::new().unwrap();
        let dir_b = TempDir::new().unwrap();

        let a = signed_event(&dir_a, 1);
        let b = signed_event(&dir_b, 1);

        assert_ne!(a.origin_node, b.origin_node);
        assert_ne!(a.event_id, b.event_id);
        assert!(a.verify().is_ok());
        assert!(b.verify().is_ok());
    }

    // --- Tamper detection --------------------------------------------------

    #[test]
    fn a_tampered_payload_fails_verification() {
        let dir = TempDir::new().unwrap();
        let mut event = signed_event(&dir, 1);

        event.payload = event.payload.replace("HIGH", "LOW");
        assert!(event.verify().is_err());
    }

    #[test]
    fn a_tampered_sequence_number_fails_verification() {
        let dir = TempDir::new().unwrap();
        let mut event = signed_event(&dir, 1);

        event.origin_seq = 99;
        assert!(event.verify().is_err());
    }

    #[test]
    fn a_tampered_timestamp_fails_verification() {
        let dir = TempDir::new().unwrap();
        let mut event = signed_event(&dir, 1);

        event.created_at += chrono::Duration::seconds(3600);
        assert!(event.verify().is_err());
    }

    #[test]
    fn claiming_another_nodes_id_is_rejected() {
        let dir_a = TempDir::new().unwrap();
        let dir_b = TempDir::new().unwrap();

        let mut forged = signed_event(&dir_a, 1);
        let victim = identity(&dir_b);

        // Attach the victim's node ID while keeping the attacker's key.
        forged.origin_node = victim.node_id().to_string();

        let err = forged.verify().unwrap_err();
        assert!(err.message().contains("does not match its public key"));
    }

    #[test]
    fn substituting_the_public_key_is_rejected() {
        let dir_a = TempDir::new().unwrap();
        let dir_b = TempDir::new().unwrap();

        let mut forged = signed_event(&dir_a, 1);
        forged.origin_public_key = identity(&dir_b).public_key_hex();

        // Node ID no longer matches the substituted key.
        assert!(forged.verify().is_err());
    }

    #[test]
    fn a_signature_from_another_event_does_not_transfer() {
        let dir = TempDir::new().unwrap();
        let first = signed_event(&dir, 1);
        let mut second = signed_event(&dir, 2);

        second.signature = first.signature.clone();
        assert!(second.verify().is_err());
    }

    // --- Malformed input ---------------------------------------------------

    #[test]
    fn malformed_events_are_rejected_without_panicking() {
        let dir = TempDir::new().unwrap();
        let template = signed_event(&dir, 1);

        let mut zero_seq = template.clone();
        zero_seq.origin_seq = 0;
        assert!(zero_seq.verify().is_err());

        let mut huge_seq = template.clone();
        huge_seq.origin_seq = u64::MAX;
        assert!(huge_seq.verify().is_err());

        let mut bad_key = template.clone();
        bad_key.origin_public_key = "not-hex".to_string();
        assert!(bad_key.verify().is_err());

        let mut short_key = template.clone();
        short_key.origin_public_key = "aabb".to_string();
        assert!(short_key.verify().is_err());

        let mut bad_sig = template.clone();
        bad_sig.signature = "zzzz".to_string();
        assert!(bad_sig.verify().is_err());

        let mut empty_sig = template.clone();
        empty_sig.signature = String::new();
        assert!(empty_sig.verify().is_err());

        let mut bad_id = template.clone();
        bad_id.event_id = "not-a-uuid".to_string();
        assert!(bad_id.verify().is_err());
    }

    #[test]
    fn an_oversized_payload_is_refused_at_creation() {
        let dir = TempDir::new().unwrap();
        let oversized = IncidentCreatedPayload {
            incident_id: Uuid::new_v4().to_string(),
            description: "x".repeat(MAX_PAYLOAD_BYTES + 1),
            severity: "LOW".to_string(),
            latitude: None,
            longitude: None,
        };

        let result = MeshEvent::create(&identity(&dir), 1, EventKind::IncidentCreated, oversized);
        assert!(result.is_err());
    }

    // --- Canonical encoding ------------------------------------------------

    #[test]
    fn length_prefixing_prevents_field_boundary_confusion() {
        // Without length prefixes these two would serialise identically.
        let first = canonical_bytes("ab", "c", "k", 1, EventKind::IncidentCreated, "p", "t");
        let second = canonical_bytes("a", "bc", "k", 1, EventKind::IncidentCreated, "p", "t");
        assert_ne!(first, second);
    }

    #[test]
    fn signing_is_domain_separated() {
        let dir = TempDir::new().unwrap();
        let event = signed_event(&dir, 1);

        assert!(event.canonical_bytes().starts_with(EVENT_SIGNING_DOMAIN));
    }

    #[test]
    fn canonical_bytes_are_stable_across_calls() {
        let dir = TempDir::new().unwrap();
        let event = signed_event(&dir, 1);

        assert_eq!(event.canonical_bytes(), event.canonical_bytes());
        assert_eq!(event.content_hash(), event.content_hash());
    }

    // --- Equivocation ------------------------------------------------------

    #[test]
    fn differing_events_at_the_same_sequence_have_different_hashes() {
        let dir = TempDir::new().unwrap();
        let signer = identity(&dir);

        // The same node signing two different events at sequence 1 is exactly
        // the equivocation the sync engine must detect.
        let first = MeshEvent::create(&signer, 1, EventKind::IncidentCreated, payload()).unwrap();
        let second = MeshEvent::create(&signer, 1, EventKind::IncidentCreated, payload()).unwrap();

        assert_eq!(first.origin_node, second.origin_node);
        assert_eq!(first.origin_seq, second.origin_seq);
        assert_ne!(first.content_hash(), second.content_hash());
        assert!(first.verify().is_ok());
        assert!(second.verify().is_ok());
    }

    // --- Payload access ----------------------------------------------------

    #[test]
    fn payload_round_trips_through_its_typed_form() {
        let dir = TempDir::new().unwrap();
        let original = payload();
        let event = MeshEvent::create(
            &identity(&dir),
            1,
            EventKind::IncidentCreated,
            original.clone(),
        )
        .unwrap();

        assert_eq!(event.incident_created_payload().unwrap(), original);
    }

    #[test]
    fn reading_the_wrong_payload_type_is_an_error_not_a_panic() {
        let dir = TempDir::new().unwrap();
        let event = signed_event(&dir, 1);

        assert!(event.incident_observation_payload().is_err());
    }

    #[test]
    fn event_kind_round_trips_through_its_string_form() {
        for kind in [EventKind::IncidentCreated, EventKind::IncidentObservation] {
            assert_eq!(kind.as_str().parse::<EventKind>().unwrap(), kind);
        }
        assert!("NOT_A_KIND".parse::<EventKind>().is_err());
    }
}
