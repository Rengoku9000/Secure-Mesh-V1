//! End-to-end tests for a SecureMesh node, driven through the public API of
//! the core exactly as the Tauri command layer drives it.
//!
//! These tests deliberately use real on-disk directories rather than in-memory
//! substitutes. Phase 1's central claim is that a node keeps its identity and
//! its records across a restart, and only real files can demonstrate that.
//!
//! Phase 3 added local intelligence above this layer, and nothing here changed:
//! a node's lifecycle does not depend on whether a model is provisioned. The
//! AI-specific counterpart is `ai_boundary.rs`.

use securemesh_lib::domain::{NewIncident, Severity, SyncStatus};
use securemesh_lib::NodeRuntime;
use tempfile::TempDir;

fn incident(description: &str, severity: &str) -> NewIncident {
    NewIncident {
        description: description.to_string(),
        severity: severity.to_string(),
        latitude: None,
        longitude: None,
    }
}

fn located(description: &str, severity: &str, lat: f64, lon: f64) -> NewIncident {
    NewIncident {
        description: description.to_string(),
        severity: severity.to_string(),
        latitude: Some(lat),
        longitude: Some(lon),
    }
}

#[test]
fn a_node_starts_clean_and_reports_itself_honestly() {
    let dir = TempDir::new().unwrap();
    let node = NodeRuntime::initialize(dir.path()).unwrap();

    let identity = node.public_identity();
    assert!(identity.node_name.starts_with("SM-"));
    assert_eq!(identity.node_id.len(), 64);
    assert_eq!(identity.algorithm, "Ed25519");

    // Phase 1 has no hardware key protection, and must not pretend otherwise.
    assert!(!identity.hardware_backed);

    let network = node.network_status().unwrap();
    assert!(!network.online);
    assert_eq!(network.connected_peers, 0);

    assert!(node.list_incidents(None).unwrap().is_empty());
}

#[test]
fn the_full_incident_workflow_persists_across_a_restart() {
    let dir = TempDir::new().unwrap();

    // --- First launch: create a mix of incidents ---
    let node_id = {
        let node = NodeRuntime::initialize(dir.path()).unwrap();

        node.create_incident(incident("Collapsed footbridge", "HIGH"))
            .unwrap();
        node.create_incident(located(
            "Medical camp operational",
            "MEDIUM",
            12.9716,
            77.5946,
        ))
        .unwrap();
        node.create_incident(incident("Water supply contaminated", "CRITICAL"))
            .unwrap();

        assert_eq!(node.list_incidents(None).unwrap().len(), 3);
        node.public_identity().node_id
    };

    // --- Restart: everything must still be there ---
    let node = NodeRuntime::initialize(dir.path()).unwrap();
    assert_eq!(node.public_identity().node_id, node_id);

    let incidents = node.list_incidents(None).unwrap();
    assert_eq!(incidents.len(), 3);

    // Newest first.
    assert_eq!(incidents[0].description, "Water supply contaminated");
    assert_eq!(incidents[0].severity, Severity::Critical);

    let with_location = incidents
        .iter()
        .find(|i| i.description == "Medical camp operational")
        .expect("the located incident should have survived");
    assert_eq!(with_location.latitude, Some(12.9716));
    assert_eq!(with_location.longitude, Some(77.5946));
    assert_eq!(with_location.severity, Severity::Medium);

    // Nothing has been synchronised, because there is no network yet.
    assert!(incidents
        .iter()
        .all(|i| i.sync_status == SyncStatus::Pending));
    assert_eq!(node.network_status().unwrap().pending_sync, 3);
}

#[test]
fn every_incident_is_attributed_to_the_node_that_authored_it() {
    let dir = TempDir::new().unwrap();
    let node = NodeRuntime::initialize(dir.path()).unwrap();
    let node_id = node.public_identity().node_id;

    let created = node.create_incident(incident("Road blocked", "LOW")).unwrap();
    assert_eq!(created.created_by, node_id);

    // Fetching it individually agrees with the record returned on creation.
    assert_eq!(node.get_incident(&created.id).unwrap(), created);
}

#[test]
fn two_nodes_have_independent_identities_and_independent_data() {
    let dir_a = TempDir::new().unwrap();
    let dir_b = TempDir::new().unwrap();

    let node_a = NodeRuntime::initialize(dir_a.path()).unwrap();
    let node_b = NodeRuntime::initialize(dir_b.path()).unwrap();

    let id_a = node_a.public_identity();
    let id_b = node_b.public_identity();
    assert_ne!(id_a.node_id, id_b.node_id);
    assert_ne!(id_a.public_key, id_b.public_key);

    node_a.create_incident(incident("Seen only by A", "HIGH")).unwrap();
    node_b.create_incident(incident("Seen only by B", "LOW")).unwrap();
    node_b.create_incident(incident("Also only by B", "LOW")).unwrap();

    // This is the Phase 1 baseline the Phase 2 sync engine will change: with
    // no transport, neither node knows anything about the other's records.
    assert_eq!(node_a.list_incidents(None).unwrap().len(), 1);
    assert_eq!(node_b.list_incidents(None).unwrap().len(), 2);
    assert_eq!(node_a.network_status().unwrap().known_peers, 0);
    assert_eq!(node_b.network_status().unwrap().known_peers, 0);
}

#[test]
fn invalid_input_is_rejected_and_leaves_no_trace() {
    let dir = TempDir::new().unwrap();
    let node = NodeRuntime::initialize(dir.path()).unwrap();

    let rejected = vec![
        incident("", "HIGH"),
        incident("   ", "HIGH"),
        incident("valid text", "EXTREME"),
        incident("valid text", ""),
        located("valid text", "LOW", 91.0, 0.0),
        located("valid text", "LOW", 0.0, -181.0),
        located("valid text", "LOW", f64::NAN, 0.0),
    ];

    for input in rejected {
        let err = node
            .create_incident(input)
            .expect_err("input should have been rejected");
        assert_eq!(err.code(), "VALIDATION_ERROR");
    }

    assert_eq!(node.list_incidents(None).unwrap().len(), 0);
}

#[test]
fn a_missing_incident_is_a_not_found_error_rather_than_a_panic() {
    let dir = TempDir::new().unwrap();
    let node = NodeRuntime::initialize(dir.path()).unwrap();

    for id in ["", "not-a-uuid", "00000000-0000-4000-8000-000000000000"] {
        let err = node.get_incident(id).unwrap_err();
        assert_eq!(err.code(), "NOT_FOUND");
    }
}

#[test]
fn the_private_key_never_appears_in_any_response_the_ui_can_request() {
    let dir = TempDir::new().unwrap();
    let node = NodeRuntime::initialize(dir.path()).unwrap();
    node.create_incident(incident("routine report", "LOW")).unwrap();

    // Read the actual private key off disk.
    let keyfile = std::fs::read_to_string(dir.path().join("node_identity.json")).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&keyfile).unwrap();
    let secret = parsed["secret_key"].as_str().unwrap();
    assert_eq!(secret.len(), 64, "test needs a real key to search for");

    // Serialise every command response and confirm the key is in none of them.
    let responses = [
        serde_json::to_string(&node.public_identity()).unwrap(),
        serde_json::to_string(&node.system_status()).unwrap(),
        serde_json::to_string(&node.network_status().unwrap()).unwrap(),
        serde_json::to_string(&node.list_incidents(None).unwrap()).unwrap(),
    ];

    for response in responses {
        assert!(!response.contains(secret), "private key leaked to the UI");
    }
}

#[test]
fn a_node_operates_with_no_network_interface_involved() {
    // Nothing in Phase 1 opens a socket. This test documents the offline-first
    // guarantee at the API level: a node initialises, stores, and reads back
    // its data with no transport configured and no peer available.
    let dir = TempDir::new().unwrap();
    let node = NodeRuntime::initialize(dir.path()).unwrap();

    let created = node
        .create_incident(incident("Operating fully offline", "HIGH"))
        .unwrap();

    let status = node.network_status().unwrap();
    assert_eq!(status.transport, "none");
    assert!(!status.online);

    assert_eq!(node.get_incident(&created.id).unwrap().id, created.id);
}

#[test]
fn listing_is_bounded_even_when_many_incidents_exist() {
    let dir = TempDir::new().unwrap();
    let node = NodeRuntime::initialize(dir.path()).unwrap();

    for n in 0..25 {
        node.create_incident(incident(&format!("incident {n}"), "LOW"))
            .unwrap();
    }

    assert_eq!(node.list_incidents(Some(10)).unwrap().len(), 10);
    assert_eq!(node.list_incidents(None).unwrap().len(), 25);
    // An unreasonable request is clamped rather than honoured.
    assert_eq!(node.list_incidents(Some(u32::MAX)).unwrap().len(), 25);
}

#[test]
fn a_node_survives_many_sequential_restarts() {
    let dir = TempDir::new().unwrap();

    let first = NodeRuntime::initialize(dir.path()).unwrap();
    let original_id = first.public_identity().node_id;
    drop(first);

    for round in 0..5 {
        let node = NodeRuntime::initialize(dir.path()).unwrap();
        assert_eq!(
            node.public_identity().node_id,
            original_id,
            "identity changed on restart {round}"
        );
        node.create_incident(incident(&format!("round {round}"), "LOW"))
            .unwrap();
        assert_eq!(node.list_incidents(None).unwrap().len(), round + 1);
    }

    let final_node = NodeRuntime::initialize(dir.path()).unwrap();
    assert_eq!(final_node.list_incidents(None).unwrap().len(), 5);
}
