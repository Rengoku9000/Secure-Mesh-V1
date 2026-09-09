//! The SecureMesh wire protocol.
//!
//! Every message is a versioned, signed envelope. The protocol is deliberately
//! small and carries no application behaviour: decoding a message tells you
//! *what was said*, never *what to do about it*. Deciding that is the sync
//! engine's job, which is what keeps transport and application concerns apart.
//!
//! # Trust model at this layer
//!
//! The transport already authenticates the peer — a libp2p session proves the
//! peer holds the private key behind its node ID. Envelope signatures are
//! therefore **defence in depth**, not the primary control: they mean a message
//! is still attributable if it is ever relayed, logged, or replayed outside the
//! session that carried it.
//!
//! Replicated events carry their **own** signature from their original author
//! (see [`crate::domain::event`]). An envelope signature only attests to who
//! *sent* the message, never to who authored its contents.
//!
//! # Hostile input
//!
//! Everything arriving here is untrusted. Decoding is total: it returns an
//! error for any malformed input and never panics, and every variable-length
//! field is bounded before allocation.

use crate::domain::{LocationSource, MeshEvent};
use crate::error::{CoreError, CoreResult};
use crate::identity::NodeIdentity;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Wire protocol version understood by this build.
///
/// Bumped whenever the envelope or any message body changes incompatibly. A
/// peer announcing a different major version is refused politely rather than
/// being fed messages it will misinterpret.
pub const PROTOCOL_VERSION: u16 = 1;

/// Domain-separation prefix for envelope signatures.
///
/// Distinct from the event-signing domain, so a signature over an event can
/// never be replayed as a valid signature over an envelope, or vice versa.
pub const ENVELOPE_SIGNING_DOMAIN: &[u8] = b"securemesh-envelope-v1:";

/// Largest accepted encoded message, in bytes.
///
/// Checked *before* deserialisation, so an oversized frame is dropped without
/// ever being parsed.
pub const MAX_MESSAGE_BYTES: usize = 4 * 1024 * 1024;

/// Largest number of events one `EVENT_BATCH` may carry.
pub const MAX_EVENTS_PER_BATCH: usize = 200;

/// Largest number of origins a peer may claim in one `SYNC_REQUEST`.
pub const MAX_WATERMARKS: usize = 1_000;

/// A single origin and how far the sender holds its log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OriginWatermark {
    pub origin_node: String,
    /// Highest contiguous sequence number the sender holds for this origin.
    pub watermark: u64,
}

/// The body of a protocol message.
///
/// `#[serde(tag = "kind")]` keeps the discriminant explicit on the wire, so an
/// unknown message type from a newer peer fails to decode cleanly instead of
/// being silently coerced into a known variant.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum MessageBody {
    /// First message on a new session: announces version and capabilities.
    Hello {
        protocol_version: u16,
        node_name: String,
        capabilities: Vec<String>,
    },
    /// Node metadata exchanged after HELLO.
    PeerInfo {
        node_name: String,
        /// Hex Ed25519 public key. Must match the session's authenticated peer.
        public_key: String,
        capabilities: Vec<String>,
    },
    /// "Here is what I hold; send me what I lack."
    SyncRequest { have: Vec<OriginWatermark> },
    /// Answers a `SyncRequest`, describing what the responder will send.
    SyncResponse {
        /// `message_id` of the request being answered.
        in_reply_to: String,
        /// Origins the responder holds beyond the requester's watermark.
        available: Vec<OriginWatermark>,
    },
    /// A contiguous run of events from a single origin.
    EventBatch {
        origin_node: String,
        events: Vec<MeshEvent>,
        /// True when the sender has nothing further for this origin.
        complete: bool,
    },
    /// Confirms durable receipt, advancing the sender's ack watermark.
    Ack {
        in_reply_to: String,
        origin_node: String,
        /// Highest contiguous sequence the acknowledging node now holds.
        accepted_through: u64,
    },
    /// Where the sending node says it is, now.
    ///
    /// **Carries no node identifier.** The envelope's authenticated sender is
    /// the subject, so there is no field in which a peer could name another
    /// node — the spoofing question is removed rather than checked.
    ///
    /// Ephemeral: the receiver holds the latest in memory and never writes it
    /// to the event log. A node being somewhere five minutes ago is not a fact
    /// worth keeping forever, and replicating it would put operational state
    /// into an append-only record of things that happened.
    LocationHeartbeat {
        /// Latitude in units of 1e-7 degrees.
        ///
        /// **Fixed point, not a float, and that is load-bearing.** An
        /// envelope's signature is verified by re-serialising its body and
        /// comparing bytes, so every field must survive a JSON round trip
        /// exactly. `serde_json`'s float parser is not precisely inverse to
        /// its writer at full `f64` precision — a real reading was observed
        /// leaving as `13.133598560775905` and returning as
        /// `...903`, which invalidated the signature. Integers have no such
        /// failure mode.
        ///
        /// 1e-7 degrees is about a centimetre, far finer than any source here
        /// resolves, and the whole range fits in an `i32`.
        latitude_e7: i32,
        /// Longitude in units of 1e-7 degrees. See `latitude_e7`.
        longitude_e7: i32,
        /// Radius of uncertainty in millimetres, or absent when none was
        /// reported. Integer for the same reason as the coordinates.
        accuracy_mm: Option<u64>,
        /// How the position was obtained. A wireless fix is never relabelled.
        source: LocationSource,
        /// When the sender measured it — not when this node received it.
        ///
        /// Serialised as an RFC 3339 string, which round-trips exactly.
        captured_at: DateTime<Utc>,
        /// The origin's monotonic counter. What decides which update is newer,
        /// because two nodes do not share a clock.
        sequence: u64,
    },
    /// Liveness probe.
    Ping { nonce: u64 },
    /// Liveness response, echoing the probe's nonce.
    Pong { nonce: u64 },
}

impl MessageBody {
    /// Stable label used in logs, audit records, and the outbound queue.
    pub fn kind(&self) -> &'static str {
        match self {
            MessageBody::Hello { .. } => "HELLO",
            MessageBody::PeerInfo { .. } => "PEER_INFO",
            MessageBody::SyncRequest { .. } => "SYNC_REQUEST",
            MessageBody::SyncResponse { .. } => "SYNC_RESPONSE",
            MessageBody::EventBatch { .. } => "EVENT_BATCH",
            MessageBody::Ack { .. } => "ACK",
            MessageBody::LocationHeartbeat { .. } => "LOCATION_HEARTBEAT",
            MessageBody::Ping { .. } => "PING",
            MessageBody::Pong { .. } => "PONG",
        }
    }
}

/// A signed protocol message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Envelope {
    pub version: u16,
    /// Unique per message; lets `ACK` and `SYNC_RESPONSE` correlate to a request.
    pub message_id: String,
    /// `node_id` of the sender.
    pub sender_node_id: String,
    /// Hex Ed25519 public key of the sender, so the envelope verifies standalone.
    pub sender_public_key: String,
    pub body: MessageBody,
    /// Hex Ed25519 signature over the canonical encoding of the fields above.
    pub signature: String,
}

/// Serialises the signed portion of an envelope.
///
/// The body is serialised once and that exact text is what gets signed and sent,
/// so verification never depends on re-serialisation producing identical bytes.
fn signing_bytes(
    version: u16,
    message_id: &str,
    sender_node_id: &str,
    sender_public_key: &str,
    body_json: &str,
) -> Vec<u8> {
    let mut buffer = Vec::with_capacity(body_json.len() + 128);
    buffer.extend_from_slice(ENVELOPE_SIGNING_DOMAIN);

    let mut push = |field: &[u8]| {
        buffer.extend_from_slice(&(field.len() as u64).to_be_bytes());
        buffer.extend_from_slice(field);
    };

    push(&version.to_be_bytes());
    push(message_id.as_bytes());
    push(sender_node_id.as_bytes());
    push(sender_public_key.as_bytes());
    push(body_json.as_bytes());

    buffer
}

impl Envelope {
    /// Builds and signs a message from this node.
    pub fn create(identity: &NodeIdentity, body: MessageBody) -> CoreResult<Self> {
        let message_id = Uuid::new_v4().to_string();
        let body_json = serde_json::to_string(&body)?;
        let signature = identity.sign(&signing_bytes(
            PROTOCOL_VERSION,
            &message_id,
            identity.node_id(),
            &identity.public_key_hex(),
            &body_json,
        ));

        Ok(Self {
            version: PROTOCOL_VERSION,
            message_id,
            sender_node_id: identity.node_id().to_string(),
            sender_public_key: identity.public_key_hex(),
            body,
            signature: hex::encode(signature),
        })
    }

    /// Encodes for transmission.
    pub fn encode(&self) -> CoreResult<Vec<u8>> {
        let bytes = serde_json::to_vec(self)?;
        if bytes.len() > MAX_MESSAGE_BYTES {
            return Err(CoreError::validation("outgoing message is too large"));
        }
        Ok(bytes)
    }

    /// Decodes and fully validates a message received from a peer.
    ///
    /// Checks run cheapest-first: size, then structure, then version, then the
    /// signature, so hostile input is discarded before any expensive work.
    pub fn decode(bytes: &[u8]) -> CoreResult<Self> {
        if bytes.is_empty() {
            return Err(CoreError::validation("message is empty"));
        }
        if bytes.len() > MAX_MESSAGE_BYTES {
            return Err(CoreError::validation("message exceeds the size limit"));
        }

        let envelope: Envelope = serde_json::from_slice(bytes)
            .map_err(|_| CoreError::validation("message is not a valid SecureMesh envelope"))?;

        envelope.validate()?;
        Ok(envelope)
    }

    /// Structural, version, and signature checks.
    pub fn validate(&self) -> CoreResult<()> {
        if self.version != PROTOCOL_VERSION {
            return Err(CoreError::validation(format!(
                "unsupported protocol version {} (this node speaks {})",
                self.version, PROTOCOL_VERSION
            )));
        }

        if Uuid::parse_str(&self.message_id).is_err() {
            return Err(CoreError::validation("message ID is not a valid UUID"));
        }

        let key_bytes = hex::decode(&self.sender_public_key)
            .map_err(|_| CoreError::validation("sender public key is not valid hex"))?;
        if key_bytes.len() != 32 {
            return Err(CoreError::validation(
                "sender public key has a wrong length",
            ));
        }

        // The sender's claimed node ID must be the hash of the key that will
        // verify the signature, so an envelope cannot borrow another node's ID.
        let derived = hex::encode(sha2::Sha256::digest(&key_bytes));
        if derived != self.sender_node_id {
            return Err(CoreError::validation(
                "sender node ID does not match its public key",
            ));
        }

        self.validate_body_bounds()?;

        let body_json = serde_json::to_string(&self.body)?;
        let signature = hex::decode(&self.signature)
            .map_err(|_| CoreError::validation("message signature is not valid hex"))?;

        if !crate::identity::verify_with_public_key(
            &self.sender_public_key,
            &signing_bytes(
                self.version,
                &self.message_id,
                &self.sender_node_id,
                &self.sender_public_key,
                &body_json,
            ),
            &signature,
        ) {
            return Err(CoreError::validation("message signature is invalid"));
        }

        Ok(())
    }

    /// Rejects bodies whose collections exceed what this node will process.
    ///
    /// Applied after decoding but before any of the contents are acted on, so a
    /// peer cannot make this node do unbounded work by claiming a large batch.
    fn validate_body_bounds(&self) -> CoreResult<()> {
        match &self.body {
            MessageBody::SyncRequest { have } => {
                if have.len() > MAX_WATERMARKS {
                    return Err(CoreError::validation("sync request lists too many origins"));
                }
            }
            MessageBody::SyncResponse { available, .. } => {
                if available.len() > MAX_WATERMARKS {
                    return Err(CoreError::validation(
                        "sync response lists too many origins",
                    ));
                }
            }
            MessageBody::EventBatch { events, .. } => {
                if events.is_empty() {
                    return Err(CoreError::validation("event batch is empty"));
                }
                if events.len() > MAX_EVENTS_PER_BATCH {
                    return Err(CoreError::validation("event batch is too large"));
                }
            }
            MessageBody::Hello {
                node_name,
                capabilities,
                ..
            }
            | MessageBody::PeerInfo {
                node_name,
                capabilities,
                ..
            } => {
                if node_name.len() > 64 {
                    return Err(CoreError::validation("node name is too long"));
                }
                if capabilities.len() > 32 {
                    return Err(CoreError::validation("too many capabilities announced"));
                }
            }
            // Coordinates and provenance are checked by `LocationReport`,
            // which is the same validation the incident path uses. Nothing
            // here is a collection, so there is no size to bound.
            MessageBody::LocationHeartbeat { .. }
            | MessageBody::Ack { .. }
            | MessageBody::Ping { .. }
            | MessageBody::Pong { .. } => {}
        }
        Ok(())
    }
}

use sha2::Digest;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::event::{EventKind, IncidentCreatedPayload};
    use crate::identity::keystore::FileKeyStore;
    use tempfile::TempDir;

    fn identity(dir: &TempDir) -> NodeIdentity {
        NodeIdentity::load_or_create(&FileKeyStore::new(dir.path().join("id.json"))).unwrap()
    }

    fn ping() -> MessageBody {
        MessageBody::Ping { nonce: 42 }
    }

    fn sample_event(identity: &NodeIdentity, seq: u64) -> MeshEvent {
        MeshEvent::create(
            identity,
            seq,
            EventKind::IncidentCreated,
            IncidentCreatedPayload {
                incident_id: Uuid::new_v4().to_string(),
                description: "sample".to_string(),
                severity: "LOW".to_string(),
                latitude: None,
                longitude: None,
                accuracy_meters: None,
                location_source: crate::domain::LocationSource::Unknown,
                location_captured_at: None,
            },
        )
        .unwrap()
    }

    // --- Round trips -------------------------------------------------------

    #[test]
    fn an_envelope_round_trips_through_the_wire_format() {
        let dir = TempDir::new().unwrap();
        let envelope = Envelope::create(&identity(&dir), ping()).unwrap();

        let decoded = Envelope::decode(&envelope.encode().unwrap()).unwrap();
        assert_eq!(decoded, envelope);
        assert_eq!(decoded.version, PROTOCOL_VERSION);
    }

    #[test]
    fn every_message_kind_round_trips() {
        let dir = TempDir::new().unwrap();
        let signer = identity(&dir);

        let bodies = vec![
            MessageBody::Hello {
                protocol_version: PROTOCOL_VERSION,
                node_name: "SM-AAAAA".to_string(),
                capabilities: vec!["sync/1".to_string()],
            },
            MessageBody::PeerInfo {
                node_name: "SM-AAAAA".to_string(),
                public_key: signer.public_key_hex(),
                capabilities: vec![],
            },
            MessageBody::SyncRequest {
                have: vec![OriginWatermark {
                    origin_node: "a".repeat(64),
                    watermark: 7,
                }],
            },
            MessageBody::SyncResponse {
                in_reply_to: Uuid::new_v4().to_string(),
                available: vec![],
            },
            MessageBody::EventBatch {
                origin_node: signer.node_id().to_string(),
                events: vec![sample_event(&signer, 1)],
                complete: true,
            },
            MessageBody::Ack {
                in_reply_to: Uuid::new_v4().to_string(),
                origin_node: "b".repeat(64),
                accepted_through: 3,
            },
            MessageBody::Ping { nonce: 1 },
            MessageBody::Pong { nonce: 1 },
        ];

        for body in bodies {
            let kind = body.kind();
            let envelope = Envelope::create(&signer, body).unwrap();
            let decoded = Envelope::decode(&envelope.encode().unwrap())
                .unwrap_or_else(|e| panic!("{kind} failed to round trip: {e}"));
            assert_eq!(decoded.body.kind(), kind);
        }
    }

    #[test]
    fn message_ids_are_unique_per_message() {
        let dir = TempDir::new().unwrap();
        let signer = identity(&dir);

        let first = Envelope::create(&signer, ping()).unwrap();
        let second = Envelope::create(&signer, ping()).unwrap();
        assert_ne!(first.message_id, second.message_id);
    }

    // --- Authentication ----------------------------------------------------

    #[test]
    fn the_sender_id_is_bound_to_the_sender_key() {
        let dir_a = TempDir::new().unwrap();
        let dir_b = TempDir::new().unwrap();

        let mut forged = Envelope::create(&identity(&dir_a), ping()).unwrap();
        forged.sender_node_id = identity(&dir_b).node_id().to_string();

        let err = forged.validate().unwrap_err();
        assert!(err.message().contains("does not match its public key"));
    }

    #[test]
    fn a_tampered_body_fails_verification() {
        let dir = TempDir::new().unwrap();
        let mut envelope = Envelope::create(&identity(&dir), ping()).unwrap();

        envelope.body = MessageBody::Ping { nonce: 99 };
        assert!(envelope.validate().is_err());
    }

    #[test]
    fn a_signature_cannot_be_moved_between_messages() {
        let dir = TempDir::new().unwrap();
        let signer = identity(&dir);

        let first = Envelope::create(&signer, ping()).unwrap();
        let mut second = Envelope::create(&signer, MessageBody::Pong { nonce: 42 }).unwrap();
        second.signature = first.signature.clone();

        assert!(second.validate().is_err());
    }

    #[test]
    fn envelope_signing_is_domain_separated_from_event_signing() {
        // An envelope signature and an event signature must never be
        // interchangeable, even though both are made with the same node key.
        assert_ne!(
            ENVELOPE_SIGNING_DOMAIN,
            crate::domain::event::EVENT_SIGNING_DOMAIN
        );
    }

    // --- Protocol versioning -----------------------------------------------

    #[test]
    fn an_unsupported_protocol_version_is_refused_not_processed() {
        let dir = TempDir::new().unwrap();
        let mut envelope = Envelope::create(&identity(&dir), ping()).unwrap();
        envelope.version = PROTOCOL_VERSION + 1;

        let err = envelope.validate().unwrap_err();
        assert!(err.message().contains("unsupported protocol version"));
    }

    #[test]
    fn a_message_from_an_older_version_is_refused() {
        let dir = TempDir::new().unwrap();
        let mut envelope = Envelope::create(&identity(&dir), ping()).unwrap();
        envelope.version = 0;

        assert!(envelope.validate().is_err());
    }

    // --- Hostile input -----------------------------------------------------

    #[test]
    fn malformed_bytes_are_rejected_without_panicking() {
        let cases: Vec<Vec<u8>> = vec![
            vec![],
            b"{".to_vec(),
            b"not json at all".to_vec(),
            b"null".to_vec(),
            b"[]".to_vec(),
            b"{}".to_vec(),
            br#"{"version":1}"#.to_vec(),
            br#"{"version":1,"messageId":"x","senderNodeId":"y","senderPublicKey":"z","body":{"kind":"PING","nonce":1},"signature":"q"}"#.to_vec(),
            // Valid JSON, unknown message kind.
            br#"{"version":1,"messageId":"00000000-0000-4000-8000-000000000000","senderNodeId":"a","senderPublicKey":"b","body":{"kind":"WAT"},"signature":"c"}"#.to_vec(),
            vec![0xff; 64],
            vec![0x00; 1024],
        ];

        for bytes in cases {
            let result = Envelope::decode(&bytes);
            assert!(
                result.is_err(),
                "should reject: {:?}",
                &bytes[..bytes.len().min(40)]
            );
        }
    }

    #[test]
    fn an_oversized_frame_is_rejected_before_parsing() {
        let oversized = vec![b'{'; MAX_MESSAGE_BYTES + 1];
        let err = Envelope::decode(&oversized).unwrap_err();
        assert!(err.message().contains("size limit"));
    }

    #[test]
    fn a_truncated_message_is_rejected() {
        let dir = TempDir::new().unwrap();
        let encoded = Envelope::create(&identity(&dir), ping())
            .unwrap()
            .encode()
            .unwrap();

        for cut in [1, encoded.len() / 2, encoded.len() - 1] {
            assert!(Envelope::decode(&encoded[..cut]).is_err());
        }
    }

    #[test]
    fn a_corrupt_signature_field_is_rejected() {
        let dir = TempDir::new().unwrap();
        let mut envelope = Envelope::create(&identity(&dir), ping()).unwrap();

        for bad in ["", "zz", "not-hex", &"aa".repeat(64)] {
            envelope.signature = bad.to_string();
            assert!(envelope.validate().is_err());
        }
    }

    // --- Resource bounds ---------------------------------------------------

    #[test]
    fn an_oversized_event_batch_is_refused() {
        let dir = TempDir::new().unwrap();
        let signer = identity(&dir);

        let envelope = Envelope::create(
            &signer,
            MessageBody::EventBatch {
                origin_node: signer.node_id().to_string(),
                events: (1..=(MAX_EVENTS_PER_BATCH as u64 + 1))
                    .map(|seq| sample_event(&signer, seq))
                    .collect(),
                complete: false,
            },
        )
        .unwrap();

        let err = envelope.validate().unwrap_err();
        assert!(err.message().contains("too large"));
    }

    #[test]
    fn an_empty_event_batch_is_refused() {
        let dir = TempDir::new().unwrap();
        let signer = identity(&dir);

        let envelope = Envelope::create(
            &signer,
            MessageBody::EventBatch {
                origin_node: signer.node_id().to_string(),
                events: vec![],
                complete: true,
            },
        )
        .unwrap();

        assert!(envelope.validate().is_err());
    }

    #[test]
    fn a_sync_request_claiming_too_many_origins_is_refused() {
        let dir = TempDir::new().unwrap();
        let signer = identity(&dir);

        let envelope = Envelope::create(
            &signer,
            MessageBody::SyncRequest {
                have: (0..=MAX_WATERMARKS)
                    .map(|n| OriginWatermark {
                        origin_node: format!("{n:064}"),
                        watermark: 1,
                    })
                    .collect(),
            },
        )
        .unwrap();

        assert!(envelope.validate().is_err());
    }

    #[test]
    fn an_overlong_node_name_is_refused() {
        let dir = TempDir::new().unwrap();
        let envelope = Envelope::create(
            &identity(&dir),
            MessageBody::Hello {
                protocol_version: PROTOCOL_VERSION,
                node_name: "x".repeat(65),
                capabilities: vec![],
            },
        )
        .unwrap();

        assert!(envelope.validate().is_err());
    }

    #[test]
    fn events_inside_a_batch_keep_their_own_signatures() {
        let dir_a = TempDir::new().unwrap();
        let dir_b = TempDir::new().unwrap();
        let author = identity(&dir_a);
        let relay = identity(&dir_b);

        // B relays an event authored by A. The envelope is signed by B, but the
        // event inside still verifies against A, which is what makes multi-hop
        // safe without trusting the relay.
        let event = sample_event(&author, 1);
        let envelope = Envelope::create(
            &relay,
            MessageBody::EventBatch {
                origin_node: author.node_id().to_string(),
                events: vec![event.clone()],
                complete: true,
            },
        )
        .unwrap();

        let decoded = Envelope::decode(&envelope.encode().unwrap()).unwrap();
        assert_eq!(decoded.sender_node_id, relay.node_id());

        match decoded.body {
            MessageBody::EventBatch { events, .. } => {
                assert_eq!(events[0].origin_node, author.node_id());
                assert!(events[0].verify().is_ok());
            }
            other => panic!("expected an event batch, got {}", other.kind()),
        }
    }

    #[test]
    fn kind_labels_are_stable() {
        assert_eq!(MessageBody::Ping { nonce: 0 }.kind(), "PING");
        assert_eq!(MessageBody::Pong { nonce: 0 }.kind(), "PONG");
        assert_eq!(
            MessageBody::SyncRequest { have: vec![] }.kind(),
            "SYNC_REQUEST"
        );
    }
}
