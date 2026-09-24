//! Phase 6 historical LoRa sync: the signed `SyncRequest` payload.
//!
//! # Scope
//!
//! Pure codec, signing and verification only. Nothing here reads the
//! database, consults the trust store, selects events, applies replay or rate
//! policy, or transmits anything. The caller supplies the requester's public
//! key from its own trust state — exactly as [`super::lora_event_ingest`]
//! does for an event frame — and decides separately whether that requester
//! is authorized at all.
//!
//! # Wire format (big-endian throughout, exactly 122 bytes)
//!
//! ```text
//! req_version     u8        = 1
//! target_origin   [32]      raw node ID whose events are requested
//! watermark       u64       requester's highest contiguous seq for target
//! have_bitmap     u64       bit i set = requester holds seq watermark + 2 + i
//! max_events      u8        1..=8
//! request_ts      i64       requester's Unix milliseconds
//! signature       [64]      Ed25519 over `signing_bytes`
//! ```
//!
//! Absent from the payload, by design: the requester's node ID (the LoRa
//! frame's `source_node_id` carries it, and it is bound into the signature)
//! and any public key (the verifier takes it from local state, never from the
//! air).
//!
//! # Signed bytes
//!
//! A distinct domain, [`SYNC_REQUEST_SIGNING_DOMAIN`], followed by every field
//! except the signature, each prefixed with its length as a big-endian `u64` —
//! the same construction `protocol.rs` uses for envelopes. The requester's
//! node ID is included, so a signature is bound to the frame source it
//! arrived under and cannot be re-attributed to another node.

use crate::error::{CoreError, CoreResult};
use crate::identity::{self, NodeIdentity};

/// Layout version of this payload.
pub const SYNC_REQUEST_VERSION: u8 = 1;

/// Domain-separation prefix for `SyncRequest` signatures. Distinct from the
/// envelope and event signing domains, so a signature made for one can never
/// verify as another.
pub const SYNC_REQUEST_SIGNING_DOMAIN: &[u8] = b"securemesh-lora-sync-v1:";

/// Largest number of events one request may ask for. Matches the runtime's
/// per-tick LoRa RX cap, so a single answer can never exceed what the
/// receiver will process in one tick.
pub const MAX_SYNC_REQUEST_EVENTS: u8 = 8;

const SIGNATURE_LEN: usize = 64;

/// Exact encoded size of a `SyncRequest` payload.
pub const SYNC_REQUEST_PAYLOAD_BYTES: usize = 1 + 32 + 8 + 8 + 1 + 8 + SIGNATURE_LEN;

const TARGET_OFFSET: usize = 1;
const WATERMARK_OFFSET: usize = TARGET_OFFSET + 32;
const BITMAP_OFFSET: usize = WATERMARK_OFFSET + 8;
const MAX_EVENTS_OFFSET: usize = BITMAP_OFFSET + 8;
const TIMESTAMP_OFFSET: usize = MAX_EVENTS_OFFSET + 1;
const SIGNATURE_OFFSET: usize = TIMESTAMP_OFFSET + 8;

/// The fields a requester signs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyncRequest {
    /// Raw 32 bytes of the node whose events are requested.
    pub target_origin: [u8; 32],
    /// Highest contiguous sequence the requester holds for `target_origin`.
    pub watermark: u64,
    /// Bit `i` set means the requester already holds sequence
    /// `watermark + 2 + i`.
    pub have_bitmap: u64,
    /// How many events the requester will accept, `1..=8`.
    pub max_events: u8,
    /// The requester's clock, in Unix milliseconds. Carried and signed only;
    /// freshness policy belongs to the responder.
    pub request_ts: i64,
}

/// A decoded request with its signature, not yet verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedSyncRequest {
    pub request: SyncRequest,
    pub signature: [u8; 64],
}

/// Signs `request` as `identity` and encodes the 122-byte payload.
pub fn encode(identity: &NodeIdentity, request: &SyncRequest) -> CoreResult<Vec<u8>> {
    let requester = node_id_bytes(identity.node_id())?;
    validate(&requester, request)?;

    let signature = identity.sign(&signing_bytes(&requester, request));

    let mut out = Vec::with_capacity(SYNC_REQUEST_PAYLOAD_BYTES);
    out.push(SYNC_REQUEST_VERSION);
    out.extend_from_slice(&request.target_origin);
    out.extend_from_slice(&request.watermark.to_be_bytes());
    out.extend_from_slice(&request.have_bitmap.to_be_bytes());
    out.push(request.max_events);
    out.extend_from_slice(&request.request_ts.to_be_bytes());
    out.extend_from_slice(&signature);
    debug_assert_eq!(out.len(), SYNC_REQUEST_PAYLOAD_BYTES);
    Ok(out)
}

/// Structurally decodes a payload. Does **not** verify the signature — call
/// [`verify`] (or use [`decode_verified`]) before acting on the result.
pub fn decode(payload: &[u8]) -> CoreResult<SignedSyncRequest> {
    if payload.len() < SYNC_REQUEST_PAYLOAD_BYTES {
        return Err(CoreError::validation("LoRa sync request is truncated"));
    }
    if payload.len() > SYNC_REQUEST_PAYLOAD_BYTES {
        return Err(CoreError::validation(
            "LoRa sync request has trailing bytes",
        ));
    }

    if payload[0] != SYNC_REQUEST_VERSION {
        return Err(CoreError::validation(format!(
            "unsupported LoRa sync request version {}",
            payload[0]
        )));
    }

    let request = SyncRequest {
        target_origin: fixed::<32>(payload, TARGET_OFFSET),
        watermark: u64::from_be_bytes(fixed::<8>(payload, WATERMARK_OFFSET)),
        have_bitmap: u64::from_be_bytes(fixed::<8>(payload, BITMAP_OFFSET)),
        max_events: payload[MAX_EVENTS_OFFSET],
        request_ts: i64::from_be_bytes(fixed::<8>(payload, TIMESTAMP_OFFSET)),
    };
    validate_max_events(request.max_events)?;

    Ok(SignedSyncRequest {
        request,
        signature: fixed::<SIGNATURE_LEN>(payload, SIGNATURE_OFFSET),
    })
}

/// Verifies a decoded request as coming from `requester`.
///
/// `requester` is the LoRa frame's `source_node_id`; `requester_public_key_hex`
/// must come from this node's own trust state. The key is re-bound to the
/// node ID with the standard SecureMesh rule before use.
pub fn verify(
    signed: &SignedSyncRequest,
    requester: &[u8; 32],
    requester_public_key_hex: &str,
) -> CoreResult<()> {
    if !identity::node_id_matches_key(&hex::encode(requester), requester_public_key_hex) {
        return Err(CoreError::validation(
            "the public key supplied for this LoRa sync requester does not match its node ID",
        ));
    }

    validate(requester, &signed.request)?;

    if !identity::verify_with_public_key(
        requester_public_key_hex,
        &signing_bytes(requester, &signed.request),
        &signed.signature,
    ) {
        return Err(CoreError::validation(
            "LoRa sync request signature is invalid",
        ));
    }
    Ok(())
}

/// [`decode`] followed by [`verify`].
pub fn decode_verified(
    payload: &[u8],
    requester: &[u8; 32],
    requester_public_key_hex: &str,
) -> CoreResult<SyncRequest> {
    let signed = decode(payload)?;
    verify(&signed, requester, requester_public_key_hex)?;
    Ok(signed.request)
}

/// The exact bytes a requester signs.
pub fn signing_bytes(requester: &[u8; 32], request: &SyncRequest) -> Vec<u8> {
    let mut buffer = Vec::with_capacity(SYNC_REQUEST_SIGNING_DOMAIN.len() + 7 * 8 + 90);
    buffer.extend_from_slice(SYNC_REQUEST_SIGNING_DOMAIN);

    let mut push = |field: &[u8]| {
        buffer.extend_from_slice(&(field.len() as u64).to_be_bytes());
        buffer.extend_from_slice(field);
    };

    push(&[SYNC_REQUEST_VERSION]);
    push(requester);
    push(&request.target_origin);
    push(&request.watermark.to_be_bytes());
    push(&request.have_bitmap.to_be_bytes());
    push(&[request.max_events]);
    push(&request.request_ts.to_be_bytes());

    buffer
}

fn validate(requester: &[u8; 32], request: &SyncRequest) -> CoreResult<()> {
    validate_max_events(request.max_events)?;
    if &request.target_origin == requester {
        return Err(CoreError::validation(
            "a LoRa sync request cannot target its own requester",
        ));
    }
    Ok(())
}

fn validate_max_events(max_events: u8) -> CoreResult<()> {
    if !(1..=MAX_SYNC_REQUEST_EVENTS).contains(&max_events) {
        return Err(CoreError::validation(format!(
            "LoRa sync request max_events must be 1..={MAX_SYNC_REQUEST_EVENTS}, got {max_events}"
        )));
    }
    Ok(())
}

pub(crate) fn node_id_bytes(node_id: &str) -> CoreResult<[u8; 32]> {
    let decoded =
        hex::decode(node_id).map_err(|_| CoreError::internal("local node id is not valid hex"))?;
    <[u8; 32]>::try_from(decoded.as_slice())
        .map_err(|_| CoreError::internal("local node id does not decode to 32 bytes"))
}

/// Copies a fixed-size field. Callers have already checked the total length.
fn fixed<const N: usize>(payload: &[u8], offset: usize) -> [u8; N] {
    let mut out = [0u8; N];
    out.copy_from_slice(&payload[offset..offset + N]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::keystore::FileKeyStore;
    use tempfile::TempDir;

    fn identity(dir: &TempDir) -> NodeIdentity {
        NodeIdentity::load_or_create(&FileKeyStore::new(dir.path().join("id.json"))).unwrap()
    }

    fn raw_id(identity: &NodeIdentity) -> [u8; 32] {
        node_id_bytes(identity.node_id()).unwrap()
    }

    struct Pair {
        _dirs: (TempDir, TempDir),
        requester: NodeIdentity,
        target: NodeIdentity,
    }

    fn pair() -> Pair {
        let (a, b) = (TempDir::new().unwrap(), TempDir::new().unwrap());
        Pair {
            requester: identity(&a),
            target: identity(&b),
            _dirs: (a, b),
        }
    }

    fn request_for(target: &NodeIdentity) -> SyncRequest {
        SyncRequest {
            target_origin: raw_id(target),
            watermark: 3,
            have_bitmap: 0b1000,
            max_events: 8,
            request_ts: 1_790_000_000_123,
        }
    }

    fn signed_payload(p: &Pair) -> Vec<u8> {
        encode(&p.requester, &request_for(&p.target)).unwrap()
    }

    fn verify_payload(p: &Pair, payload: &[u8]) -> CoreResult<SyncRequest> {
        decode_verified(
            payload,
            &raw_id(&p.requester),
            &p.requester.public_key_hex(),
        )
    }

    /// Builds a `NodeIdentity` from a fixed 32-byte seed rather than a
    /// random one, so a test can reproduce the exact same key (and hence the
    /// exact same signature) on every run and on every platform.
    fn fixed_identity(dir: &TempDir, name: &str, seed_byte: u8) -> NodeIdentity {
        let path = dir.path().join(format!("{name}.json"));
        let secret = hex::encode([seed_byte; 32]);
        std::fs::write(
            &path,
            format!(
                r#"{{"version":1,"algorithm":"ed25519","created_at":"2026-01-01T00:00:00Z","secret_key":"{secret}"}}"#
            ),
        )
        .unwrap();
        NodeIdentity::load_or_create(&FileKeyStore::new(path)).unwrap()
    }

    /// **Cross-platform interoperability guarantee.** This exact input,
    /// built from fixed seeds so it reproduces identically on any machine,
    /// was independently generated and verified on both the Windows and Pi
    /// trees during the Phase 6 protocol-parity audit — the two produced
    /// byte-for-byte identical signing bytes, payload, and signature. That
    /// comparison is not repeatable inside a single-tree test suite, but a
    /// change to `signing_bytes`, `encode`, `decode`, or `verify` that
    /// altered any of these fixed outputs would silently break
    /// interoperability with a peer running the other tree's build without
    /// any other test here catching it (every other test in this module
    /// re-derives its own expectations from the *current* code, so a
    /// consistent bug in both encode and its own round-trip check would
    /// still pass them). This test's expected values are hard-coded
    /// precisely so that cannot happen silently.
    ///
    /// Do not update these constants to make a future code change pass —
    /// that defeats the point. A deliberate, reviewed wire-format change
    /// must update the golden values here *and* be re-verified against the
    /// other platform out of band, exactly as this vector originally was.
    #[test]
    fn the_cross_platform_golden_vector_is_unchanged() {
        let dir = TempDir::new().unwrap();
        let requester = fixed_identity(&dir, "requester", 0x01);
        let target = fixed_identity(&dir, "target", 0x02);

        assert_eq!(
            requester.public_key_hex(),
            "8a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c"
        );
        assert_eq!(
            requester.node_id(),
            "34750f98bd59fcfc946da45aaabe933be154a4b5094e1c4abf42866505f3c97e"
        );
        assert_eq!(
            target.node_id(),
            "6a3803d5f059902a1c6dafbc9ba4729212f7caac08634cc3ae76b27529f03827"
        );

        let request = SyncRequest {
            target_origin: raw_id(&target),
            watermark: 3,
            have_bitmap: 0x8,
            max_events: 8,
            request_ts: 1_790_000_000_123,
        };

        let signing = signing_bytes(&raw_id(&requester), &request);
        assert_eq!(signing.len(), 170);
        assert_eq!(
            hex::encode(&signing),
            "7365637572656d6573682d6c6f72612d73796e632d76313a0000000000000001\
010000000000000020347\
50f98bd59fcfc946da45aaabe933be154a4b5094e1c4abf42866505f3c97e0000000000000020\
6a3803d5f059902a1c6dafbc9ba4729212f7caac08634cc3ae76b27529f0382700000000000000\
0800000000000000030000000000000008000000000000000800000000000000010800000000\
0000000800000\
1a0c4506c7b"
                .replace('\n', "")
        );

        let payload = encode(&requester, &request).unwrap();
        assert_eq!(payload.len(), 122);
        let expected_payload_hex = "016a3803d5f059902a1c6dafbc9ba4729212f7caac08634cc3ae76b27529f\
03827000000000000000300000000000000\
0808000001a0c4506c7b071756652996f1494cccf60b339df313b3d0d0fdbbe33ba6e159352e7\
477f1431b46435d2f7175395faf02829c50b81c4caf76ac85298b39a5aa49209d559102"
            .replace('\n', "");
        assert_eq!(hex::encode(&payload), expected_payload_hex);

        let signature = &payload[SYNC_REQUEST_PAYLOAD_BYTES - 64..];
        assert_eq!(
            hex::encode(signature),
            &expected_payload_hex[expected_payload_hex.len() - 128..]
        );

        // The vector also verifies under the exact production verification
        // path, not just byte-for-byte — proof the fixture is internally
        // consistent, not merely a frozen string.
        let verified = decode_verified(&payload, &raw_id(&requester), &requester.public_key_hex())
            .expect("the golden vector must verify under the current code");
        assert_eq!(verified, request);
    }

    /// Flips one byte inside a field, leaving the signature untouched.
    fn tampered(p: &Pair, offset: usize) -> Vec<u8> {
        let mut payload = signed_payload(p);
        payload[offset] ^= 0x01;
        payload
    }

    // 1
    #[test]
    fn a_valid_request_round_trips_through_encode_and_decode() {
        let p = pair();
        let request = request_for(&p.target);
        let decoded = decode(&encode(&p.requester, &request).unwrap()).unwrap();
        assert_eq!(decoded.request, request);
    }

    // 2
    #[test]
    fn the_payload_is_exactly_122_bytes() {
        let p = pair();
        assert_eq!(SYNC_REQUEST_PAYLOAD_BYTES, 122);
        assert_eq!(signed_payload(&p).len(), 122);
        assert!(SYNC_REQUEST_PAYLOAD_BYTES <= super::super::lora_transport::MAX_LORA_PAYLOAD_BYTES);
    }

    // 3
    #[test]
    fn a_signature_verifies_under_the_requester_key() {
        let p = pair();
        let request = verify_payload(&p, &signed_payload(&p)).unwrap();
        assert_eq!(request, request_for(&p.target));
    }

    // 4
    #[test]
    fn a_tampered_watermark_is_rejected() {
        let p = pair();
        let err = verify_payload(&p, &tampered(&p, WATERMARK_OFFSET + 7)).unwrap_err();
        assert!(err.message().contains("signature"));
    }

    // 5
    #[test]
    fn a_tampered_target_origin_is_rejected() {
        let p = pair();
        let err = verify_payload(&p, &tampered(&p, TARGET_OFFSET)).unwrap_err();
        assert!(err.message().contains("signature"));
    }

    // 6
    #[test]
    fn a_tampered_bitmap_is_rejected() {
        let p = pair();
        let err = verify_payload(&p, &tampered(&p, BITMAP_OFFSET + 7)).unwrap_err();
        assert!(err.message().contains("signature"));
    }

    // 7
    #[test]
    fn a_tampered_max_events_is_rejected() {
        let p = pair();
        // 8 -> 9 would fail the bounds check first; 8 -> 7 stays in bounds so
        // this isolates the signature check.
        let mut payload = signed_payload(&p);
        payload[MAX_EVENTS_OFFSET] = 7;
        let err = verify_payload(&p, &payload).unwrap_err();
        assert!(err.message().contains("signature"));
    }

    // 8
    #[test]
    fn a_tampered_request_ts_is_rejected() {
        let p = pair();
        let err = verify_payload(&p, &tampered(&p, TIMESTAMP_OFFSET + 7)).unwrap_err();
        assert!(err.message().contains("signature"));
    }

    #[test]
    fn a_tampered_signature_is_rejected() {
        let p = pair();
        let err = verify_payload(&p, &tampered(&p, SIGNATURE_OFFSET)).unwrap_err();
        assert!(err.message().contains("signature"));
    }

    // 9
    #[test]
    fn a_wrong_signing_key_is_rejected() {
        let p = pair();
        let impostor_dir = TempDir::new().unwrap();
        let impostor = identity(&impostor_dir);

        // Signed by the impostor, presented under the real requester's ID and key.
        let payload = encode(&impostor, &request_for(&p.target)).unwrap();
        let err = verify_payload(&p, &payload).unwrap_err();
        assert!(err.message().contains("signature"));
    }

    #[test]
    fn a_signature_is_bound_to_the_requester_it_was_made_for() {
        // A valid request from the requester, re-attributed to a different
        // frame source whose own key is supplied: the signed bytes include the
        // requester ID, so it cannot be claimed by anyone else.
        let p = pair();
        let other_dir = TempDir::new().unwrap();
        let other = identity(&other_dir);

        let err = decode_verified(
            &signed_payload(&p),
            &raw_id(&other),
            &other.public_key_hex(),
        )
        .unwrap_err();
        assert!(err.message().contains("signature"));
    }

    #[test]
    fn a_key_that_does_not_match_the_requester_id_is_refused() {
        // Existing SecureMesh binding rule: node ID = SHA-256(public key).
        let p = pair();
        let err = decode_verified(
            &signed_payload(&p),
            &raw_id(&p.requester),
            &p.target.public_key_hex(),
        )
        .unwrap_err();
        assert!(err.message().contains("does not match its node ID"));
    }

    #[test]
    fn no_public_key_is_carried_on_the_wire() {
        let p = pair();
        let payload = signed_payload(&p);
        let key = hex::decode(p.requester.public_key_hex()).unwrap();
        assert!(!payload.windows(32).any(|w| w == key.as_slice()));
        let id = raw_id(&p.requester);
        assert!(!payload.windows(32).any(|w| w == id));
    }

    // 10
    #[test]
    fn malformed_and_truncated_payloads_are_rejected() {
        let p = pair();
        let payload = signed_payload(&p);

        for len in [0, 1, 32, SIGNATURE_OFFSET, SYNC_REQUEST_PAYLOAD_BYTES - 1] {
            let err = decode(&payload[..len]).unwrap_err();
            assert!(err.message().contains("truncated"), "len {len}");
        }
        assert!(decode(&[0xFF; SYNC_REQUEST_PAYLOAD_BYTES]).is_err());
    }

    // 11
    #[test]
    fn trailing_bytes_are_rejected() {
        let p = pair();
        let mut payload = signed_payload(&p);
        payload.push(0);
        let err = decode(&payload).unwrap_err();
        assert!(err.message().contains("trailing"));
    }

    // 12
    #[test]
    fn max_events_outside_one_to_eight_is_rejected() {
        let p = pair();
        for bad in [0u8, 9, u8::MAX] {
            let request = SyncRequest {
                max_events: bad,
                ..request_for(&p.target)
            };
            assert!(encode(&p.requester, &request).is_err(), "encode {bad}");

            let mut payload = signed_payload(&p);
            payload[MAX_EVENTS_OFFSET] = bad;
            let err = decode(&payload).unwrap_err();
            assert!(err.message().contains("max_events"), "decode {bad}");
        }
        for good in 1..=MAX_SYNC_REQUEST_EVENTS {
            let request = SyncRequest {
                max_events: good,
                ..request_for(&p.target)
            };
            let payload = encode(&p.requester, &request).unwrap();
            assert_eq!(verify_payload(&p, &payload).unwrap().max_events, good);
        }
    }

    // 13
    #[test]
    fn an_unsupported_version_is_rejected() {
        let p = pair();
        for version in [0u8, 2, u8::MAX] {
            let mut payload = signed_payload(&p);
            payload[0] = version;
            let err = decode(&payload).unwrap_err();
            assert!(err.message().contains("version"), "version {version}");
        }
    }

    // 14
    #[test]
    fn signed_bytes_are_deterministic_and_canonical() {
        let p = pair();
        let requester = raw_id(&p.requester);
        let request = request_for(&p.target);

        let first = signing_bytes(&requester, &request);
        assert_eq!(first, signing_bytes(&requester, &request));
        assert!(first.starts_with(SYNC_REQUEST_SIGNING_DOMAIN));
        // Domain, then seven length-prefixed fields: 1+32+32+8+8+1+8 = 90
        // bytes of content plus 7 × 8 bytes of length.
        assert_eq!(first.len(), SYNC_REQUEST_SIGNING_DOMAIN.len() + 7 * 8 + 90);

        // Ed25519 is deterministic, so the whole payload is too.
        assert_eq!(signed_payload(&p), signed_payload(&p));

        // Decoding then re-encoding reproduces the payload byte for byte:
        // fixed-width big-endian fields leave no alternative encoding.
        let payload = signed_payload(&p);
        let decoded = decode(&payload).unwrap();
        assert_eq!(encode(&p.requester, &decoded.request).unwrap(), payload);
    }

    #[test]
    fn integers_are_big_endian_at_their_documented_offsets() {
        let p = pair();
        let request = SyncRequest {
            target_origin: raw_id(&p.target),
            watermark: 0x0102_0304_0506_0708,
            have_bitmap: 0x1112_1314_1516_1718,
            max_events: 5,
            request_ts: -2,
        };
        let payload = encode(&p.requester, &request).unwrap();
        assert_eq!(payload[0], 1);
        assert_eq!(
            &payload[TARGET_OFFSET..WATERMARK_OFFSET],
            &request.target_origin
        );
        assert_eq!(
            &payload[WATERMARK_OFFSET..BITMAP_OFFSET],
            &[1, 2, 3, 4, 5, 6, 7, 8]
        );
        assert_eq!(
            &payload[BITMAP_OFFSET..MAX_EVENTS_OFFSET],
            &[0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18]
        );
        assert_eq!(payload[MAX_EVENTS_OFFSET], 5);
        assert_eq!(
            &payload[TIMESTAMP_OFFSET..SIGNATURE_OFFSET],
            &(-2i64).to_be_bytes()
        );
        assert_eq!(verify_payload(&p, &payload).unwrap(), request);
    }

    #[test]
    fn the_signing_domain_is_distinct_from_the_envelope_domain() {
        let p = pair();
        let requester = raw_id(&p.requester);
        let request = request_for(&p.target);

        // Same fields, signed under the envelope domain instead.
        let mut foreign = signing_bytes(&requester, &request);
        foreign.splice(
            ..SYNC_REQUEST_SIGNING_DOMAIN.len(),
            super::super::protocol::ENVELOPE_SIGNING_DOMAIN
                .iter()
                .copied(),
        );
        let signature = p.requester.sign(&foreign);

        let mut payload = signed_payload(&p);
        payload[SIGNATURE_OFFSET..].copy_from_slice(&signature);
        let err = verify_payload(&p, &payload).unwrap_err();
        assert!(err.message().contains("signature"));
    }

    #[test]
    fn a_request_targeting_its_own_requester_is_refused() {
        let p = pair();
        let request = SyncRequest {
            target_origin: raw_id(&p.requester),
            ..request_for(&p.target)
        };
        assert!(encode(&p.requester, &request).is_err());
    }
}
