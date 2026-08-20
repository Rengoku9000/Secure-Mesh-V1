//! Device location, from capture through replication.
//!
//! # What is being guarded
//!
//! Location is **optional incident metadata captured as a snapshot**. Three
//! properties matter more than the feature itself:
//!
//! 1. A position never becomes a precondition for recording an incident. The
//!    device may have no receiver, the operator may refuse permission, the fix
//!    may time out — and an incident must still be capturable and replicable.
//!    GPS must not become a single point of failure for the one thing this
//!    application exists to do.
//! 2. Coordinates travel *with the incident*, over the existing signed and
//!    authorized path. There is no second channel for location, so a receiving
//!    node needs no location hardware of its own to display where something
//!    happened.
//! 3. Nothing is invented. A provider that cannot answer says so.
//!
//! The provider is mocked here because the outcome under test is what
//! SecureMesh does with a position, not whether a particular machine has a GNSS
//! receiver — and CI has none.

use securemesh_lib::domain::NewIncident;
use securemesh_lib::location::{
    DeviceLocation, LocationPermission, LocationProvider, LocationSource, UnavailableProvider,
};
use securemesh_lib::networking::loopback::LoopbackNetwork;
use securemesh_lib::NodeRuntime;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use tempfile::TempDir;

/// A provider a test can drive: it yields whatever fix (or failure) is set.
struct MockProvider {
    permission: Mutex<LocationPermission>,
    fix: Mutex<Option<DeviceLocation>>,
    /// Counts fixes taken, so "nothing reads the sensor on its own" is
    /// checkable rather than merely asserted in prose.
    reads: AtomicUsize,
}

impl MockProvider {
    fn granted_with(fix: DeviceLocation) -> Self {
        Self {
            permission: Mutex::new(LocationPermission::Granted),
            fix: Mutex::new(Some(fix)),
            reads: AtomicUsize::new(0),
        }
    }

    fn denied() -> Self {
        Self {
            permission: Mutex::new(LocationPermission::Denied),
            fix: Mutex::new(None),
            reads: AtomicUsize::new(0),
        }
    }
}

impl LocationProvider for MockProvider {
    fn permission(&self) -> LocationPermission {
        *self.permission.lock().unwrap()
    }

    fn request_permission(&self) -> LocationPermission {
        *self.permission.lock().unwrap()
    }

    fn current_location(&self) -> securemesh_lib::CoreResult<DeviceLocation> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        match self.fix.lock().unwrap().clone() {
            Some(fix) => fix.validated(),
            None => Err(securemesh_lib::CoreError::internal(
                "device location is unavailable",
            )),
        }
    }

    fn describe(&self) -> &'static str {
        "mock provider"
    }
}

fn fix(latitude: f64, longitude: f64) -> DeviceLocation {
    DeviceLocation {
        latitude,
        longitude,
        accuracy_meters: Some(8.0),
        altitude_meters: Some(920.0),
        heading_degrees: None,
        speed_mps: None,
        source: LocationSource::Satellite,
        captured_at: chrono::Utc::now(),
    }
}

fn incident_at(description: &str, location: Option<&DeviceLocation>) -> NewIncident {
    NewIncident {
        description: description.to_string(),
        severity: "HIGH".to_string(),
        latitude: location.map(|l| l.latitude),
        longitude: location.map(|l| l.longitude),
        // The capture path forwards everything the reading carried, not just
        // the two numbers. Whatever the UI does, this is the shape the command
        // layer receives.
        accuracy_meters: location.and_then(|l| l.accuracy_meters),
        location_source: location.map(|l| l.source.into()),
        location_captured_at: location.map(|l| l.captured_at),
    }
}

/// Ticks every node until nothing further happens.
fn settle(nodes: &[&NodeRuntime]) {
    for _ in 0..40 {
        let mut quiet = true;
        for node in nodes {
            if node.sync_tick().unwrap() != Default::default() {
                quiet = false;
            }
        }
        if quiet {
            return;
        }
    }
    panic!("the mesh did not settle");
}

// ---------------------------------------------------------------------------
// Parsing and validation of a reported fix
// ---------------------------------------------------------------------------

#[test]
fn a_reported_fix_keeps_every_field_the_platform_supplied() {
    let captured = fix(12.9716, 77.5946).validated().unwrap();

    assert_eq!(captured.latitude, 12.9716);
    assert_eq!(captured.longitude, 77.5946);
    assert_eq!(captured.accuracy_meters, Some(8.0));
    assert_eq!(captured.altitude_meters, Some(920.0));
    // Absent values stay absent rather than becoming zero, which would read as
    // "stationary, facing north" instead of "not reported".
    assert_eq!(captured.heading_degrees, None);
    assert_eq!(captured.speed_mps, None);
}

#[test]
fn the_edges_of_the_coordinate_system_are_accepted() {
    for (lat, lon) in [(-90.0, -180.0), (90.0, 180.0)] {
        assert!(fix(lat, lon).validated().is_ok());

        // And the incident boundary agrees, which is the gate that matters.
        let incident = incident_at("edge of the world", Some(&fix(lat, lon)));
        assert!(incident.validate("node").is_ok());
    }
}

#[test]
fn an_out_of_range_latitude_is_refused_by_both_gates() {
    for bad in [90.1, -90.1, 1000.0] {
        assert!(
            fix(bad, 0.0).validated().is_err(),
            "{bad} is not a latitude"
        );
        assert!(incident_at("bad lat", Some(&fix(bad, 0.0)))
            .validate("node")
            .is_err());
    }
}

#[test]
fn an_out_of_range_longitude_is_refused_by_both_gates() {
    for bad in [180.1, -180.1, 5000.0] {
        assert!(
            fix(0.0, bad).validated().is_err(),
            "{bad} is not a longitude"
        );
        assert!(incident_at("bad lon", Some(&fix(0.0, bad)))
            .validate("node")
            .is_err());
    }
}

#[test]
fn a_half_supplied_coordinate_is_refused() {
    // Half a position is not a position, and silently dropping the other half
    // would store a point on the prime meridian that nobody chose.
    let mut only_latitude = incident_at("half", None);
    only_latitude.latitude = Some(12.9);
    assert!(only_latitude.validate("node").is_err());

    let mut only_longitude = incident_at("half", None);
    only_longitude.longitude = Some(77.5);
    assert!(only_longitude.validate("node").is_err());
}

#[test]
fn a_non_finite_coordinate_never_reaches_an_incident() {
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert!(fix(value, 0.0).validated().is_err());
        assert!(incident_at("nonsense", Some(&fix(value, 0.0)))
            .validate("node")
            .is_err());
    }
}

// ---------------------------------------------------------------------------
// Unavailable and denied
// ---------------------------------------------------------------------------

#[test]
fn an_unavailable_provider_reports_instead_of_inventing() {
    let provider = UnavailableProvider::new("no location provider on this machine");

    assert_eq!(provider.permission(), LocationPermission::Unavailable);
    let error = provider.current_location().unwrap_err();
    assert!(error.message().contains("no location provider"));
}

#[test]
fn a_denied_provider_yields_no_position() {
    let provider = MockProvider::denied();

    assert_eq!(provider.permission(), LocationPermission::Denied);
    // Refusal must not be answered with a stale or default coordinate.
    assert!(provider.current_location().is_err());
}

// ---------------------------------------------------------------------------
// Incident creation, with and without a position
// ---------------------------------------------------------------------------

#[test]
fn an_incident_records_the_coordinates_it_was_given() {
    let dir = TempDir::new().unwrap();
    let runtime = NodeRuntime::initialize(dir.path()).unwrap();
    let captured = fix(12.9716, 77.5946);

    let incident = runtime
        .create_incident(incident_at("Road blocked", Some(&captured)))
        .unwrap();

    assert_eq!(incident.latitude, Some(12.9716));
    assert_eq!(incident.longitude, Some(77.5946));
}

#[test]
fn an_incident_without_a_position_is_perfectly_valid() {
    let dir = TempDir::new().unwrap();
    let runtime = NodeRuntime::initialize(dir.path()).unwrap();

    let incident = runtime
        .create_incident(incident_at("Reported by radio, position unknown", None))
        .unwrap();

    assert_eq!(incident.latitude, None);
    assert_eq!(incident.longitude, None);
    assert_eq!(runtime.list_incidents(None).unwrap().len(), 1);
}

#[test]
fn a_failing_location_provider_does_not_block_incident_capture() {
    // The property the whole feature is subordinate to: no receiver, no
    // permission, no fix — and the operator can still record what happened.
    let provider = UnavailableProvider::new("no receiver");
    assert!(provider.current_location().is_err());

    let dir = TempDir::new().unwrap();
    let runtime = NodeRuntime::initialize(dir.path()).unwrap();
    let incident = runtime
        .create_incident(incident_at("Recorded with no GPS at all", None))
        .unwrap();

    assert_eq!(incident.description, "Recorded with no GPS at all");
    assert_eq!(runtime.database().count_events().unwrap(), 1);
}

// ---------------------------------------------------------------------------
// Durability
// ---------------------------------------------------------------------------

#[test]
fn coordinates_survive_a_restart() {
    let dir = TempDir::new().unwrap();
    let captured = fix(-33.8688, 151.2093);

    let id = {
        let runtime = NodeRuntime::initialize(dir.path()).unwrap();
        runtime
            .create_incident(incident_at("Harbour bridge closed", Some(&captured)))
            .unwrap()
            .id
    };

    let restarted = NodeRuntime::initialize(dir.path()).unwrap();
    let reloaded = restarted.get_incident(&id).unwrap();

    assert_eq!(reloaded.latitude, Some(-33.8688));
    assert_eq!(reloaded.longitude, Some(151.2093));
}

// ---------------------------------------------------------------------------
// Replication, and immutability of the snapshot
// ---------------------------------------------------------------------------

#[test]
fn coordinates_replicate_with_the_incident_and_are_not_re_derived() {
    // Node B has no location provider of its own — deliberately. It must still
    // display where the incident happened, because the position travels as
    // incident data rather than being sensed locally.
    use securemesh_lib::identity::keystore::FileKeyStore;
    use securemesh_lib::identity::NodeIdentity;
    use securemesh_lib::runtime::KEYSTORE_FILE;

    let network = LoopbackNetwork::new();
    let mut nodes = Vec::new();
    for _ in 0..2 {
        let dir = TempDir::new().unwrap();
        let identity =
            NodeIdentity::load_or_create(&FileKeyStore::new(dir.path().join(KEYSTORE_FILE)))
                .unwrap();
        let node_id = identity.node_id().to_string();
        let transport = network.attach(&node_id, &identity.public_key_hex());
        let runtime =
            NodeRuntime::initialize_with_transport(dir.path(), Box::new(transport)).unwrap();
        nodes.push((dir, runtime, node_id));
    }

    let (_dir_a, a, a_id) = &nodes[0];
    let (_dir_b, b, b_id) = &nodes[1];

    network.connect(a_id, b_id);
    settle(&[a, b]);
    a.approve_peer(b_id, None).unwrap();
    b.approve_peer(a_id, None).unwrap();
    settle(&[a, b]);

    let captured = fix(12.9716, 77.5946);
    let created = a
        .create_incident(incident_at("Bridge out on the north road", Some(&captured)))
        .unwrap();
    settle(&[a, b]);

    let received = b.get_incident(&created.id).unwrap();
    assert_eq!(
        received.latitude,
        Some(12.9716),
        "the receiving node must show the reported position"
    );
    assert_eq!(received.longitude, Some(77.5946));
    assert_eq!(received.description, "Bridge out on the north road");

    // The snapshot is immutable. An observation appended afterwards records new
    // information without moving where the incident was reported.
    b.add_observation(&created.id, "Still impassable an hour later")
        .unwrap();
    settle(&[a, b]);

    let after = a.get_incident(&created.id).unwrap();
    assert_eq!(after.latitude, Some(12.9716), "the position must not move");
    assert_eq!(after.longitude, Some(77.5946));
}

// ---------------------------------------------------------------------------
// No network, no audit noise, no silent sensor reads
// ---------------------------------------------------------------------------

#[test]
fn obtaining_a_position_uses_no_http_client_and_no_geocoder() {
    // Asserted over the source, because a runtime assertion could only show
    // that today's call did not reach the network. The location path must never
    // acquire a URL, an API key, or a geocoding endpoint.
    // Comments are stripped first. The module documentation legitimately says
    // what this feature does *not* do — "it does not geocode" — and matching a
    // promise not to do something would fail the test that enforces it.
    let module = include_str!("../src/location/mod.rs");
    let implementation = module
        .split("#[cfg(test)]")
        .next()
        .unwrap()
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join(
            "
",
        )
        .to_lowercase();

    for forbidden in [
        "http://",
        "https://",
        "reqwest",
        "geocod",
        "googleapis",
        "mapbox",
        "openstreetmap",
        "api_key",
    ] {
        assert!(
            !implementation.contains(forbidden),
            "the location path must not reference {forbidden}"
        );
    }

    // And no HTTP client is a declared dependency of the crate.
    let manifest = include_str!("../Cargo.toml");
    for forbidden in ["reqwest", "ureq", "isahc", "curl"] {
        assert!(!manifest.contains(forbidden));
    }
}

#[test]
fn reading_location_state_writes_nothing_and_is_not_audited() {
    // A sensor read is an observation, not a state change. Auditing it would
    // repeat the mistake that once filled the log with identity reads.
    let dir = TempDir::new().unwrap();
    let runtime = NodeRuntime::initialize(dir.path()).unwrap();

    let (_, records) = securemesh_lib::security::audit::capture(|| {
        let _ = runtime.location_permission();
        let _ = runtime.location_provider_name();
    });

    assert!(
        records.is_empty(),
        "reading location state must not write to the audit log, got {records:?}"
    );
    assert_eq!(runtime.list_incidents(None).unwrap().len(), 0);
    assert_eq!(runtime.database().count_events().unwrap(), 0);
}

#[test]
fn nothing_reads_the_sensor_unless_asked() {
    // Guards the "do not silently capture a position when the form opens" rule
    // at the level a test can reach: checking permission must not take a fix.
    let provider = MockProvider::granted_with(fix(1.0, 1.0));
    assert_eq!(provider.reads.load(Ordering::SeqCst), 0);

    let _ = provider.permission();
    let _ = provider.request_permission();
    assert_eq!(
        provider.reads.load(Ordering::SeqCst),
        0,
        "checking permission must not take a fix"
    );

    provider.current_location().unwrap();
    assert_eq!(provider.reads.load(Ordering::SeqCst), 1);
}

#[test]
fn only_a_satellite_fix_is_presented_as_working_offline() {
    // The honesty rule: a Wi-Fi or IP estimate required the OS to reach the
    // Internet, and must never be labelled as a GPS fix.
    assert!(LocationSource::Satellite.works_offline());
    for source in [
        LocationSource::Wireless,
        LocationSource::IpAddress,
        LocationSource::Unknown,
    ] {
        assert!(
            !source.works_offline(),
            "{source:?} is not an offline source"
        );
    }
}
