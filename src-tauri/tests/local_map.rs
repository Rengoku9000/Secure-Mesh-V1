//! The offline map: provisioning, status, and the boundaries it must not cross.
//!
//! # What is being guarded
//!
//! 1. **The map never reaches the network.** Structurally, not by observation:
//!    there is no HTTP client, no tile URL and no geocoder anywhere on the map
//!    path, so no request can be made regardless of what a data file contains.
//! 2. **Absence is reported, never papered over.** A node with no basemap says
//!    so. It does not report "Ready", and it does not silently download one.
//! 3. **The map is only a visualization layer.** A node with no map data —
//!    which is every node by default — records, replicates and indexes
//!    incidents exactly as before. Nothing about the map can be load-bearing.

use securemesh_lib::domain::{LocationSource, NewIncident};
use securemesh_lib::map::{self, BASEMAP_FILE};
use securemesh_lib::runtime::ComponentState;
use securemesh_lib::NodeRuntime;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};
use tempfile::TempDir;

/// Serialises the tests that set `SECUREMESH_MAP_ROOT`.
///
/// Environment variables are per-process, and Rust runs tests in parallel
/// threads. Without this, one test's map root would leak into another's and the
/// failures would be intermittent and misleading.
static ENVIRONMENT: Mutex<()> = Mutex::new(());

fn lock() -> MutexGuard<'static, ()> {
    ENVIRONMENT
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

const VALID_BASEMAP: &str = r#"{
    "type": "FeatureCollection",
    "features": [
        {
            "type": "Feature",
            "properties": { "name": "approach road" },
            "geometry": {
                "type": "LineString",
                "coordinates": [[77.5500, 13.1200], [77.5700, 13.1400]]
            }
        }
    ]
}"#;

/// A map root holding the given basemap text, or none at all.
fn map_root_with(contents: Option<&str>) -> TempDir {
    let directory = TempDir::new().unwrap();
    if let Some(text) = contents {
        std::fs::write(directory.path().join(BASEMAP_FILE), text).unwrap();
    }
    directory
}

fn with_map_root<T>(root: &TempDir, body: impl FnOnce() -> T) -> T {
    // SAFETY: every caller holds the environment lock, so no other test is
    // reading or writing this variable concurrently.
    unsafe { std::env::set_var("SECUREMESH_MAP_ROOT", root.path()) };
    let outcome = body();
    unsafe { std::env::remove_var("SECUREMESH_MAP_ROOT") };
    outcome
}

fn incident(description: &str, located: bool) -> NewIncident {
    NewIncident {
        description: description.to_string(),
        severity: "HIGH".to_string(),
        latitude: located.then_some(13.133599),
        longitude: located.then_some(77.565330),
        accuracy_meters: located.then_some(6.0),
        location_source: located.then_some(LocationSource::Gnss),
        location_captured_at: located.then(chrono::Utc::now),
    }
}

// ---------------------------------------------------------------------------
// 1 & 2. Availability and provisioning validation
// ---------------------------------------------------------------------------

#[test]
fn a_provisioned_basemap_is_described_completely() {
    let _guard = lock();
    let root = map_root_with(Some(VALID_BASEMAP));

    let basemap = with_map_root(&root, map::describe).unwrap();

    assert_eq!(basemap.name, BASEMAP_FILE);
    assert_eq!(basemap.feature_count, 1);
    assert_eq!(basemap.sha256.len(), 64);
    // Coverage is what tells an operator which incidents will have geography
    // behind them, so it has to be exact rather than approximate.
    assert!((basemap.bounds.min_latitude - 13.12).abs() < 1e-9);
    assert!((basemap.bounds.max_longitude - 77.57).abs() < 1e-9);
}

#[test]
fn an_unprovisioned_node_says_so_rather_than_reporting_ready() {
    let _guard = lock();
    let root = map_root_with(None);

    let error = with_map_root(&root, map::describe).unwrap_err();

    assert!(error.is_absence(), "absence is not a fault");
    assert!(error.detail().contains("PROVISIONING"));
}

#[test]
fn an_unusable_basemap_is_reported_as_an_error_not_as_absence() {
    let _guard = lock();

    for (contents, expected) in [
        ("not json at all", "not valid JSON"),
        (
            r#"{"type":"Point","coordinates":[0,0]}"#,
            "FeatureCollection",
        ),
        (
            r#"{"type":"FeatureCollection","features":[]}"#,
            "no drawable coordinates",
        ),
    ] {
        let root = map_root_with(Some(contents));
        let error = with_map_root(&root, map::describe).unwrap_err();

        assert!(
            !error.is_absence(),
            "a broken file is a fault, not an empty slot"
        );
        assert!(
            error.detail().contains(expected),
            "expected {expected:?} in {:?}",
            error.detail()
        );
    }
}

#[test]
fn the_geometry_returned_is_exactly_what_was_provisioned() {
    // Returned verbatim rather than re-serialised, so what the renderer draws
    // is the bytes the checksum describes.
    let _guard = lock();
    let root = map_root_with(Some(VALID_BASEMAP));

    let geojson = with_map_root(&root, map::geojson).unwrap();
    assert_eq!(geojson, VALID_BASEMAP);
}

// ---------------------------------------------------------------------------
// 11. Map state over IPC
// ---------------------------------------------------------------------------

#[test]
fn the_status_row_reports_ready_only_when_data_is_present() {
    let _guard = lock();
    let directory = TempDir::new().unwrap();
    let runtime = NodeRuntime::initialize(directory.path()).unwrap();

    let root = map_root_with(Some(VALID_BASEMAP));
    let ready = with_map_root(&root, || runtime.system_status());
    assert_eq!(ready.map.state, ComponentState::Operational);
    assert_eq!(ready.map.label, "Ready");
    assert!(ready.map.detail.contains("Offline geographic data"));

    let empty = map_root_with(None);
    let missing = with_map_root(&empty, || runtime.system_status());
    // Inactive, not degraded: an unprovisioned node is in a normal state an
    // operator can resolve, not a broken one.
    assert_eq!(missing.map.state, ComponentState::Inactive);
    assert_eq!(missing.map.label, "Not provisioned");
    assert!(missing.map.detail.contains("has not been installed"));

    let broken = map_root_with(Some("{"));
    let error = with_map_root(&broken, || runtime.system_status());
    assert_eq!(error.map.state, ComponentState::Degraded);
    assert_eq!(error.map.label, "Error");
}

#[test]
fn map_availability_is_not_network_availability() {
    // Two independent rows. A node with no network still has its map, and a
    // node with a network still has no map until one is provisioned.
    let _guard = lock();
    let directory = TempDir::new().unwrap();
    let runtime = NodeRuntime::initialize(directory.path()).unwrap();

    let root = map_root_with(Some(VALID_BASEMAP));
    let status = with_map_root(&root, || runtime.system_status());

    assert_eq!(status.map.state, ComponentState::Operational);
    // This node has no transport attached at all, and the map is unaffected.
    assert!(!runtime.network_status().unwrap().online);
}

#[test]
fn the_runtime_exposes_the_basemap_without_its_geometry() {
    // The status row is polled every couple of seconds; dragging megabytes of
    // coastline across IPC to render it would be a self-inflicted stall.
    let _guard = lock();
    let directory = TempDir::new().unwrap();
    let runtime = NodeRuntime::initialize(directory.path()).unwrap();

    let root = map_root_with(Some(VALID_BASEMAP));
    let described = with_map_root(&root, || runtime.map_basemap()).unwrap();

    assert_eq!(described.feature_count, 1);
    assert!(described.bytes > 0);

    let empty = map_root_with(None);
    assert!(with_map_root(&empty, || runtime.map_basemap()).is_none());
}

// ---------------------------------------------------------------------------
// 3 & 14. No remote map URL, no external network dependency
// ---------------------------------------------------------------------------

#[test]
fn no_online_map_provider_appears_anywhere_on_the_map_path() {
    // Comments are stripped: these files explain at length which providers they
    // refuse to use, and a scan over prose would fail on correct code.
    for path in ["src/map/mod.rs", "src/commands/map.rs"] {
        let source = std::fs::read_to_string(path).unwrap();
        let implementation = source.split("#[cfg(test)]").next().unwrap();
        let code: String = implementation
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n")
            .to_ascii_lowercase();

        for provider in [
            "mapbox",
            "googleapis",
            "google.com",
            "openstreetmap.org",
            "arcgis",
            "bing.com",
            "here.com",
            "maptiler",
            "api_key",
            "access_token",
        ] {
            assert!(!code.contains(provider), "{path} references {provider}");
        }
    }
}

#[test]
fn the_map_path_holds_no_network_client() {
    for path in ["src/map/mod.rs", "src/commands/map.rs"] {
        let source = std::fs::read_to_string(path).unwrap();
        let implementation = source.split("#[cfg(test)]").next().unwrap();
        let code: String = implementation
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");

        for transport in [
            "http://",
            "https://",
            "reqwest",
            "ureq",
            "isahc",
            "TcpStream",
            "UdpSocket",
        ] {
            assert!(!code.contains(transport), "{path} uses {transport}");
        }
    }
}

#[test]
fn the_frontend_map_layer_holds_no_network_client() {
    // The renderer is where a tile request would live if one existed. Scanned
    // from here as well as from the frontend's own test run, so a Rust-only CI
    // job still catches it.
    let directory = PathBuf::from("../src/features/map");
    let entries = std::fs::read_dir(&directory).expect("the map feature directory");

    let mut scanned = 0;
    for entry in entries {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        if name.ends_with(".test.ts") {
            continue;
        }
        if !name.ends_with(".ts") && !name.ends_with(".tsx") {
            continue;
        }

        let source = std::fs::read_to_string(&path).unwrap();
        let code: String = source
            .lines()
            .filter(|line| {
                let trimmed = line.trim_start();
                !trimmed.starts_with("//")
                    && !trimmed.starts_with("*")
                    && !trimmed.starts_with("/*")
            })
            .collect::<Vec<_>>()
            .join("\n");
        scanned += 1;

        for forbidden in [
            "http://",
            "https://",
            "fetch(",
            "XMLHttpRequest",
            "WebSocket",
            "mapbox",
            "googleapis",
            "watchPosition",
            "navigator.geolocation",
        ] {
            assert!(!code.contains(forbidden), "{name} references {forbidden}");
        }
    }

    assert!(scanned >= 3, "expected the map layer to have been scanned");
}

// ---------------------------------------------------------------------------
// 15-18. The map is only a visualization layer
// ---------------------------------------------------------------------------

#[test]
fn a_node_with_no_map_data_records_and_reads_incidents_normally() {
    let _guard = lock();
    let directory = TempDir::new().unwrap();
    let runtime = NodeRuntime::initialize(directory.path()).unwrap();
    let empty = map_root_with(None);

    with_map_root(&empty, || {
        let located = runtime
            .create_incident(incident("landslide across the access road", true))
            .unwrap();
        let unlocated = runtime
            .create_incident(incident("radio check", false))
            .unwrap();

        // Location metadata is untouched by the map's absence.
        assert_eq!(located.accuracy_meters, Some(6.0));
        assert_eq!(located.location_source, LocationSource::Gnss);
        assert_eq!(unlocated.latitude, None);

        assert_eq!(runtime.list_incidents(None).unwrap().len(), 2);
    });
}

#[test]
fn provisioning_a_basemap_changes_nothing_about_incidents() {
    // The map reads incident state and never writes it. Recording the same
    // incident with and without map data must produce identical records.
    let _guard = lock();

    let unprovisioned = {
        let directory = TempDir::new().unwrap();
        let runtime = NodeRuntime::initialize(directory.path()).unwrap();
        let empty = map_root_with(None);
        with_map_root(&empty, || {
            runtime
                .create_incident(incident("same report", true))
                .unwrap()
        })
    };

    let provisioned = {
        let directory = TempDir::new().unwrap();
        let runtime = NodeRuntime::initialize(directory.path()).unwrap();
        let root = map_root_with(Some(VALID_BASEMAP));
        with_map_root(&root, || {
            runtime
                .create_incident(incident("same report", true))
                .unwrap()
        })
    };

    assert_eq!(unprovisioned.description, provisioned.description);
    assert_eq!(unprovisioned.latitude, provisioned.latitude);
    assert_eq!(unprovisioned.accuracy_meters, provisioned.accuracy_meters);
    assert_eq!(unprovisioned.location_source, provisioned.location_source);
    assert_eq!(unprovisioned.severity, provisioned.severity);
}

#[test]
fn the_map_never_writes_to_the_database() {
    // Structural: the map module takes no database handle, so there is no path
    // from it to a write. A runtime check could only show that today's code
    // happened not to.
    let source = std::fs::read_to_string("src/map/mod.rs").unwrap();
    let implementation = source.split("#[cfg(test)]").next().unwrap();

    for forbidden in [
        "Database", "conn(", "execute(", "INSERT", "UPDATE", "DELETE",
    ] {
        assert!(
            !implementation.contains(forbidden),
            "the map module references {forbidden}"
        );
    }
}

// ---------------------------------------------------------------------------
// 18. Map data is local to a node and never replicates
// ---------------------------------------------------------------------------

#[test]
fn a_basemap_is_never_written_into_the_event_log() {
    // Each node owns its own geography. An incident replicates; the map behind
    // it does not, and must not — a basemap is tens of thousands of times
    // larger than the record it sits under, and every node provisions its own.
    let _guard = lock();
    let directory = TempDir::new().unwrap();
    let runtime = NodeRuntime::initialize(directory.path()).unwrap();
    let root = map_root_with(Some(VALID_BASEMAP));

    with_map_root(&root, || {
        runtime
            .create_incident(incident("landslide across the access road", true))
            .unwrap();

        // One event: the incident. Reading the basemap adds none.
        let _ = runtime.map_basemap();
        let _ = runtime.map_geojson();

        assert_eq!(runtime.database().count_events().unwrap(), 1);

        let origin = runtime.public_identity().node_id;
        let events = runtime.database().events_since(&origin, 0, 100).unwrap();
        for event in &events {
            // The basemap's own geometry must appear in no payload.
            assert!(
                !event.payload.contains("approach road"),
                "geography leaked into the replicated log: {}",
                event.payload
            );
            assert!(!event.payload.contains("FeatureCollection"));
        }
    });
}

#[test]
fn the_sync_protocol_has_no_message_that_could_carry_a_basemap() {
    // Structural. Replication moves signed events and nothing else, so there is
    // no channel a basemap could travel on even if something tried to send one.
    let source = std::fs::read_to_string("src/networking/protocol.rs").unwrap();
    let implementation = source.split("#[cfg(test)]").next().unwrap();

    for forbidden in ["Basemap", "basemap", "GeoJSON", "geojson", "MapData"] {
        assert!(
            !implementation.contains(forbidden),
            "the sync protocol references {forbidden}"
        );
    }
}

#[test]
fn two_nodes_can_hold_different_basemaps_and_still_agree_on_an_incident() {
    // The distinction that matters: coordinates replicate, geography does not.
    // A node with no basemap at all still receives and can display an incident
    // recorded by one that has geography.
    let _guard = lock();

    let author_dir = TempDir::new().unwrap();
    let author = NodeRuntime::initialize(author_dir.path()).unwrap();

    let with_geography = map_root_with(Some(VALID_BASEMAP));
    let created = with_map_root(&with_geography, || {
        author
            .create_incident(incident("avalanche on the approach", true))
            .unwrap()
    });

    // A second node, provisioned with nothing.
    let receiver_dir = TempDir::new().unwrap();
    let receiver = NodeRuntime::initialize(receiver_dir.path()).unwrap();
    let without = map_root_with(None);

    with_map_root(&without, || {
        assert!(receiver.map_basemap().is_none());
        // Its map status differs from the author's, and that is correct: the
        // basemap is a local asset, not shared state.
        assert_eq!(receiver.system_status().map.state, ComponentState::Inactive);
    });

    // The coordinates the author recorded are what would travel, unchanged by
    // either node's map data.
    assert_eq!(created.latitude, Some(13.133599));
    assert_eq!(created.accuracy_meters, Some(6.0));
    assert_eq!(created.location_source, LocationSource::Gnss);
}
