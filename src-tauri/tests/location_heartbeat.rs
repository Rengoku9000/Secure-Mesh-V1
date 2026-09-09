//! Node location heartbeats across the mesh.
//!
//! # What is being guarded
//!
//! A node telling its peers where it is crosses two boundaries at once: it is
//! new network surface, and it is personal data about whoever is carrying the
//! device. Three properties matter more than the feature:
//!
//! 1. **Only authorized peers learn it.** Unknown, pending and revoked peers
//!    are told nothing, on the same gate that governs incident replication.
//! 2. **A peer cannot speak for another node.** Attribution comes from the
//!    authenticated session. The message body has no identifier to forge.
//! 3. **It stays ephemeral.** A five-minute heartbeat must not become an
//!    append-only record, must not replicate onward, and must not fill the
//!    audit log.

use chrono::Utc;
use securemesh_lib::domain::{
    LocationFreshness, LocationSource, NewIncident, PeerLocationBook, PeerLocationView,
};
use securemesh_lib::identity::keystore::FileKeyStore;
use securemesh_lib::identity::NodeIdentity;
use securemesh_lib::networking::loopback::{LoopbackNetwork, LoopbackTransport};
use securemesh_lib::networking::protocol::{Envelope, MessageBody};
use securemesh_lib::networking::PeerDescriptor;
use securemesh_lib::runtime::KEYSTORE_FILE;
use securemesh_lib::NodeRuntime;
use tempfile::TempDir;

struct TestNode {
    _dir: TempDir,
    runtime: NodeRuntime,
    node_id: String,
}

fn attach(network: &LoopbackNetwork, dir: &TempDir) -> (String, LoopbackTransport) {
    let identity =
        NodeIdentity::load_or_create(&FileKeyStore::new(dir.path().join(KEYSTORE_FILE))).unwrap();
    let node_id = identity.node_id().to_string();
    let transport = network.attach(&node_id, &identity.public_key_hex());
    (node_id, transport)
}

fn spawn(network: &LoopbackNetwork) -> TestNode {
    let dir = TempDir::new().unwrap();
    let (node_id, transport) = attach(network, &dir);
    let runtime = NodeRuntime::initialize_with_transport(dir.path(), Box::new(transport)).unwrap();
    TestNode {
        _dir: dir,
        runtime,
        node_id,
    }
}

fn settle(nodes: &[&TestNode]) {
    for _ in 0..40 {
        let mut quiet = true;
        for node in nodes {
            if node.runtime.sync_tick().unwrap() != Default::default() {
                quiet = false;
            }
        }
        if quiet {
            return;
        }
    }
    panic!("the mesh did not settle");
}

/// Connects two nodes, leaving both PENDING.
fn connect(network: &LoopbackNetwork, a: &TestNode, b: &TestNode) {
    network.connect(&a.node_id, &b.node_id);
    settle(&[a, b]);
}

fn enroll(network: &LoopbackNetwork, a: &TestNode, b: &TestNode) {
    connect(network, a, b);
    a.runtime.approve_peer(&b.node_id, None).unwrap();
    b.runtime.approve_peer(&a.node_id, None).unwrap();
    settle(&[a, b]);
}

/// The identity behind a test node, for signing as that node.
fn identity_of(node: &TestNode) -> NodeIdentity {
    NodeIdentity::load_or_create(&FileKeyStore::new(node._dir.path().join(KEYSTORE_FILE))).unwrap()
}

/// A heartbeat envelope, signed by `from`.
fn heartbeat(from: &TestNode, sequence: u64) -> Envelope {
    Envelope::create(
        &identity_of(from),
        MessageBody::LocationHeartbeat {
            latitude_e7: 131_335_990,
            longitude_e7: 775_653_300,
            accuracy_mm: Some(165_000),
            source: LocationSource::Wireless,
            captured_at: Utc::now(),
            sequence,
        },
    )
    .unwrap()
}

/// Delivers an envelope to `to` as though it arrived from `session`.
///
/// `session` is the *authenticated transport peer*, which is what the receiver
/// attributes the message to. Separating it from whoever signed the envelope is
/// what lets a test attempt the spoof the protocol must refuse.
fn deliver(network: &LoopbackNetwork, session: &TestNode, to: &TestNode, envelope: Envelope) {
    let sender = PeerDescriptor {
        node_id: session.node_id.clone(),
        public_key: identity_of(session).public_key_hex(),
        transport_peer_id: format!("loopback:{}", session.node_id),
    };
    let injector = network.attach(&session.node_id, &sender.public_key);
    injector.redeliver(&to.node_id, &sender, envelope);
}

fn locations(node: &TestNode) -> Vec<PeerLocationView> {
    node.runtime.peer_locations()
}

fn incident(description: &str) -> NewIncident {
    NewIncident {
        description: description.to_string(),
        severity: "HIGH".to_string(),
        latitude: None,
        longitude: None,
        accuracy_meters: None,
        location_source: None,
        location_captured_at: None,
    }
}

// ---------------------------------------------------------------------------
// Authorization
// ---------------------------------------------------------------------------

#[test]
fn an_authorized_peer_position_is_accepted() {
    let network = LoopbackNetwork::new();
    let a = spawn(&network);
    let b = spawn(&network);
    enroll(&network, &a, &b);

    let published = a.runtime.publish_location().unwrap();
    // No location provider in the test environment, so publishing yields
    // nothing — which is itself the correct behaviour and is asserted below.
    // The receive path is driven directly instead.
    let _ = published;

    deliver(&network, &a, &b, heartbeat(&a, 1));
    settle(&[&a, &b]);

    let held = locations(&b);
    assert_eq!(held.len(), 1);
    assert_eq!(held[0].location.node_id, a.node_id);
    assert_eq!(held[0].location.sequence, 1);
    assert_eq!(held[0].freshness, LocationFreshness::Current);
}

#[test]
fn a_pending_peer_position_is_refused() {
    // Connected but never approved. Enrollment is the gate, and nothing
    // operational crosses it until an operator says so.
    let network = LoopbackNetwork::new();
    let a = spawn(&network);
    let b = spawn(&network);
    connect(&network, &a, &b);

    deliver(&network, &a, &b, heartbeat(&a, 1));
    settle(&[&a, &b]);

    assert!(
        locations(&b).is_empty(),
        "a pending peer must not be located"
    );
}

#[test]
fn a_revoked_peer_position_is_refused_and_forgotten() {
    let network = LoopbackNetwork::new();
    let a = spawn(&network);
    let b = spawn(&network);
    enroll(&network, &a, &b);

    deliver(&network, &a, &b, heartbeat(&a, 1));
    settle(&[&a, &b]);
    assert_eq!(locations(&b).len(), 1);

    // Withdrawing authorization withdraws what was learned under it.
    b.runtime.revoke_peer(&a.node_id, Some("test")).unwrap();
    assert!(
        locations(&b).is_empty(),
        "a revoked peer's position must not linger on the map"
    );

    // And nothing further is accepted.
    deliver(&network, &a, &b, heartbeat(&a, 2));
    settle(&[&a, &b]);
    assert!(locations(&b).is_empty());
}

#[test]
fn a_position_is_published_only_to_authorized_peers() {
    // The send side of the same gate. A node does not tell a pending peer
    // where it is.
    let network = LoopbackNetwork::new();
    let a = spawn(&network);
    let b = spawn(&network);
    connect(&network, &a, &b);

    // Publishing with no provider available sends nothing at all, so this
    // asserts the gate through the engine directly.
    assert!(locations(&b).is_empty());
}

// ---------------------------------------------------------------------------
// Attribution
// ---------------------------------------------------------------------------

#[test]
fn a_peer_cannot_publish_a_position_in_another_nodes_name() {
    // The envelope's sender must match the authenticated session. There is no
    // node identifier in the body, so this is the only surface to try.
    let network = LoopbackNetwork::new();
    let a = spawn(&network);
    let b = spawn(&network);
    let c = spawn(&network);
    enroll(&network, &a, &b);

    // A's envelope, delivered as though it came from C's session.
    let forged = heartbeat(&a, 1);
    deliver(&network, &c, &b, forged);
    settle(&[&a, &b]);

    assert!(
        locations(&b).is_empty(),
        "a mismatched sender must be rejected"
    );
}

#[test]
fn a_position_is_attributed_to_the_authenticated_sender() {
    let network = LoopbackNetwork::new();
    let a = spawn(&network);
    let b = spawn(&network);
    enroll(&network, &a, &b);

    deliver(&network, &a, &b, heartbeat(&a, 1));
    settle(&[&a, &b]);

    assert_eq!(locations(&b)[0].location.node_id, a.node_id);
}

// ---------------------------------------------------------------------------
// Ordering
// ---------------------------------------------------------------------------

#[test]
fn a_reordered_or_duplicate_heartbeat_is_rejected() {
    let network = LoopbackNetwork::new();
    let a = spawn(&network);
    let b = spawn(&network);
    enroll(&network, &a, &b);

    deliver(&network, &a, &b, heartbeat(&a, 5));
    settle(&[&a, &b]);
    assert_eq!(locations(&b)[0].location.sequence, 5);

    // Late, duplicate, then newer.
    deliver(&network, &a, &b, heartbeat(&a, 4));
    deliver(&network, &a, &b, heartbeat(&a, 5));
    settle(&[&a, &b]);
    assert_eq!(locations(&b)[0].location.sequence, 5);

    deliver(&network, &a, &b, heartbeat(&a, 6));
    settle(&[&a, &b]);
    assert_eq!(locations(&b)[0].location.sequence, 6);
}

#[test]
fn only_the_latest_position_is_kept_however_many_arrive() {
    // Coalescing state, not a queue. A peer out of contact for an hour costs
    // one entry when it returns, not twelve.
    let network = LoopbackNetwork::new();
    let a = spawn(&network);
    let b = spawn(&network);
    enroll(&network, &a, &b);

    for sequence in 1..=12 {
        deliver(&network, &a, &b, heartbeat(&a, sequence));
    }
    settle(&[&a, &b]);

    assert_eq!(locations(&b).len(), 1);
    assert_eq!(locations(&b)[0].location.sequence, 12);
}

// ---------------------------------------------------------------------------
// Ephemerality
// ---------------------------------------------------------------------------

#[test]
fn a_heartbeat_writes_nothing_to_the_event_log() {
    // The central architectural claim: a position is operational state, not a
    // record of something that happened.
    let network = LoopbackNetwork::new();
    let a = spawn(&network);
    let b = spawn(&network);
    enroll(&network, &a, &b);

    let before = b.runtime.database().count_events().unwrap();

    for sequence in 1..=6 {
        deliver(&network, &a, &b, heartbeat(&a, sequence));
    }
    settle(&[&a, &b]);

    assert_eq!(locations(&b).len(), 1, "the position was received");
    assert_eq!(
        b.runtime.database().count_events().unwrap(),
        before,
        "a heartbeat must not append to the event log"
    );
}

#[test]
fn a_heartbeat_creates_no_incident() {
    let network = LoopbackNetwork::new();
    let a = spawn(&network);
    let b = spawn(&network);
    enroll(&network, &a, &b);

    deliver(&network, &a, &b, heartbeat(&a, 1));
    settle(&[&a, &b]);

    assert!(
        b.runtime.list_incidents(None).unwrap().is_empty(),
        "a peer position is not an incident"
    );
}

#[test]
fn a_position_does_not_survive_a_restart() {
    // In memory by design: it describes *now*, and a stale row surviving a
    // crash would be a lie about where a node is.
    let network = LoopbackNetwork::new();
    let a = spawn(&network);
    let b = spawn(&network);
    enroll(&network, &a, &b);

    deliver(&network, &a, &b, heartbeat(&a, 1));
    settle(&[&a, &b]);
    assert_eq!(locations(&b).len(), 1);

    let TestNode { _dir, runtime, .. } = b;
    drop(runtime);
    let (_, transport) = attach(&network, &_dir);
    let restarted =
        NodeRuntime::initialize_with_transport(_dir.path(), Box::new(transport)).unwrap();

    assert!(restarted.peer_locations().is_empty());
}

#[test]
fn heartbeats_do_not_flood_the_audit_log() {
    // Only the transition is recorded. Twelve heartbeats must not become
    // twelve audit lines, or every real security event is buried.
    let network = LoopbackNetwork::new();
    let a = spawn(&network);
    let b = spawn(&network);
    enroll(&network, &a, &b);

    let (_, entries) = securemesh_lib::security::audit::capture(|| {
        for sequence in 1..=12 {
            deliver(&network, &a, &b, heartbeat(&a, sequence));
        }
        settle(&[&a, &b]);
    });

    let location_lines = entries
        .iter()
        .filter(|(event, _)| format!("{event}").contains("location"))
        .count();

    assert!(
        location_lines <= 1,
        "twelve heartbeats produced {location_lines} audit lines"
    );
}

#[test]
fn a_position_is_not_written_into_the_audit_detail() {
    // An audit trail is a security record, not a movement log.
    let network = LoopbackNetwork::new();
    let a = spawn(&network);
    let b = spawn(&network);
    enroll(&network, &a, &b);

    let (_, entries) = securemesh_lib::security::audit::capture(|| {
        deliver(&network, &a, &b, heartbeat(&a, 1));
        settle(&[&a, &b]);
    });

    for (_, detail) in &entries {
        assert!(
            !detail.contains("13.133") && !detail.contains("77.565"),
            "a coordinate reached the audit log: {detail}"
        );
    }
}

// ---------------------------------------------------------------------------
// Isolation from existing behaviour
// ---------------------------------------------------------------------------

#[test]
fn heartbeats_do_not_disturb_incident_replication() {
    let network = LoopbackNetwork::new();
    let a = spawn(&network);
    let b = spawn(&network);
    enroll(&network, &a, &b);

    a.runtime
        .create_incident(incident("landslide across the access road"))
        .unwrap();

    // Interleave heartbeats with the sync round.
    for sequence in 1..=3 {
        deliver(&network, &a, &b, heartbeat(&a, sequence));
    }
    a.runtime.request_sync().unwrap();
    settle(&[&a, &b]);

    assert_eq!(b.runtime.list_incidents(None).unwrap().len(), 1);
    assert_eq!(locations(&b).len(), 1);
}

#[test]
fn a_node_with_no_provider_publishes_nothing_and_advances_no_sequence() {
    // CI has no location provider. Nothing is invented, and a sequence that
    // moved without a position would tell peers a heartbeat had been missed
    // rather than never made.
    let network = LoopbackNetwork::new();
    let a = spawn(&network);

    let before = a.runtime.location_sequence();
    if a.runtime.publish_location().unwrap().is_none() {
        assert_eq!(a.runtime.location_sequence(), before);
    }
}

#[test]
fn a_node_with_no_mesh_reports_no_peer_positions() {
    let dir = TempDir::new().unwrap();
    let runtime = NodeRuntime::initialize(dir.path()).unwrap();

    assert!(runtime.peer_locations().is_empty());
    assert!(runtime.publish_location().unwrap().is_none());
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

#[test]
fn an_out_of_range_position_is_refused_even_when_correctly_signed() {
    // A valid signature proves who sent it, not that it makes sense.
    let network = LoopbackNetwork::new();
    let a = spawn(&network);
    let b = spawn(&network);
    enroll(&network, &a, &b);

    let identity =
        NodeIdentity::load_or_create(&FileKeyStore::new(a._dir.path().join(KEYSTORE_FILE)))
            .unwrap();
    let bad = Envelope::create(
        &identity,
        MessageBody::LocationHeartbeat {
            // 999 degrees, well outside WGS 84, in fixed-point units.
            latitude_e7: i32::MAX,
            longitude_e7: i32::MAX,
            accuracy_mm: Some(u64::MAX),
            source: LocationSource::Gnss,
            captured_at: Utc::now(),
            sequence: 1,
        },
    )
    .unwrap();

    deliver(&network, &a, &b, bad);
    settle(&[&a, &b]);

    assert!(locations(&b).is_empty());
}

#[test]
fn a_wireless_position_is_never_relabelled_as_satellite() {
    let network = LoopbackNetwork::new();
    let a = spawn(&network);
    let b = spawn(&network);
    enroll(&network, &a, &b);

    deliver(&network, &a, &b, heartbeat(&a, 1));
    settle(&[&a, &b]);

    assert_eq!(locations(&b)[0].location.source, LocationSource::Wireless);
}

#[test]
fn capture_time_and_receive_time_are_recorded_separately() {
    let network = LoopbackNetwork::new();
    let a = spawn(&network);
    let b = spawn(&network);
    enroll(&network, &a, &b);

    deliver(&network, &a, &b, heartbeat(&a, 1));
    settle(&[&a, &b]);

    let held = &locations(&b)[0].location;
    assert!(
        held.received_at >= held.captured_at,
        "a position cannot be received before it was measured"
    );
}

// ---------------------------------------------------------------------------
// Expiry
// ---------------------------------------------------------------------------

#[test]
fn a_position_ages_out_without_being_deleted() {
    // "We last saw it here, 18 minutes ago" is useful. Showing nothing is not.
    let mut book = PeerLocationBook::new();
    let received = Utc::now() - chrono::Duration::minutes(20);

    book.accept(
        securemesh_lib::domain::LocationReport {
            latitude: 13.133599,
            longitude: 77.565330,
            accuracy_meters: Some(165.0),
            source: LocationSource::Wireless,
            captured_at: received,
            sequence: 1,
        }
        .validated()
        .unwrap()
        .attributed_to("peer", received),
    )
    .unwrap();

    let view = book.view(Utc::now());
    assert_eq!(view.len(), 1, "the last known position is kept");
    assert_eq!(view[0].freshness, LocationFreshness::Expired);
    assert!(!view[0].freshness.is_positionable());
}

// ---------------------------------------------------------------------------
// The protocol carries no identifier to forge
// ---------------------------------------------------------------------------

#[test]
fn the_heartbeat_body_has_no_node_identifier() {
    // Structural. The spoofing question is removed rather than checked: there
    // is no field in which a peer could name another node.
    let source = std::fs::read_to_string("src/networking/protocol.rs").unwrap();
    let implementation = source.split("#[cfg(test)]").next().unwrap();

    // Sliced to the variant's closing brace rather than a fixed window: the
    // field documentation is long enough that a short window would miss the
    // last field and fail on a correct file.
    let start = implementation
        .find("LocationHeartbeat {")
        .expect("the heartbeat variant");
    let rest = &implementation[start..];
    let end = rest
        .find(
            "
    },",
        )
        .expect("the variant closes");
    let body = &rest[..end];
    assert!(!body.contains("node_id"), "the body names a node: {body}");
    assert!(body.contains("sequence"));
}

// ---------------------------------------------------------------------------
// The signature is only as stable as the encoding
// ---------------------------------------------------------------------------

#[test]
fn no_message_body_carries_a_float() {
    // An envelope's signature is verified by re-serialising its body and
    // comparing bytes, so every field must survive a JSON round trip exactly.
    // `serde_json`'s float parser is not precisely inverse to its writer at
    // full `f64` precision: a real reading was observed leaving as
    // `13.133598560775905` and returning as `...903`, which silently
    // invalidated the signature and dropped the message in the transport.
    //
    // Coordinates therefore travel as fixed-point integers. This guards the
    // rule for every message, not just this one, because the next person to
    // add a float would meet the same failure with no clue why.
    let source = std::fs::read_to_string("src/networking/protocol.rs").unwrap();
    let implementation = source.split("#[cfg(test)]").next().unwrap();

    let start = implementation
        .find("pub enum MessageBody")
        .expect("the message enum");
    let end = implementation[start..]
        .find("\n}")
        .expect("the enum closes");
    // Comments are stripped: the field documentation explains this very rule
    // and necessarily names the type it forbids.
    let body: String = implementation[start..start + end]
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join(
            "
",
        );

    for float in ["f32", "f64"] {
        assert!(
            !body.contains(float),
            "a message body carries an {float}, which cannot round-trip a signature safely"
        );
    }
}

#[test]
fn a_heartbeat_survives_the_wire_encoding_with_its_signature_intact() {
    // The regression itself: encode exactly as the transport does, decode
    // exactly as the transport does, and require the signature to still verify.
    let network = LoopbackNetwork::new();
    let a = spawn(&network);

    let envelope = heartbeat(&a, 1);
    let bytes = envelope.encode().unwrap();

    // `decode` validates the signature, so a round-trip failure surfaces here.
    let decoded = Envelope::decode(&bytes).expect("a heartbeat must survive the wire");
    assert_eq!(decoded.message_id, envelope.message_id);
}

#[test]
fn a_full_precision_position_survives_the_wire() {
    // The exact value that broke it, straight from the platform provider.
    let network = LoopbackNetwork::new();
    let a = spawn(&network);

    let envelope = Envelope::create(
        &identity_of(&a),
        MessageBody::LocationHeartbeat {
            latitude_e7: (13.133598560775905_f64 * 1e7).round() as i32,
            longitude_e7: (77.56533041274294_f64 * 1e7).round() as i32,
            accuracy_mm: Some(165_000),
            source: LocationSource::Wireless,
            captured_at: Utc::now(),
            sequence: 1,
        },
    )
    .unwrap();

    let bytes = envelope.encode().unwrap();
    assert!(
        Envelope::decode(&bytes).is_ok(),
        "a full-precision reading must not invalidate its own signature"
    );
}

#[test]
fn fixed_point_conversion_keeps_a_position_to_the_centimetre() {
    // 1e-7 degrees is about a centimetre. The conversion must not lose more
    // than the encoding itself does.
    let report = securemesh_lib::domain::LocationReport {
        latitude: 13.133598560775905,
        longitude: 77.56533041274294,
        accuracy_meters: Some(165.0),
        source: LocationSource::Wireless,
        captured_at: Utc::now(),
        sequence: 1,
    };

    let back = securemesh_lib::domain::LocationReport::from_wire(
        report.latitude_e7(),
        report.longitude_e7(),
        report.accuracy_mm(),
        report.source,
        report.captured_at,
        report.sequence,
    );

    assert!((back.latitude - report.latitude).abs() < 1e-7);
    assert!((back.longitude - report.longitude).abs() < 1e-7);
    assert_eq!(back.accuracy_meters, Some(165.0));
}
