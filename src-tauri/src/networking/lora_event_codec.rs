//! Compact binary encoding of one signed SecureMesh event, sized for a single
//! LoRa frame.
//!
//! # What this is not
//!
//! This is **not** a second event format, and it signs nothing. It is a
//! lossless re-encoding of an existing [`MeshEvent`] whose purpose is to carry
//! that event's *existing* Ed25519 signature across a 240-byte radio packet.
//! The receiver rebuilds the exact [`MeshEvent`] the origin signed — the same
//! `event_id` text, the same payload JSON, the same timestamp text — so
//! [`MeshEvent::verify`] and [`MeshEvent::content_hash`] behave exactly as they
//! do for an event that arrived over QUIC. Nothing in the domain layer knows
//! which transport delivered an event.
//!
//! # Wire format (big-endian throughout)
//!
//! ```text
//! codec_version   u8        = 1
//! kind            u8        1 = INCIDENT_CREATED, 2 = INCIDENT_OBSERVATION
//! event_id        [16]      raw UUID bytes
//! origin_seq      u64
//! created_at      i64       Unix milliseconds
//! signature       [64]      raw Ed25519 signature
//! body            (kind-specific, below)
//!
//! INCIDENT_CREATED body:
//!   incident_id          [16]  raw UUID bytes
//!   severity             u8    1 LOW, 2 MEDIUM, 3 HIGH, 4 CRITICAL
//!   location_source      u8    1 GNSS, 2 WIRELESS, 3 UNKNOWN
//!   flags                u8    bit0 latitude, bit1 longitude,
//!                              bit2 accuracy_meters, bit3 location_captured_at
//!   latitude             f64   IEEE-754 bits, present if bit0
//!   longitude            f64   present if bit1
//!   accuracy_meters      f64   present if bit2
//!   location_captured_at i64   Unix nanoseconds, present if bit3
//!   description_len      u8
//!   description          UTF-8
//!
//! INCIDENT_OBSERVATION body:
//!   observation_id  [16]
//!   incident_id     [16]
//!   note_len        u8
//!   note            UTF-8
//! ```
//!
//! Absent from the wire, by design: `origin_node` (the LoRa frame's
//! `source_node_id` already carries those 32 bytes) and `origin_public_key`
//! (the receiver takes it from its own trust state, never from the air).
//!
//! # Lossless or refused
//!
//! [`encode`] finishes by decoding its own output and comparing it with the
//! original event field by field. Anything that would not survive exactly —
//! a payload not written by `serde_json` in canonical form, a timestamp with
//! sub-millisecond precision, a non-canonical UUID, an unknown severity, or an
//! event too large for one frame — is refused with a stated reason. Nothing
//! is truncated or normalised to make it fit.

use super::lora_transport::MAX_SECUREMESH_EVENT_PAYLOAD_BYTES;
use crate::domain::event::{
    EventKind, IncidentCreatedPayload, IncidentObservationPayload, MeshEvent,
};
use crate::domain::incident::Severity;
use crate::domain::LocationSource;
use crate::error::{CoreError, CoreResult};
use chrono::{DateTime, Utc};
use uuid::Uuid;

/// Version of this compact layout, independent of the outer LoRa frame
/// version so the event encoding can evolve without changing the frame.
pub const EVENT_CODEC_VERSION: u8 = 1;

/// Bytes every encoded event spends before its kind-specific body:
/// version, kind, event_id, origin_seq, created_at, signature.
pub const FIXED_HEADER_BYTES: usize = 1 + 1 + 16 + 8 + 8 + 64;

const KIND_INCIDENT_CREATED: u8 = 1;
const KIND_INCIDENT_OBSERVATION: u8 = 2;

const FLAG_LATITUDE: u8 = 1 << 0;
const FLAG_LONGITUDE: u8 = 1 << 1;
const FLAG_ACCURACY: u8 = 1 << 2;
const FLAG_CAPTURED_AT: u8 = 1 << 3;
const KNOWN_FLAGS: u8 = FLAG_LATITUDE | FLAG_LONGITUDE | FLAG_ACCURACY | FLAG_CAPTURED_AT;

/// Encodes a signed event for one LoRa frame.
///
/// Never signs anything: the event's existing signature is carried verbatim.
pub fn encode(event: &MeshEvent) -> CoreResult<Vec<u8>> {
    let source_node_id = decode_fixed::<32>(&event.origin_node, "origin node ID")?;
    let event_id = parse_uuid(&event.event_id, "event ID")?;
    let signature = decode_fixed::<64>(&event.signature, "event signature")?;

    let mut out = Vec::with_capacity(MAX_SECUREMESH_EVENT_PAYLOAD_BYTES);
    out.push(EVENT_CODEC_VERSION);
    out.push(kind_code(event.kind));
    out.extend_from_slice(event_id.as_bytes());
    out.extend_from_slice(&event.origin_seq.to_be_bytes());
    out.extend_from_slice(&event.created_at.timestamp_millis().to_be_bytes());
    out.extend_from_slice(&signature);

    match event.kind {
        EventKind::IncidentCreated => encode_created(&mut out, event)?,
        EventKind::IncidentObservation => encode_observation(&mut out, event)?,
    }

    if out.len() > MAX_SECUREMESH_EVENT_PAYLOAD_BYTES {
        return Err(too_large(out.len()));
    }

    // The guarantee this module exists to give: the receiver will rebuild
    // exactly this event. Checked here, at the source, so an event that cannot
    // be carried faithfully is refused before it is ever transmitted.
    let rebuilt = decode(&out, &source_node_id, &event.origin_public_key)?;
    if let Some(field) = first_difference(&rebuilt, event) {
        return Err(CoreError::validation(format!(
            "event cannot be represented losslessly in the compact LoRa encoding \
             ({field} would change)"
        )));
    }

    Ok(out)
}

/// Rebuilds a [`MeshEvent`] from its compact encoding.
///
/// `source_node_id` is the LoRa frame's sender and becomes `origin_node`;
/// `origin_public_key_hex` must come from this node's own trust state. This
/// function does **not** verify the signature or check trust — that is
/// [`super::lora_event_ingest`]'s job, and it must call [`MeshEvent::verify`]
/// on the result before the event is used for anything.
pub fn decode(
    payload: &[u8],
    source_node_id: &[u8; 32],
    origin_public_key_hex: &str,
) -> CoreResult<MeshEvent> {
    let public_key = decode_fixed::<32>(origin_public_key_hex, "origin public key")?;
    let mut reader = Reader::new(payload);

    let version = reader.u8()?;
    if version != EVENT_CODEC_VERSION {
        return Err(CoreError::validation(format!(
            "unsupported compact LoRa event version {version}"
        )));
    }

    let kind = match reader.u8()? {
        KIND_INCIDENT_CREATED => EventKind::IncidentCreated,
        KIND_INCIDENT_OBSERVATION => EventKind::IncidentObservation,
        other => {
            return Err(CoreError::validation(format!(
                "unknown compact LoRa event kind {other}"
            )))
        }
    };

    let event_id = Uuid::from_bytes(reader.array::<16>()?).to_string();
    let origin_seq = reader.u64()?;
    let created_at = DateTime::<Utc>::from_timestamp_millis(reader.i64()?)
        .ok_or_else(|| CoreError::validation("event timestamp is out of range"))?;
    let signature = reader.array::<64>()?;

    let payload_json = match kind {
        EventKind::IncidentCreated => serde_json::to_string(&decode_created(&mut reader)?)?,
        EventKind::IncidentObservation => serde_json::to_string(&decode_observation(&mut reader)?)?,
    };

    reader.finish()?;

    Ok(MeshEvent {
        event_id,
        origin_node: hex::encode(source_node_id),
        origin_public_key: hex::encode(public_key),
        origin_seq,
        kind,
        payload: payload_json,
        created_at,
        signature: hex::encode(signature),
    })
}

fn encode_created(out: &mut Vec<u8>, event: &MeshEvent) -> CoreResult<()> {
    let mut payload = event.incident_created_payload()?;

    // `serde_json`'s default float parser is not exactly inverse to its
    // writer: a real reading has been seen to come back one unit in the last
    // place away (see `LocationHeartbeat` in `protocol.rs`). Re-reading each
    // number from its exact source text with the standard library's
    // correctly-rounded parser recovers the value the origin actually wrote.
    // The round-trip check in `encode` still guards the result either way.
    if let Some(tokens) = top_level_number_tokens(&event.payload) {
        payload.latitude = exact_float(&tokens, "latitude").or(payload.latitude);
        payload.longitude = exact_float(&tokens, "longitude").or(payload.longitude);
        payload.accuracy_meters =
            exact_float(&tokens, "accuracyMeters").or(payload.accuracy_meters);
    }

    out.extend_from_slice(parse_uuid(&payload.incident_id, "incident ID")?.as_bytes());
    out.push(severity_code(&payload.severity)?);
    out.push(location_source_code(payload.location_source));

    let mut flags = 0u8;
    if payload.latitude.is_some() {
        flags |= FLAG_LATITUDE;
    }
    if payload.longitude.is_some() {
        flags |= FLAG_LONGITUDE;
    }
    if payload.accuracy_meters.is_some() {
        flags |= FLAG_ACCURACY;
    }
    if payload.location_captured_at.is_some() {
        flags |= FLAG_CAPTURED_AT;
    }
    out.push(flags);

    for value in [payload.latitude, payload.longitude, payload.accuracy_meters]
        .into_iter()
        .flatten()
    {
        out.extend_from_slice(&value.to_bits().to_be_bytes());
    }
    if let Some(captured_at) = payload.location_captured_at {
        let nanos = captured_at.timestamp_nanos_opt().ok_or_else(|| {
            CoreError::validation("location capture time is outside the encodable range")
        })?;
        out.extend_from_slice(&nanos.to_be_bytes());
    }

    push_str(out, &payload.description, "description")
}

fn encode_observation(out: &mut Vec<u8>, event: &MeshEvent) -> CoreResult<()> {
    let payload = event.incident_observation_payload()?;
    out.extend_from_slice(parse_uuid(&payload.observation_id, "observation ID")?.as_bytes());
    out.extend_from_slice(parse_uuid(&payload.incident_id, "incident ID")?.as_bytes());
    push_str(out, &payload.note, "note")
}

fn decode_created(reader: &mut Reader<'_>) -> CoreResult<IncidentCreatedPayload> {
    let incident_id = Uuid::from_bytes(reader.array::<16>()?).to_string();
    let severity = severity_from_code(reader.u8()?)?;
    let location_source = location_source_from_code(reader.u8()?)?;

    let flags = reader.u8()?;
    if flags & !KNOWN_FLAGS != 0 {
        return Err(CoreError::validation(
            "compact LoRa event sets unknown location flags",
        ));
    }

    let latitude = if flags & FLAG_LATITUDE != 0 {
        Some(reader.f64()?)
    } else {
        None
    };
    let longitude = if flags & FLAG_LONGITUDE != 0 {
        Some(reader.f64()?)
    } else {
        None
    };
    let accuracy_meters = if flags & FLAG_ACCURACY != 0 {
        Some(reader.f64()?)
    } else {
        None
    };
    let location_captured_at = if flags & FLAG_CAPTURED_AT != 0 {
        Some(DateTime::<Utc>::from_timestamp_nanos(reader.i64()?))
    } else {
        None
    };

    Ok(IncidentCreatedPayload {
        incident_id,
        description: reader.string()?,
        severity: severity.as_str().to_string(),
        latitude,
        longitude,
        accuracy_meters,
        location_source,
        location_captured_at,
    })
}

fn decode_observation(reader: &mut Reader<'_>) -> CoreResult<IncidentObservationPayload> {
    Ok(IncidentObservationPayload {
        observation_id: Uuid::from_bytes(reader.array::<16>()?).to_string(),
        incident_id: Uuid::from_bytes(reader.array::<16>()?).to_string(),
        note: reader.string()?,
    })
}

fn kind_code(kind: EventKind) -> u8 {
    match kind {
        EventKind::IncidentCreated => KIND_INCIDENT_CREATED,
        EventKind::IncidentObservation => KIND_INCIDENT_OBSERVATION,
    }
}

// Explicit matches rather than indexes into `Severity::ALL` or
// `LocationSource::ALL`: these are wire values, and reordering a Rust array
// must never silently renumber them.

fn severity_code(severity: &str) -> CoreResult<u8> {
    match severity {
        "LOW" => Ok(1),
        "MEDIUM" => Ok(2),
        "HIGH" => Ok(3),
        "CRITICAL" => Ok(4),
        _ => Err(CoreError::validation(
            "incident severity has no compact LoRa encoding",
        )),
    }
}

fn severity_from_code(code: u8) -> CoreResult<Severity> {
    match code {
        1 => Ok(Severity::Low),
        2 => Ok(Severity::Medium),
        3 => Ok(Severity::High),
        4 => Ok(Severity::Critical),
        _ => Err(CoreError::validation(format!(
            "unknown compact LoRa severity code {code}"
        ))),
    }
}

fn location_source_code(source: LocationSource) -> u8 {
    match source {
        LocationSource::Gnss => 1,
        LocationSource::Wireless => 2,
        LocationSource::Unknown => 3,
    }
}

fn location_source_from_code(code: u8) -> CoreResult<LocationSource> {
    match code {
        1 => Ok(LocationSource::Gnss),
        2 => Ok(LocationSource::Wireless),
        3 => Ok(LocationSource::Unknown),
        _ => Err(CoreError::validation(format!(
            "unknown compact LoRa location source code {code}"
        ))),
    }
}

fn too_large(bytes: usize) -> CoreError {
    CoreError::validation(format!(
        "event needs {bytes} bytes but one LoRa event frame carries at most \
         {MAX_SECUREMESH_EVENT_PAYLOAD_BYTES}; fragmentation is not implemented"
    ))
}

fn push_str(out: &mut Vec<u8>, value: &str, field: &str) -> CoreResult<()> {
    let length = u8::try_from(value.len()).map_err(|_| {
        CoreError::validation(format!(
            "{field} is {} bytes, too long for one LoRa event frame; \
             fragmentation is not implemented",
            value.len()
        ))
    })?;
    out.push(length);
    out.extend_from_slice(value.as_bytes());
    Ok(())
}

fn parse_uuid(value: &str, field: &str) -> CoreResult<Uuid> {
    Uuid::parse_str(value)
        .map_err(|_| CoreError::validation(format!("{field} is not a valid UUID")))
}

fn decode_fixed<const N: usize>(hex_value: &str, field: &str) -> CoreResult<[u8; N]> {
    let bytes = hex::decode(hex_value)
        .map_err(|_| CoreError::validation(format!("{field} is not valid hex")))?;
    <[u8; N]>::try_from(bytes.as_slice())
        .map_err(|_| CoreError::validation(format!("{field} has a wrong length")))
}

/// The first field on which two events differ, if any.
fn first_difference(a: &MeshEvent, b: &MeshEvent) -> Option<&'static str> {
    if a.event_id != b.event_id {
        Some("event_id")
    } else if a.origin_node != b.origin_node {
        Some("origin_node")
    } else if a.origin_public_key != b.origin_public_key {
        Some("origin_public_key")
    } else if a.origin_seq != b.origin_seq {
        Some("origin_seq")
    } else if a.kind != b.kind {
        Some("kind")
    } else if a.payload != b.payload {
        Some("payload")
    } else if a.created_at != b.created_at {
        Some("created_at")
    } else if a.signature != b.signature {
        Some("signature")
    } else {
        None
    }
}

/// The `"key": number` pairs at the top level of a flat JSON object, each with
/// the number's exact source text. `None` for any shape this does not read
/// (nested objects or arrays), in which case callers fall back to the value
/// `serde_json` produced and the round-trip check decides.
fn top_level_number_tokens(json: &str) -> Option<Vec<(String, String)>> {
    let bytes = json.as_bytes();
    let mut at = 0;
    let mut tokens = Vec::new();

    skip_whitespace(bytes, &mut at);
    if bytes.get(at) != Some(&b'{') {
        return None;
    }
    at += 1;
    skip_whitespace(bytes, &mut at);
    if bytes.get(at) == Some(&b'}') {
        return Some(tokens);
    }

    loop {
        skip_whitespace(bytes, &mut at);
        let key = read_json_string(bytes, &mut at)?;
        skip_whitespace(bytes, &mut at);
        if bytes.get(at) != Some(&b':') {
            return None;
        }
        at += 1;
        skip_whitespace(bytes, &mut at);

        match *bytes.get(at)? {
            b'"' => {
                read_json_string(bytes, &mut at)?;
            }
            b'-' | b'0'..=b'9' => {
                let start = at;
                while at < bytes.len()
                    && matches!(bytes[at], b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9')
                {
                    at += 1;
                }
                tokens.push((key, json[start..at].to_string()));
            }
            b'n' | b't' | b'f' => {
                while at < bytes.len() && bytes[at].is_ascii_alphabetic() {
                    at += 1;
                }
            }
            _ => return None,
        }

        skip_whitespace(bytes, &mut at);
        match *bytes.get(at)? {
            b',' => at += 1,
            b'}' => return Some(tokens),
            _ => return None,
        }
    }
}

fn skip_whitespace(bytes: &[u8], at: &mut usize) {
    while *at < bytes.len() && matches!(bytes[*at], b' ' | b'\t' | b'\n' | b'\r') {
        *at += 1;
    }
}

/// Reads a JSON string starting at its opening quote, returning its raw
/// (still-escaped) contents. Escapes are skipped, not decoded: the only keys
/// this is used to match contain none.
fn read_json_string(bytes: &[u8], at: &mut usize) -> Option<String> {
    if bytes.get(*at) != Some(&b'"') {
        return None;
    }
    *at += 1;
    let start = *at;
    while *at < bytes.len() {
        match bytes[*at] {
            b'\\' => *at += 2,
            b'"' => {
                let raw = std::str::from_utf8(&bytes[start..*at]).ok()?.to_string();
                *at += 1;
                return Some(raw);
            }
            _ => *at += 1,
        }
    }
    None
}

fn exact_float(tokens: &[(String, String)], key: &str) -> Option<f64> {
    tokens
        .iter()
        .find(|(name, _)| name == key)
        .and_then(|(_, text)| text.parse::<f64>().ok())
}

/// Bounds-checked big-endian reader. Every read fails cleanly on truncated
/// input; nothing here can panic on hostile bytes.
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }

    fn take(&mut self, count: usize) -> CoreResult<&'a [u8]> {
        let end = self
            .at
            .checked_add(count)
            .filter(|end| *end <= self.bytes.len())
            .ok_or_else(|| CoreError::validation("compact LoRa event is truncated"))?;
        let slice = &self.bytes[self.at..end];
        self.at = end;
        Ok(slice)
    }

    fn array<const N: usize>(&mut self) -> CoreResult<[u8; N]> {
        let mut out = [0u8; N];
        out.copy_from_slice(self.take(N)?);
        Ok(out)
    }

    fn u8(&mut self) -> CoreResult<u8> {
        Ok(self.take(1)?[0])
    }

    fn u64(&mut self) -> CoreResult<u64> {
        Ok(u64::from_be_bytes(self.array::<8>()?))
    }

    fn i64(&mut self) -> CoreResult<i64> {
        Ok(i64::from_be_bytes(self.array::<8>()?))
    }

    fn f64(&mut self) -> CoreResult<f64> {
        let value = f64::from_bits(self.u64()?);
        // JSON cannot represent NaN or infinity, so no genuine event holds one.
        if !value.is_finite() {
            return Err(CoreError::validation(
                "compact LoRa event carries a non-finite number",
            ));
        }
        Ok(value)
    }

    fn string(&mut self) -> CoreResult<String> {
        let length = self.u8()? as usize;
        let bytes = self.take(length)?;
        String::from_utf8(bytes.to_vec())
            .map_err(|_| CoreError::validation("compact LoRa event text is not valid UTF-8"))
    }

    fn finish(&self) -> CoreResult<()> {
        if self.at != self.bytes.len() {
            return Err(CoreError::validation(
                "compact LoRa event has trailing bytes",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::event::UnsignedEvent;
    use crate::identity::keystore::FileKeyStore;
    use crate::identity::NodeIdentity;
    use crate::networking::lora_transport::{
        LoraFrame, LoraMessageType, MAX_SECUREMESH_EVENT_FRAME_BYTES,
    };
    use tempfile::TempDir;

    fn identity(dir: &TempDir) -> NodeIdentity {
        NodeIdentity::load_or_create(&FileKeyStore::new(dir.path().join("id.json"))).unwrap()
    }

    fn created(signer: &NodeIdentity, seq: u64, description: &str, located: bool) -> MeshEvent {
        let (latitude, longitude, accuracy, source, captured) = if located {
            (
                Some(13.133599),
                Some(77.56533),
                Some(12.5),
                LocationSource::Gnss,
                Some(crate::domain::now()),
            )
        } else {
            (None, None, None, LocationSource::Unknown, None)
        };
        MeshEvent::create(
            signer,
            seq,
            EventKind::IncidentCreated,
            IncidentCreatedPayload {
                incident_id: Uuid::new_v4().to_string(),
                description: description.to_string(),
                severity: "HIGH".to_string(),
                latitude,
                longitude,
                accuracy_meters: accuracy,
                location_source: source,
                location_captured_at: captured,
            },
        )
        .unwrap()
    }

    fn observation(signer: &NodeIdentity, seq: u64, note: &str) -> MeshEvent {
        MeshEvent::create(
            signer,
            seq,
            EventKind::IncidentObservation,
            IncidentObservationPayload {
                observation_id: Uuid::new_v4().to_string(),
                incident_id: Uuid::new_v4().to_string(),
                note: note.to_string(),
            },
        )
        .unwrap()
    }

    fn source_of(event: &MeshEvent) -> [u8; 32] {
        decode_fixed::<32>(&event.origin_node, "origin").unwrap()
    }

    fn round_trip(event: &MeshEvent) -> MeshEvent {
        let bytes = encode(event).unwrap();
        decode(&bytes, &source_of(event), &event.origin_public_key).unwrap()
    }

    /// Signs arbitrary unsigned content, for events `MeshEvent::create` would
    /// never produce but a peer on the wire might.
    fn sign(signer: &NodeIdentity, unsigned: UnsignedEvent) -> MeshEvent {
        let signature = hex::encode(signer.sign(&unsigned.canonical_bytes()));
        MeshEvent {
            event_id: unsigned.event_id,
            origin_node: unsigned.origin_node,
            origin_public_key: unsigned.origin_public_key,
            origin_seq: unsigned.origin_seq,
            kind: unsigned.kind,
            payload: unsigned.payload,
            created_at: unsigned.created_at,
            signature,
        }
    }

    // --- Round trip and exact reconstruction (tests 1, 2, 3) --------------

    #[test]
    fn an_incident_without_location_round_trips_exactly() {
        let dir = TempDir::new().unwrap();
        let event = created(&identity(&dir), 1, "Road blocked at gate 2", false);

        let rebuilt = round_trip(&event);
        assert_eq!(rebuilt, event);
        assert_eq!(rebuilt.canonical_bytes(), event.canonical_bytes());
        assert_eq!(rebuilt.content_hash(), event.content_hash());
        assert!(rebuilt.verify().is_ok());
    }

    #[test]
    fn an_incident_with_full_location_round_trips_exactly() {
        let dir = TempDir::new().unwrap();
        let event = created(&identity(&dir), 7, "Flooding north gate", true);

        let rebuilt = round_trip(&event);
        assert_eq!(rebuilt, event);
        assert_eq!(rebuilt.content_hash(), event.content_hash());
        assert!(rebuilt.verify().is_ok());
    }

    #[test]
    fn an_observation_round_trips_exactly() {
        let dir = TempDir::new().unwrap();
        let event = observation(&identity(&dir), 3, "Water rising");

        let rebuilt = round_trip(&event);
        assert_eq!(rebuilt, event);
        assert!(rebuilt.verify().is_ok());
    }

    #[test]
    fn a_coordinate_the_json_parser_misreads_is_still_carried_exactly() {
        // The value observed in the field to leave as ...905 and return from
        // serde_json as ...903. It must survive, not be refused or altered.
        let dir = TempDir::new().unwrap();
        let signer = identity(&dir);
        let event = MeshEvent::create(
            &signer,
            1,
            EventKind::IncidentCreated,
            IncidentCreatedPayload {
                incident_id: Uuid::new_v4().to_string(),
                description: "precision".to_string(),
                severity: "LOW".to_string(),
                latitude: Some("13.133598560775905".parse().unwrap()),
                longitude: Some("77.56533012345678".parse().unwrap()),
                accuracy_meters: None,
                location_source: LocationSource::Gnss,
                location_captured_at: None,
            },
        )
        .unwrap();

        let rebuilt = round_trip(&event);
        assert_eq!(rebuilt.payload, event.payload);
        assert!(rebuilt.verify().is_ok());
    }

    #[test]
    fn origin_identity_is_not_on_the_wire() {
        // origin_node comes from the frame and the key from local trust
        // state, so neither appears in the compact bytes.
        let dir = TempDir::new().unwrap();
        let event = created(&identity(&dir), 1, "x", false);
        let bytes = encode(&event).unwrap();

        let node_id = source_of(&event);
        let key = decode_fixed::<32>(&event.origin_public_key, "key").unwrap();
        assert!(!bytes.windows(32).any(|w| w == node_id));
        assert!(!bytes.windows(32).any(|w| w == key));
    }

    // --- Tampering is caught by the existing signature (tests 4-7) --------

    fn tampered(event: &MeshEvent, offset: usize) -> MeshEvent {
        let mut bytes = encode(event).unwrap();
        bytes[offset] ^= 0x01;
        decode(&bytes, &source_of(event), &event.origin_public_key).unwrap()
    }

    #[test]
    fn a_modified_payload_fails_verification() {
        let dir = TempDir::new().unwrap();
        let event = created(&identity(&dir), 1, "severity check", false);
        let last = encode(&event).unwrap().len() - 1; // final description byte

        assert!(tampered(&event, last).verify().is_err());
    }

    #[test]
    fn a_modified_origin_seq_fails_verification() {
        let dir = TempDir::new().unwrap();
        // Sequence 2, so the flipped value (3) is still a legal sequence
        // number and only the signature can reject it.
        let event = created(&identity(&dir), 2, "seq", false);
        let seq_offset = 1 + 1 + 16 + 7; // low byte of origin_seq

        let forged = tampered(&event, seq_offset);
        assert_eq!(forged.origin_seq, 3);
        let err = forged.verify().unwrap_err();
        assert!(err.message().contains("signature"));
    }

    #[test]
    fn a_modified_event_id_fails_verification() {
        let dir = TempDir::new().unwrap();
        let event = created(&identity(&dir), 1, "id", false);

        let forged = tampered(&event, 2); // first event_id byte
        assert_ne!(forged.event_id, event.event_id);
        assert!(forged.verify().is_err());
    }

    #[test]
    fn a_modified_timestamp_or_signature_fails_verification() {
        let dir = TempDir::new().unwrap();
        let event = created(&identity(&dir), 1, "time", false);

        assert!(tampered(&event, 1 + 1 + 16 + 8 + 7).verify().is_err()); // created_at
        assert!(tampered(&event, FIXED_HEADER_BYTES - 1).verify().is_err()); // signature
    }

    #[test]
    fn decoding_with_the_wrong_public_key_fails_verification() {
        let dir_a = TempDir::new().unwrap();
        let dir_b = TempDir::new().unwrap();
        let author = identity(&dir_a);
        let other = identity(&dir_b);
        let event = created(&author, 1, "wrong key", false);

        let bytes = encode(&event).unwrap();
        let forged = decode(&bytes, &source_of(&event), &other.public_key_hex()).unwrap();

        let err = forged.verify().unwrap_err();
        assert!(err.message().contains("does not match its public key"));
    }

    // --- Budget and refusal, never truncation (test 12) --------------------

    fn largest_fitting(make: impl Fn(&str) -> MeshEvent) -> usize {
        (0..=255)
            .take_while(|length| encode(&make(&"x".repeat(*length))).is_ok())
            .last()
            .unwrap()
    }

    #[test]
    fn the_text_budget_is_exactly_what_the_frame_allows() {
        let dir = TempDir::new().unwrap();
        let signer = identity(&dir);

        assert_eq!(FIXED_HEADER_BYTES, 98);
        assert_eq!(largest_fitting(|d| created(&signer, 1, d, false)), 74);
        assert_eq!(largest_fitting(|d| created(&signer, 1, d, true)), 42);
        assert_eq!(largest_fitting(|n| observation(&signer, 1, n)), 61);
    }

    #[test]
    fn the_largest_event_frame_stays_within_239_bytes() {
        let dir = TempDir::new().unwrap();
        let signer = identity(&dir);

        for event in [
            created(&signer, u64::MAX >> 1, &"x".repeat(74), false),
            created(&signer, 1, &"x".repeat(42), true),
            observation(&signer, 1, &"x".repeat(61)),
        ] {
            let frame = LoraFrame {
                message_type: LoraMessageType::SecureMeshEvent,
                source_node_id: source_of(&event),
                sequence: u32::MAX,
                payload: encode(&event).unwrap(),
            };
            let bytes = frame.encode().unwrap();
            assert!(bytes.len() <= MAX_SECUREMESH_EVENT_FRAME_BYTES);
            assert!(bytes.len() < 240);
        }
    }

    #[test]
    fn an_event_that_does_not_fit_is_refused_not_truncated() {
        let dir = TempDir::new().unwrap();
        let event = created(&identity(&dir), 1, &"x".repeat(75), false);

        let err = encode(&event).unwrap_err();
        assert!(err.message().contains("fragmentation is not implemented"));
    }

    #[test]
    fn a_long_multibyte_description_is_refused_not_truncated() {
        let dir = TempDir::new().unwrap();
        // 30 characters, 90 UTF-8 bytes: the budget is in bytes, not chars.
        let event = created(&identity(&dir), 1, &"ನ".repeat(30), false);
        assert!(encode(&event).is_err());
    }

    // --- Lossless or refused ------------------------------------------------

    #[test]
    fn a_non_canonical_payload_is_refused_rather_than_rewritten() {
        let dir = TempDir::new().unwrap();
        let signer = identity(&dir);
        let incident_id = Uuid::new_v4();
        // Valid, signed, but with whitespace serde_json would never write.
        let payload = format!(
            "{{ \"incidentId\": \"{incident_id}\", \"description\": \"x\", \"severity\": \"LOW\", \
             \"latitude\": null, \"longitude\": null, \"accuracyMeters\": null, \
             \"locationSource\": \"UNKNOWN\", \"locationCapturedAt\": null }}"
        );
        let event = sign(
            &signer,
            UnsignedEvent {
                event_id: Uuid::new_v4().to_string(),
                origin_node: signer.node_id().to_string(),
                origin_public_key: signer.public_key_hex(),
                origin_seq: 1,
                kind: EventKind::IncidentCreated,
                payload,
                created_at: crate::domain::now(),
            },
        );
        assert!(event.verify().is_ok());

        let err = encode(&event).unwrap_err();
        assert!(err.message().contains("losslessly"));
        assert!(err.message().contains("payload"));
    }

    #[test]
    fn a_sub_millisecond_timestamp_is_refused_rather_than_rounded() {
        let dir = TempDir::new().unwrap();
        let signer = identity(&dir);
        let template = created(&signer, 1, "x", false);
        let event = sign(
            &signer,
            UnsignedEvent {
                event_id: template.event_id.clone(),
                origin_node: template.origin_node.clone(),
                origin_public_key: template.origin_public_key.clone(),
                origin_seq: 1,
                kind: EventKind::IncidentCreated,
                payload: template.payload.clone(),
                created_at: template.created_at + chrono::Duration::microseconds(1),
            },
        );

        let err = encode(&event).unwrap_err();
        assert!(err.message().contains("created_at"));
    }

    #[test]
    fn an_unknown_severity_is_refused() {
        let dir = TempDir::new().unwrap();
        let signer = identity(&dir);
        let event = MeshEvent::create(
            &signer,
            1,
            EventKind::IncidentCreated,
            IncidentCreatedPayload {
                incident_id: Uuid::new_v4().to_string(),
                description: "x".to_string(),
                severity: "SEVERE".to_string(),
                latitude: None,
                longitude: None,
                accuracy_meters: None,
                location_source: LocationSource::Unknown,
                location_captured_at: None,
            },
        )
        .unwrap();
        assert!(encode(&event).is_err());
    }

    // --- Hostile input -------------------------------------------------------

    #[test]
    fn malformed_compact_bytes_are_rejected_without_panicking() {
        let dir = TempDir::new().unwrap();
        let event = created(&identity(&dir), 1, "hostile", true);
        let good = encode(&event).unwrap();
        let source = source_of(&event);
        let key = &event.origin_public_key;

        let mut wrong_version = good.clone();
        wrong_version[0] = 9;
        let mut wrong_kind = good.clone();
        wrong_kind[1] = 9;
        let mut unknown_flags = good.clone();
        unknown_flags[FIXED_HEADER_BYTES + 18] |= 0x80;
        let mut trailing = good.clone();
        trailing.push(0);

        for bytes in [
            Vec::new(),
            good[..good.len() - 1].to_vec(),
            good[..FIXED_HEADER_BYTES].to_vec(),
            wrong_version,
            wrong_kind,
            unknown_flags,
            trailing,
            vec![0xFF; 192],
        ] {
            assert!(decode(&bytes, &source, key).is_err());
        }

        assert!(decode(&good, &source, "not-hex").is_err());
        assert!(decode(&good, &source, "aabb").is_err());
    }

    // --- Stage 2: the real captured payload, through the real codec --------
    //
    // Bytes captured off the air on the same bench run as the frame proven
    // in `lora_transport`'s own regression test — the 142 payload bytes that
    // sit between that frame's 43-byte outer header and 4-byte outer CRC.
    // This calls the actual `decode` function above; nothing here re-derives
    // its fields by hand.
    //
    // `PLACEHOLDER_NOT_A_REAL_KEY` is exactly that: no keystore holding this
    // sender's genuine public key exists in this environment, and inventing
    // one would be a fabricated cryptographic fixture. Using an all-zero
    // stand-in is safe *only* because `decode` never uses this parameter
    // cryptographically — see its doc comment above: it hex-decodes the
    // string, checks its length, and round-trips it verbatim into
    // `MeshEvent.origin_public_key`. Signature verification is a separate,
    // later step (`MeshEvent::verify`, called by `lora_event_ingest`, not
    // here) that this test deliberately does not reach. Consistent with
    // that, nothing below asserts anything about `origin_public_key`.
    #[test]
    fn the_actual_captured_payload_decodes_through_the_real_codec() {
        const CAPTURED_PAYLOAD_HEX: &str = "0101815FF0674A6A4C8A9788473E00835F9E0000000000000002000001A0D113A5F70B350C39F06E9DB2ED8B512346081EF2A456C72566CAD9411A5F3EA73D6D1A78F72201E53717502988A3F1AD9651DE19A4281729F048C5ECABCF1DBAA6E27C0D470F17109873471BA96E18CDC782EA66010300184C6F526120524620696E746567726174696F6E2074657374";
        const SOURCE_NODE_ID_HEX: &str =
            "3b0af6f3e07bb3aa0e9d3dd11fd0188b64c5102ea0ce5f72a1199cba9c1392a6";

        let payload = hex::decode(CAPTURED_PAYLOAD_HEX).unwrap();
        assert_eq!(
            payload.len(),
            142,
            "captured payload must be exactly 142 bytes"
        );
        assert!(payload[0] == EVENT_CODEC_VERSION, "codec_version must be 1");

        // Size limits this payload must respect (Stage 1's frame constants).
        assert!(payload.len() <= MAX_SECUREMESH_EVENT_PAYLOAD_BYTES);
        assert!(
            43 + payload.len() + 4 <= MAX_SECUREMESH_EVENT_FRAME_BYTES,
            "43(header)+142(payload)+4(crc)=189 must fit under the frame ceiling"
        );

        let source_node_id = decode_fixed::<32>(SOURCE_NODE_ID_HEX, "source node id").unwrap();
        let placeholder_not_a_real_key = "0".repeat(64);

        let event = decode(&payload, &source_node_id, &placeholder_not_a_real_key)
            .expect("the real captured payload must decode through the real codec");

        // `Uuid::to_string()` renders in hyphenated form; the raw 16 bytes on
        // the wire (815ff0674a6a4c8a9788473e00835f9e) are unchanged, just
        // formatted as `Uuid` always formats them.
        assert_eq!(event.event_id, "815ff067-4a6a-4c8a-9788-473e00835f9e");
        assert_eq!(event.origin_seq, 2);
        assert_eq!(event.created_at.timestamp_millis(), 1_790_214_120_951);
        assert_eq!(event.kind, EventKind::IncidentCreated);

        let incident = event.incident_created_payload().unwrap();
        assert_eq!(incident.incident_id, "470f1710-9873-471b-a96e-18cdc782ea66");
        assert_eq!(incident.severity, "LOW");
        assert_eq!(incident.location_source, LocationSource::Unknown);
        assert!(incident.latitude.is_none());
        assert!(incident.longitude.is_none());
        assert!(incident.accuracy_meters.is_none());
        assert!(incident.location_captured_at.is_none());
        assert_eq!(incident.description, "LoRa RF integration test");

        // Exact-length enforcement: one extra byte on the end must be
        // refused as trailing data, not silently ignored. This is the
        // positive proof that all 142 bytes — no more, no less — were
        // consumed to produce the event above.
        let mut with_trailing_byte = payload.clone();
        with_trailing_byte.push(0x00);
        let err = decode(
            &with_trailing_byte,
            &source_node_id,
            &placeholder_not_a_real_key,
        )
        .unwrap_err();
        assert!(err.message().contains("trailing"));

        // No signature verification is performed by this test — `decode`
        // itself never checks one, and this test does not call
        // `MeshEvent::verify`.
    }
}
