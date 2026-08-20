//! Location provenance: accuracy, source, and capture time.
//!
//! # What is being guarded
//!
//! A coordinate on its own is not actionable. "13.133599, 77.565330" is the
//! same two numbers whether it came off a satellite and is good to five metres
//! or came from an IP lookup and is good to fifty kilometres. A responder
//! deciding whether to drive somewhere needs to tell those apart, and before
//! this the receiving node could not: accuracy and source existed only on the
//! machine that took the reading.
//!
//! Three properties are under test:
//!
//! 1. **Provenance is part of the authoritative record.** It lives inside the
//!    signed incident event, not beside it, so it replicates on the existing
//!    path and cannot be altered without breaking the signature. There is no
//!    second location event and no second channel.
//! 2. **Nothing is invented.** An incident with no measurement reads as no
//!    measurement — never as a perfect one, never as zero, and never with a
//!    capture time borrowed from when the record happened to be filed.
//! 3. **The same gate applies to peers.** A valid signature proves who wrote a
//!    payload, not that the payload is sane. A remote node cannot store an
//!    accuracy figure that a local operator would have been refused.

use chrono::{Duration, Utc};
use securemesh_lib::domain::{Incident, Location, LocationSource, NewIncident};
use securemesh_lib::identity::keystore::FileKeyStore;
use securemesh_lib::identity::NodeIdentity;
use securemesh_lib::networking::loopback::{LoopbackNetwork, LoopbackTransport};
use securemesh_lib::runtime::{DATABASE_FILE, KEYSTORE_FILE};
use securemesh_lib::NodeRuntime;
use tempfile::TempDir;

const NODE: &str = "a7f32c9e00000000000000000000000000000000000000000000000000000000";

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

struct TestNode {
    dir: TempDir,
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
        dir,
        runtime,
        node_id,
    }
}

fn restart(network: &LoopbackNetwork, node: TestNode) -> TestNode {
    let TestNode { dir, runtime, .. } = node;
    drop(runtime);
    let (node_id, transport) = attach(network, &dir);
    let runtime = NodeRuntime::initialize_with_transport(dir.path(), Box::new(transport)).unwrap();
    TestNode {
        dir,
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

fn connect_and_enroll(network: &LoopbackNetwork, a: &TestNode, b: &TestNode) {
    network.connect(&a.node_id, &b.node_id);
    settle(&[a, b]);
    a.runtime.approve_peer(&b.node_id, None).unwrap();
    b.runtime.approve_peer(&a.node_id, None).unwrap();
    a.runtime.request_sync().unwrap();
    b.runtime.request_sync().unwrap();
    settle(&[a, b]);
}

/// A fully described reading, as the capture path produces one.
fn measured() -> NewIncident {
    NewIncident {
        description: "landslide across the access road".to_string(),
        severity: "HIGH".to_string(),
        latitude: Some(13.133599),
        longitude: Some(77.565330),
        accuracy_meters: Some(4.5),
        location_source: Some(LocationSource::Gnss),
        location_captured_at: Some(Utc::now() - Duration::seconds(90)),
    }
}

/// An incident with no position at all.
fn unlocated() -> NewIncident {
    NewIncident {
        description: "radio check".to_string(),
        severity: "LOW".to_string(),
        latitude: None,
        longitude: None,
        accuracy_meters: None,
        location_source: None,
        location_captured_at: None,
    }
}

fn only(incidents: Vec<Incident>, description: &str) -> Incident {
    incidents
        .into_iter()
        .find(|incident| incident.description == description)
        .expect("incident not found")
}

// ---------------------------------------------------------------------------
// The record keeps what was measured
// ---------------------------------------------------------------------------

#[test]
fn accuracy_source_and_capture_time_survive_validation() {
    let incident = measured().validate(NODE).unwrap();

    assert_eq!(incident.accuracy_meters, Some(4.5));
    assert_eq!(incident.location_source, LocationSource::Gnss);
    assert!(incident.location_captured_at.is_some());
}

#[test]
fn provenance_is_persisted_and_survives_a_restart() {
    let network = LoopbackNetwork::new();
    let node = spawn(&network);
    let created = node.runtime.create_incident(measured()).unwrap();

    // Read back through the projection, not the validated value: this is what
    // the database actually holds.
    assert_eq!(created.accuracy_meters, Some(4.5));
    assert_eq!(created.location_source, LocationSource::Gnss);
    let captured = created.location_captured_at.expect("capture time recorded");

    let node = restart(&network, node);
    let reloaded = only(
        node.runtime.list_incidents(None).unwrap(),
        &created.description,
    );

    assert_eq!(reloaded.accuracy_meters, Some(4.5));
    assert_eq!(reloaded.location_source, LocationSource::Gnss);
    assert_eq!(reloaded.location_captured_at, Some(captured));
}

#[test]
fn capture_time_is_recorded_separately_from_when_the_incident_was_filed() {
    let network = LoopbackNetwork::new();
    let node = spawn(&network);
    let created = node.runtime.create_incident(measured()).unwrap();

    let captured = created.location_captured_at.unwrap();
    // The fix in `measured()` is 90 seconds old. If the two timestamps were
    // conflated, a stale position attached to a fresh incident would be
    // undetectable — which is the failure this field exists to expose.
    assert!(
        created.created_at - captured >= Duration::seconds(60),
        "capture time was overwritten with the filing time"
    );
}

// ---------------------------------------------------------------------------
// Nothing is invented
// ---------------------------------------------------------------------------

#[test]
fn an_incident_without_coordinates_carries_no_provenance() {
    let incident = unlocated().validate(NODE).unwrap();

    assert_eq!(incident.latitude, None);
    assert_eq!(incident.accuracy_meters, None);
    assert_eq!(incident.location_captured_at, None);
    // Unknown, not a fabricated reading — and `location()` reports the whole
    // position as absent rather than as a partially filled one.
    assert_eq!(incident.location_source, LocationSource::Unknown);
    assert!(incident.location().is_none());
}

#[test]
fn provenance_without_coordinates_is_refused() {
    for mutate in [
        (|input: &mut NewIncident| input.accuracy_meters = Some(10.0)) as fn(&mut NewIncident),
        |input: &mut NewIncident| input.location_source = Some(LocationSource::Gnss),
        |input: &mut NewIncident| input.location_captured_at = Some(Utc::now()),
    ] {
        let mut input = unlocated();
        mutate(&mut input);
        let error = input.validate(NODE).unwrap_err();
        assert_eq!(error.code(), "VALIDATION_ERROR");
        assert!(
            error.message().contains("requires coordinates"),
            "unexpected message: {}",
            error.message()
        );
    }
}

#[test]
fn a_reading_that_reported_no_accuracy_stays_unmeasured() {
    let mut input = measured();
    input.accuracy_meters = None;
    let incident = input.validate(NODE).unwrap();

    // Absent, never zero. Zero would claim a perfect fix.
    assert_eq!(incident.accuracy_meters, None);
    assert_eq!(incident.location_source, LocationSource::Gnss);
}

#[test]
fn a_hand_entered_coordinate_reads_as_unknown_provenance() {
    let mut input = measured();
    input.accuracy_meters = None;
    input.location_source = None;
    input.location_captured_at = None;

    let incident = input.validate(NODE).unwrap();

    assert_eq!(incident.latitude, Some(13.133599));
    assert_eq!(incident.location_source, LocationSource::Unknown);
    assert_eq!(incident.location_captured_at, None);
}

// ---------------------------------------------------------------------------
// Accuracy is validated, not merely stored
// ---------------------------------------------------------------------------

#[test]
fn a_negative_accuracy_is_refused_rather_than_silently_dropped() {
    let mut input = measured();
    input.accuracy_meters = Some(-1.0);

    let error = input.validate(NODE).unwrap_err();
    assert_eq!(error.code(), "VALIDATION_ERROR");
    // Dropping the figure would present the coordinate as better attested than
    // the sender claimed it was.
    assert!(error.message().contains("negative"));
}

#[test]
fn a_non_finite_accuracy_is_refused_without_panicking() {
    for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let mut input = measured();
        input.accuracy_meters = Some(bad);
        let error = input.validate(NODE).unwrap_err();
        assert_eq!(error.code(), "VALIDATION_ERROR");
    }
}

#[test]
fn an_implausibly_large_accuracy_is_refused() {
    let mut input = measured();
    input.accuracy_meters = Some(1.0e12);
    assert!(input.validate(NODE).is_err());

    // But a genuinely poor fix is kept intact and shown as poor, rather than
    // being filtered out for looking bad.
    let mut coarse = measured();
    coarse.accuracy_meters = Some(50_000.0);
    coarse.location_source = Some(LocationSource::Unknown);
    assert_eq!(
        coarse.validate(NODE).unwrap().accuracy_meters,
        Some(50_000.0)
    );
}

#[test]
fn a_zero_accuracy_is_accepted_as_reported() {
    let mut input = measured();
    input.accuracy_meters = Some(0.0);
    assert_eq!(input.validate(NODE).unwrap().accuracy_meters, Some(0.0));
}

// ---------------------------------------------------------------------------
// Vocabulary
// ---------------------------------------------------------------------------

#[test]
fn only_satellite_positioning_is_described_as_working_offline() {
    assert!(LocationSource::Gnss.works_offline());
    // Wireless positioning needs the operating system to reach a lookup
    // service. Calling it offline capability would be a claim this project
    // cannot support.
    assert!(!LocationSource::Wireless.works_offline());
    assert!(!LocationSource::Unknown.works_offline());
}

#[test]
fn the_platform_and_record_vocabularies_agree_on_every_variant() {
    use securemesh_lib::location::LocationSource as Device;

    for device in [
        Device::Satellite,
        Device::Wireless,
        Device::IpAddress,
        Device::Unknown,
    ] {
        let via_conversion: LocationSource = device.into();

        // The JSON path goes through serde aliases instead of the `From` impl.
        // If the two tables ever drift, this fails.
        let label = serde_json::to_value(device).unwrap();
        let via_parse: LocationSource = label.as_str().unwrap().parse().unwrap();

        assert_eq!(via_conversion, via_parse, "{device:?} maps inconsistently");
    }

    // An IP lookup is a city-sized guess. It must not land in the same bucket
    // as a Wi-Fi fix two orders of magnitude better.
    assert_eq!(
        LocationSource::from(Device::IpAddress),
        LocationSource::Unknown
    );
    assert_eq!(
        LocationSource::from(Device::Satellite),
        LocationSource::Gnss
    );
}

#[test]
fn an_unrecognised_source_label_is_refused() {
    for bad in ["GPS", "gnss-ish", "", "SATELLITE_MAYBE", "0"] {
        assert!(
            bad.parse::<LocationSource>().is_err(),
            "{bad} should not parse"
        );
    }
}

// ---------------------------------------------------------------------------
// Replication
// ---------------------------------------------------------------------------

#[test]
fn provenance_replicates_inside_the_signed_incident_event() {
    let network = LoopbackNetwork::new();
    let alice = spawn(&network);
    let bob = spawn(&network);
    connect_and_enroll(&network, &alice, &bob);

    let created = alice.runtime.create_incident(measured()).unwrap();
    alice.runtime.request_sync().unwrap();
    settle(&[&alice, &bob]);

    let received = only(
        bob.runtime.list_incidents(None).unwrap(),
        &created.description,
    );

    // Bob has no location hardware in this test and never took a reading. Every
    // field arrived with the incident, on the one authorized path.
    assert_eq!(received.latitude, created.latitude);
    assert_eq!(received.accuracy_meters, Some(4.5));
    assert_eq!(received.location_source, LocationSource::Gnss);
    assert_eq!(received.location_captured_at, created.location_captured_at);
}

#[test]
fn provenance_is_covered_by_the_event_signature() {
    let network = LoopbackNetwork::new();
    let alice = spawn(&network);
    let created = alice.runtime.create_incident(measured()).unwrap();

    let origin = alice.runtime.public_identity().node_id;
    let events = alice
        .runtime
        .database()
        .events_since(&origin, 0, 10)
        .unwrap();
    let event = events
        .iter()
        .find(|event| event.payload.contains(&created.id))
        .expect("the incident event");

    assert!(event.verify().is_ok());
    assert!(
        event.payload.contains("accuracyMeters"),
        "provenance is not in the signed payload: {}",
        event.payload
    );

    // Rewriting the accuracy is exactly the attack this guards: a coordinate
    // that claims to be far better attested than it is.
    let mut tampered = event.clone();
    tampered.payload = tampered.payload.replace("4.5", "0.5");
    assert!(tampered.verify().is_err());
}

#[test]
fn a_peer_cannot_replicate_an_accuracy_a_local_operator_would_be_refused() {
    // The apply path rebuilds the position through the same constructor the
    // command path uses, so a signed-but-nonsensical payload is rejected at the
    // point of storage rather than being trusted because it verified.
    let honest = Location::new(
        13.133599,
        77.565330,
        Some(4.5),
        LocationSource::Gnss,
        Some(Utc::now()),
    );
    assert!(honest.is_ok());

    for bad_accuracy in [-1.0, f64::NAN, f64::INFINITY, 1.0e12] {
        assert!(
            Location::new(
                13.133599,
                77.565330,
                Some(bad_accuracy),
                LocationSource::Gnss,
                None,
            )
            .is_err(),
            "{bad_accuracy} should be refused on the replication path too"
        );
    }
}

// ---------------------------------------------------------------------------
// Existing data
// ---------------------------------------------------------------------------

#[test]
fn an_incident_written_before_this_field_existed_reads_as_unknown() {
    let network = LoopbackNetwork::new();
    let node = spawn(&network);
    let existing = node.runtime.create_incident(unlocated()).unwrap();
    let description = existing.description.clone();

    // A row from before migration 005 has coordinates and nothing in the three
    // new columns. Writing NULLs directly reproduces that state exactly, which
    // no public API can do — which is the point of reaching past it here.
    let TestNode { dir, runtime, .. } = node;
    drop(runtime);
    rusqlite::Connection::open(dir.path().join(DATABASE_FILE))
        .unwrap()
        .execute(
            "UPDATE incidents
                SET latitude = 12.9716, longitude = 77.5946,
                    accuracy_meters = NULL,
                    location_source = NULL,
                    location_captured_at = NULL
              WHERE id = ?1",
            [&existing.id],
        )
        .unwrap();

    let (_, transport) = attach(&network, &dir);
    let runtime = NodeRuntime::initialize_with_transport(dir.path(), Box::new(transport)).unwrap();
    let reloaded = only(runtime.list_incidents(None).unwrap(), &description);

    assert_eq!(reloaded.latitude, Some(12.9716));
    assert_eq!(reloaded.accuracy_meters, None);
    assert_eq!(reloaded.location_source, LocationSource::Unknown);
    // Not backfilled from `created_at`. The filing time is not a measurement
    // time, and inventing one would put a fabricated figure in a real record.
    assert_eq!(reloaded.location_captured_at, None);
}

#[test]
fn an_event_written_before_this_field_existed_still_verifies_and_applies() {
    use securemesh_lib::domain::{EventKind, MeshEvent};

    let dir = TempDir::new().unwrap();
    let identity =
        NodeIdentity::load_or_create(&FileKeyStore::new(dir.path().join(KEYSTORE_FILE))).unwrap();

    // The old payload shape: coordinates, no provenance. Signed over exactly
    // these bytes, as an older build would have produced them.
    let event = MeshEvent::create(
        &identity,
        1,
        EventKind::IncidentCreated,
        serde_json::json!({
            "incidentId": "11111111-1111-4111-8111-111111111111",
            "description": "recorded by an older build",
            "severity": "HIGH",
            "latitude": 12.9716,
            "longitude": 77.5946,
        }),
    )
    .unwrap();

    assert!(event.verify().is_ok());

    let payload = event.incident_created_payload().unwrap();
    assert_eq!(payload.latitude, Some(12.9716));
    assert_eq!(payload.accuracy_meters, None);
    assert_eq!(payload.location_source, LocationSource::Unknown);
    assert_eq!(payload.location_captured_at, None);
}

// ---------------------------------------------------------------------------
// Boundaries this feature must not cross
// ---------------------------------------------------------------------------

#[test]
fn no_second_location_channel_or_endpoint_was_introduced() {
    let sources = ["src/networking/protocol.rs", "src/commands/mod.rs"];

    for path in sources {
        let text = std::fs::read_to_string(path).unwrap();
        let code: String = text
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");

        for forbidden in ["LocationUpdate", "location_update", "PositionReport"] {
            assert!(
                !code.contains(forbidden),
                "{path} introduces a separate location message: {forbidden}"
            );
        }
    }
}

#[test]
fn coordinates_are_not_written_into_the_audit_log() {
    let (_, entries) = securemesh_lib::security::audit::capture(|| {
        let network = LoopbackNetwork::new();
        let node = spawn(&network);
        node.runtime.create_incident(measured()).unwrap();
    });

    assert!(!entries.is_empty(), "incident creation should be audited");
    for (_, entry) in &entries {
        // The audit log is a security record, not a movement log. Recording a
        // position on every incident would build a track of where the operator
        // has been, which is a worse disclosure than anything it would prove.
        assert!(
            !entry.contains("13.133") && !entry.contains("77.565"),
            "the audit log leaked a coordinate: {entry}"
        );
        assert!(
            !entry.contains("4.5"),
            "the audit log leaked accuracy: {entry}"
        );
    }
}

#[test]
fn upgrading_a_database_that_predates_the_columns_keeps_its_incidents() {
    let network = LoopbackNetwork::new();
    let node = spawn(&network);
    let existing = node
        .runtime
        .create_incident(NewIncident {
            latitude: Some(12.9716),
            longitude: Some(77.5946),
            accuracy_meters: None,
            location_source: None,
            location_captured_at: None,
            ..unlocated()
        })
        .unwrap();
    let description = existing.description.clone();

    // Wind the database back to what a build before this change would have
    // left behind: no provenance columns, schema version 4.
    let TestNode { dir, runtime, .. } = node;
    drop(runtime);
    {
        let conn = rusqlite::Connection::open(dir.path().join(DATABASE_FILE)).unwrap();
        for column in ["accuracy_meters", "location_source", "location_captured_at"] {
            conn.execute_batch(&format!("ALTER TABLE incidents DROP COLUMN {column}"))
                .unwrap();
        }
        conn.pragma_update(None, "user_version", 4).unwrap();
    }

    // Opening the node runs migration 005 against real pre-existing data.
    let (_, transport) = attach(&network, &dir);
    let runtime = NodeRuntime::initialize_with_transport(dir.path(), Box::new(transport)).unwrap();

    let migrated = only(runtime.list_incidents(None).unwrap(), &description);
    assert_eq!(migrated.latitude, Some(12.9716));
    assert_eq!(migrated.longitude, Some(77.5946));
    // The upgrade adds no information it does not have. Anything else would be
    // a fabricated measurement sitting in a real record.
    assert_eq!(migrated.accuracy_meters, None);
    assert_eq!(migrated.location_source, LocationSource::Unknown);
    assert_eq!(migrated.location_captured_at, None);

    // And the upgraded node can still write a fully described incident.
    let fresh = runtime.create_incident(measured()).unwrap();
    assert_eq!(fresh.accuracy_meters, Some(4.5));
    assert_eq!(fresh.location_source, LocationSource::Gnss);
}
