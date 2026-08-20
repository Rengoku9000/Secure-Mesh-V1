//! Distributed-systems behaviour of the SecureMesh mesh.
//!
//! These tests run complete `NodeRuntime`s over the deterministic loopback
//! transport. That is a real [`MeshTransport`] implementation, so the code
//! under test is the code that ships — only the wire is swapped for an
//! in-process one.
//!
//! Doing it this way is deliberate. Partition, replay, reordering and restart
//! are the cases that actually break replication, and driving them over real
//! sockets makes them slow and timing-dependent. Here each of them is a
//! decision the engine makes, asserted exactly.
//!
//! The real QUIC transport is exercised separately in `mesh_libp2p.rs`.

use securemesh_lib::domain::{NewIncident, SyncStatus};
use securemesh_lib::identity::keystore::FileKeyStore;
use securemesh_lib::identity::NodeIdentity;
use securemesh_lib::networking::loopback::{LoopbackNetwork, LoopbackTransport};
use securemesh_lib::networking::protocol::{Envelope, MessageBody};
use securemesh_lib::networking::PeerDescriptor;
use securemesh_lib::runtime::KEYSTORE_FILE;
use securemesh_lib::NodeRuntime;
use tempfile::TempDir;

/// A node plus the directory that outlives it, so a restart can reuse both the
/// identity and the database.
struct TestNode {
    dir: TempDir,
    runtime: NodeRuntime,
    node_id: String,
}

impl TestNode {
    fn incidents(&self) -> Vec<String> {
        let mut descriptions: Vec<String> = self
            .runtime
            .list_incidents(None)
            .unwrap()
            .into_iter()
            .map(|incident| incident.description)
            .collect();
        descriptions.sort();
        descriptions
    }

    fn event_count(&self) -> u64 {
        self.runtime.database().count_events().unwrap()
    }
}

/// Creates the identity first: attaching to the network needs the node ID, and
/// the node ID is derived from the key.
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

/// Restarts a node in place, modelling a process exit and relaunch.
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

/// Ticks every node until no further work happens.
///
/// Sync is a request/response conversation, so a single tick per node is never
/// enough. Running to quiescence rather than a fixed count means the assertions
/// describe the converged state, not a snapshot mid-round.
fn settle(nodes: &[&TestNode]) {
    for _ in 0..40 {
        let mut quiet = true;
        for node in nodes {
            let report = node.runtime.sync_tick().unwrap();
            if report != Default::default() {
                quiet = false;
            }
        }
        if quiet {
            return;
        }
    }
    panic!("the mesh did not settle: sync is not converging");
}

/// Connects two nodes and completes mutual enrollment.
///
/// From Phase 2.5 a connection alone replicates nothing: the handshake proves
/// identity, and an explicit operator decision grants authorization. Every test
/// that expects synchronisation therefore has to enroll, which is exactly the
/// behaviour change this phase introduces — so the step is written out rather
/// than hidden inside `spawn`.
fn connect_and_enroll(network: &LoopbackNetwork, a: &TestNode, b: &TestNode) {
    network.connect(&a.node_id, &b.node_id);
    // Exchanging HELLO moves each node from UNKNOWN to PENDING on the other.
    settle(&[a, b]);

    a.runtime.approve_peer(&b.node_id, None).unwrap();
    b.runtime.approve_peer(&a.node_id, None).unwrap();

    // Approval alone does not push anything; a round has to be opened.
    a.runtime.request_sync().unwrap();
    b.runtime.request_sync().unwrap();
    settle(&[a, b]);
}

fn incident(description: &str, severity: &str) -> NewIncident {
    NewIncident {
        description: description.to_string(),
        severity: severity.to_string(),
        latitude: None,
        longitude: None,
        accuracy_meters: None,
        location_source: None,
        location_captured_at: None,
    }
}

// ---------------------------------------------------------------------------
// Baseline replication
// ---------------------------------------------------------------------------

#[test]
fn two_nodes_converge_after_connecting() {
    let network = LoopbackNetwork::new();
    let a = spawn(&network);
    let b = spawn(&network);

    a.runtime
        .create_incident(incident("Bridge collapsed", "HIGH"))
        .unwrap();

    connect_and_enroll(&network, &a, &b);

    assert_eq!(b.incidents(), vec!["Bridge collapsed".to_string()]);
    assert_eq!(a.incidents(), b.incidents());
}

#[test]
fn replication_is_bidirectional() {
    let network = LoopbackNetwork::new();
    let a = spawn(&network);
    let b = spawn(&network);

    a.runtime
        .create_incident(incident("from A", "LOW"))
        .unwrap();
    b.runtime
        .create_incident(incident("from B", "HIGH"))
        .unwrap();

    connect_and_enroll(&network, &a, &b);

    let expected = vec!["from A".to_string(), "from B".to_string()];
    assert_eq!(a.incidents(), expected);
    assert_eq!(b.incidents(), expected);
}

#[test]
fn a_replicated_incident_keeps_its_original_author() {
    let network = LoopbackNetwork::new();
    let a = spawn(&network);
    let b = spawn(&network);

    a.runtime
        .create_incident(incident("authored by A", "HIGH"))
        .unwrap();
    connect_and_enroll(&network, &a, &b);

    let replicated = &b.runtime.list_incidents(None).unwrap()[0];
    assert_eq!(replicated.created_by, a.node_id);
    assert_ne!(replicated.created_by, b.node_id);
}

#[test]
fn events_created_after_connecting_still_replicate() {
    let network = LoopbackNetwork::new();
    let a = spawn(&network);
    let b = spawn(&network);

    connect_and_enroll(&network, &a, &b);

    // No new PeerConnected event will fire, so this exercises the periodic
    // re-sync rather than the on-connect path.
    a.runtime
        .create_incident(incident("created later", "MEDIUM"))
        .unwrap();
    a.runtime.request_sync().unwrap();
    settle(&[&a, &b]);

    assert_eq!(b.incidents(), vec!["created later".to_string()]);
}

#[test]
fn observations_replicate_alongside_incidents() {
    let network = LoopbackNetwork::new();
    let a = spawn(&network);
    let b = spawn(&network);

    let created = a
        .runtime
        .create_incident(incident("Initial sighting", "LOW"))
        .unwrap();
    a.runtime
        .add_observation(&created.id, "Situation worsening")
        .unwrap();

    connect_and_enroll(&network, &a, &b);

    assert_eq!(b.event_count(), 2);
    let observations = b.runtime.list_observations(&created.id).unwrap();
    assert_eq!(observations.len(), 1);
    assert_eq!(observations[0].note, "Situation worsening");
}

// ---------------------------------------------------------------------------
// Partition and reconciliation
// ---------------------------------------------------------------------------

#[test]
fn incidents_created_during_a_partition_merge_on_reconnect() {
    let network = LoopbackNetwork::new();
    let a = spawn(&network);
    let b = spawn(&network);

    connect_and_enroll(&network, &a, &b);

    // Partition.
    network.disconnect(&a.node_id, &b.node_id);
    settle(&[&a, &b]);

    // Both sides keep working, independently and offline.
    a.runtime
        .create_incident(incident("INC-A", "HIGH"))
        .unwrap();
    b.runtime
        .create_incident(incident("INC-B", "CRITICAL"))
        .unwrap();

    assert_eq!(a.incidents(), vec!["INC-A".to_string()]);
    assert_eq!(b.incidents(), vec!["INC-B".to_string()]);

    // Heal.
    connect_and_enroll(&network, &a, &b);

    // Union, with nothing overwritten by a timestamp comparison.
    let expected = vec!["INC-A".to_string(), "INC-B".to_string()];
    assert_eq!(a.incidents(), expected);
    assert_eq!(b.incidents(), expected);
    assert_eq!(a.event_count(), 2);
    assert_eq!(b.event_count(), 2);
}

#[test]
fn a_long_partition_with_many_events_reconciles_fully() {
    let network = LoopbackNetwork::new();
    let a = spawn(&network);
    let b = spawn(&network);

    for n in 0..25 {
        a.runtime
            .create_incident(incident(&format!("A-{n:02}"), "LOW"))
            .unwrap();
    }
    for n in 0..15 {
        b.runtime
            .create_incident(incident(&format!("B-{n:02}"), "LOW"))
            .unwrap();
    }

    connect_and_enroll(&network, &a, &b);

    assert_eq!(a.incidents().len(), 40);
    assert_eq!(b.incidents().len(), 40);
    assert_eq!(a.incidents(), b.incidents());
}

#[test]
fn concurrent_creation_produces_no_conflicts() {
    let network = LoopbackNetwork::new();
    let a = spawn(&network);
    let b = spawn(&network);

    // Same wall-clock moment, both partitioned: the case a timestamp-ordered
    // design would resolve by discarding one side.
    a.runtime
        .create_incident(incident("simultaneous A", "HIGH"))
        .unwrap();
    b.runtime
        .create_incident(incident("simultaneous B", "HIGH"))
        .unwrap();

    connect_and_enroll(&network, &a, &b);

    assert_eq!(a.incidents().len(), 2);
    assert_eq!(b.incidents().len(), 2);
    assert_eq!(a.runtime.database().count_event_conflicts().unwrap(), 0);
    assert_eq!(b.runtime.database().count_event_conflicts().unwrap(), 0);
}

// ---------------------------------------------------------------------------
// The scenario from the Phase 2 brief
// ---------------------------------------------------------------------------

#[test]
fn offline_scenario_disconnect_create_restart_reconnect() {
    let network = LoopbackNetwork::new();
    let a = spawn(&network);
    let b = spawn(&network);

    // 1. Enroll, connect, and synchronise a first incident.
    a.runtime
        .create_incident(incident("before disconnect", "MEDIUM"))
        .unwrap();
    connect_and_enroll(&network, &a, &b);
    assert_eq!(b.incidents(), vec!["before disconnect".to_string()]);

    // 2. B goes away.
    network.disconnect(&a.node_id, &b.node_id);
    settle(&[&a, &b]);

    // 3. A keeps recording while alone.
    a.runtime
        .create_incident(incident("while alone 1", "HIGH"))
        .unwrap();
    a.runtime
        .create_incident(incident("while alone 2", "CRITICAL"))
        .unwrap();

    // 4. A restarts.
    let a = restart(&network, a);
    assert_eq!(a.incidents().len(), 3, "restart must lose nothing");

    // 5. B reconnects and catches up.
    connect_and_enroll(&network, &a, &b);

    let expected = vec![
        "before disconnect".to_string(),
        "while alone 1".to_string(),
        "while alone 2".to_string(),
    ];
    assert_eq!(a.incidents(), expected);
    assert_eq!(b.incidents(), expected);

    // No duplicates: three creations produced exactly three events on each side.
    assert_eq!(a.event_count(), 3);
    assert_eq!(b.event_count(), 3);
}

#[test]
fn a_node_restarted_mid_sync_resumes_without_loss() {
    let network = LoopbackNetwork::new();
    let a = spawn(&network);
    let b = spawn(&network);

    for n in 0..10 {
        a.runtime
            .create_incident(incident(&format!("event {n}"), "LOW"))
            .unwrap();
    }

    connect_and_enroll(&network, &a, &b);
    // A single tick each after new work: the round is deliberately left
    // unfinished so the restart lands mid-conversation.
    a.runtime.request_sync().unwrap();
    a.runtime.sync_tick().unwrap();
    b.runtime.sync_tick().unwrap();

    // Trust is durable, so the restarted node does not need re-enrolling —
    // only reconnecting.
    let b = restart(&network, b);
    network.connect(&a.node_id, &b.node_id);
    settle(&[&a, &b]);

    assert_eq!(b.incidents().len(), 10);
    assert_eq!(b.event_count(), 10);
}

// ---------------------------------------------------------------------------
// Idempotency, duplication, reordering
// ---------------------------------------------------------------------------

#[test]
fn repeated_synchronisation_changes_nothing() {
    let network = LoopbackNetwork::new();
    let a = spawn(&network);
    let b = spawn(&network);

    a.runtime
        .create_incident(incident("sync me", "LOW"))
        .unwrap();
    connect_and_enroll(&network, &a, &b);

    let before = b.event_count();

    // Ten more full rounds must be a no-op.
    for _ in 0..10 {
        a.runtime.request_sync().unwrap();
        b.runtime.request_sync().unwrap();
        settle(&[&a, &b]);
    }

    assert_eq!(b.event_count(), before);
    assert_eq!(b.incidents().len(), 1);
}

#[test]
fn duplicate_delivery_of_the_same_batch_is_idempotent() {
    let network = LoopbackNetwork::new();
    let a = spawn(&network);
    let b = spawn(&network);

    a.runtime
        .create_incident(incident("delivered twice", "LOW"))
        .unwrap();
    connect_and_enroll(&network, &a, &b);

    // Replay A's whole log at B, several times over.
    let events = a
        .runtime
        .database()
        .events_since(&a.node_id, 0, 100)
        .unwrap();
    let identity =
        NodeIdentity::load_or_create(&FileKeyStore::new(a.dir.path().join(KEYSTORE_FILE))).unwrap();
    let envelope = Envelope::create(
        &identity,
        MessageBody::EventBatch {
            origin_node: a.node_id.clone(),
            events,
            complete: true,
        },
    )
    .unwrap();

    let sender = PeerDescriptor {
        node_id: a.node_id.clone(),
        public_key: identity.public_key_hex(),
        transport_peer_id: format!("loopback:{}", a.node_id),
    };
    let replayer = network.attach(&a.node_id, &identity.public_key_hex());
    for _ in 0..5 {
        replayer.redeliver(&b.node_id, &sender, envelope.clone());
    }
    settle(&[&b]);

    assert_eq!(b.event_count(), 1);
    assert_eq!(b.incidents().len(), 1);
    assert_eq!(b.runtime.database().count_event_conflicts().unwrap(), 0);
}

#[test]
fn out_of_order_arrival_still_converges() {
    let network = LoopbackNetwork::new();
    let a = spawn(&network);
    let b = spawn(&network);

    for n in 0..5 {
        a.runtime
            .create_incident(incident(&format!("ordered {n}"), "LOW"))
            .unwrap();
    }

    // Enroll first: this test is about ordering, so A has to be authorized or
    // the batches would be refused before ordering ever came into it.
    connect_and_enroll(&network, &a, &b);

    let identity =
        NodeIdentity::load_or_create(&FileKeyStore::new(a.dir.path().join(KEYSTORE_FILE))).unwrap();
    let mut events = a
        .runtime
        .database()
        .events_since(&a.node_id, 0, 100)
        .unwrap();
    events.reverse();

    let sender = PeerDescriptor {
        node_id: a.node_id.clone(),
        public_key: identity.public_key_hex(),
        transport_peer_id: format!("loopback:{}", a.node_id),
    };
    let injector = network.attach(&a.node_id, &identity.public_key_hex());

    // Deliver each event on its own, newest first: every one but the last
    // arrives with a hole beneath it.
    for event in events {
        let envelope = Envelope::create(
            &identity,
            MessageBody::EventBatch {
                origin_node: a.node_id.clone(),
                events: vec![event],
                complete: false,
            },
        )
        .unwrap();
        injector.redeliver(&b.node_id, &sender, envelope);
    }
    settle(&[&b]);

    assert_eq!(b.event_count(), 5);
    assert_eq!(b.incidents().len(), 5);
    // The watermark only completes once the gaps are filled.
    assert_eq!(b.runtime.database().watermark_for(&a.node_id).unwrap(), 5);
}

// ---------------------------------------------------------------------------
// Hostile and malformed input
// ---------------------------------------------------------------------------

#[test]
fn a_message_signed_by_someone_other_than_the_sender_is_refused() {
    let network = LoopbackNetwork::new();
    let a = spawn(&network);
    let b = spawn(&network);
    let impostor = spawn(&network);

    // A and B are properly enrolled, so the rejection below is attributable to
    // the sender mismatch rather than to either of them being unauthorized.
    connect_and_enroll(&network, &a, &b);
    settle(&[&a, &b, &impostor]);

    // The impostor signs a batch but it is delivered as though it came from A.
    let impostor_identity =
        NodeIdentity::load_or_create(&FileKeyStore::new(impostor.dir.path().join(KEYSTORE_FILE)))
            .unwrap();
    impostor
        .runtime
        .create_incident(incident("forged", "CRITICAL"))
        .unwrap();
    let events = impostor
        .runtime
        .database()
        .events_since(&impostor.node_id, 0, 10)
        .unwrap();

    let envelope = Envelope::create(
        &impostor_identity,
        MessageBody::EventBatch {
            origin_node: impostor.node_id.clone(),
            events,
            complete: true,
        },
    )
    .unwrap();

    let a_identity =
        NodeIdentity::load_or_create(&FileKeyStore::new(a.dir.path().join(KEYSTORE_FILE))).unwrap();
    let spoofed_sender = PeerDescriptor {
        node_id: a.node_id.clone(),
        public_key: a_identity.public_key_hex(),
        transport_peer_id: format!("loopback:{}", a.node_id),
    };

    let injector = network.attach(&a.node_id, &a_identity.public_key_hex());
    injector.redeliver(&b.node_id, &spoofed_sender, envelope);

    let report = b.runtime.sync_tick().unwrap();
    assert_eq!(
        report.messages_rejected, 1,
        "sender mismatch must be refused"
    );
    assert_eq!(b.event_count(), 0);
}

#[test]
fn a_tampered_event_inside_a_valid_envelope_is_refused() {
    let network = LoopbackNetwork::new();
    let a = spawn(&network);
    let b = spawn(&network);

    a.runtime
        .create_incident(incident("genuine", "LOW"))
        .unwrap();

    // A is fully authorized, so the rejection below is attributable to the
    // tampering rather than to a missing enrollment. Authorization does not
    // make a peer's payloads trustworthy — each event is still verified
    // against its author's key.
    connect_and_enroll(&network, &a, &b);
    let baseline = b.event_count();

    let mut events = a
        .runtime
        .database()
        .events_since(&a.node_id, 0, 10)
        .unwrap();
    events[0].payload = events[0].payload.replace("genuine", "tampered");

    let identity =
        NodeIdentity::load_or_create(&FileKeyStore::new(a.dir.path().join(KEYSTORE_FILE))).unwrap();
    let envelope = Envelope::create(
        &identity,
        MessageBody::EventBatch {
            origin_node: a.node_id.clone(),
            events,
            complete: true,
        },
    )
    .unwrap();

    let sender = PeerDescriptor {
        node_id: a.node_id.clone(),
        public_key: identity.public_key_hex(),
        transport_peer_id: format!("loopback:{}", a.node_id),
    };
    let injector = network.attach(&a.node_id, &identity.public_key_hex());
    injector.redeliver(&b.node_id, &sender, envelope);

    let report = b.runtime.sync_tick().unwrap();
    assert_eq!(report.events_rejected, 1);
    assert_eq!(report.events_applied, 0);
    assert_eq!(
        b.event_count(),
        baseline,
        "the tampered event was not stored"
    );
}

#[test]
fn an_unsupported_protocol_version_is_refused_without_crashing() {
    let network = LoopbackNetwork::new();
    let a = spawn(&network);
    let b = spawn(&network);

    let identity =
        NodeIdentity::load_or_create(&FileKeyStore::new(a.dir.path().join(KEYSTORE_FILE))).unwrap();
    let mut envelope = Envelope::create(&identity, MessageBody::Ping { nonce: 7 }).unwrap();
    envelope.version = 999;

    let sender = PeerDescriptor {
        node_id: a.node_id.clone(),
        public_key: identity.public_key_hex(),
        transport_peer_id: format!("loopback:{}", a.node_id),
    };
    let injector = network.attach(&a.node_id, &identity.public_key_hex());
    injector.redeliver(&b.node_id, &sender, envelope);

    let report = b.runtime.sync_tick().unwrap();
    assert_eq!(report.messages_rejected, 1);

    // The node keeps working afterwards.
    b.runtime
        .create_incident(incident("still alive", "LOW"))
        .unwrap();
    assert_eq!(b.incidents(), vec!["still alive".to_string()]);
}

#[test]
fn a_flood_of_malformed_messages_does_not_disturb_the_node() {
    let network = LoopbackNetwork::new();
    let a = spawn(&network);
    let b = spawn(&network);

    let identity =
        NodeIdentity::load_or_create(&FileKeyStore::new(a.dir.path().join(KEYSTORE_FILE))).unwrap();
    let sender = PeerDescriptor {
        node_id: a.node_id.clone(),
        public_key: identity.public_key_hex(),
        transport_peer_id: format!("loopback:{}", a.node_id),
    };
    let injector = network.attach(&a.node_id, &identity.public_key_hex());

    for n in 0..50 {
        let mut envelope = Envelope::create(&identity, MessageBody::Ping { nonce: n }).unwrap();
        // Break the signature so every one of them fails validation.
        envelope.signature = "00".repeat(64);
        injector.redeliver(&b.node_id, &sender, envelope);
    }

    let report = b.runtime.sync_tick().unwrap();
    assert_eq!(report.messages_rejected, 50);
    assert_eq!(b.event_count(), 0);

    // Legitimate traffic still works immediately afterwards.
    a.runtime
        .create_incident(incident("after the flood", "LOW"))
        .unwrap();
    connect_and_enroll(&network, &a, &b);
    assert_eq!(b.incidents(), vec!["after the flood".to_string()]);
}

#[test]
fn equivocation_by_a_peer_is_detected_and_nothing_is_overwritten() {
    let network = LoopbackNetwork::new();
    let a = spawn(&network);
    let b = spawn(&network);

    a.runtime
        .create_incident(incident("the real event", "LOW"))
        .unwrap();
    connect_and_enroll(&network, &a, &b);
    assert_eq!(b.incidents(), vec!["the real event".to_string()]);

    // A forks its own log: a second, different event at sequence 1.
    let identity =
        NodeIdentity::load_or_create(&FileKeyStore::new(a.dir.path().join(KEYSTORE_FILE))).unwrap();
    let forked = securemesh_lib::domain::MeshEvent::create(
        &identity,
        1,
        securemesh_lib::domain::EventKind::IncidentCreated,
        securemesh_lib::domain::IncidentCreatedPayload {
            incident_id: uuid::Uuid::new_v4().to_string(),
            description: "the forked event".to_string(),
            severity: "CRITICAL".to_string(),
            latitude: None,
            longitude: None,
            accuracy_meters: None,
            location_source: securemesh_lib::domain::LocationSource::Unknown,
            location_captured_at: None,
        },
    )
    .unwrap();

    let envelope = Envelope::create(
        &identity,
        MessageBody::EventBatch {
            origin_node: a.node_id.clone(),
            events: vec![forked],
            complete: true,
        },
    )
    .unwrap();
    let sender = PeerDescriptor {
        node_id: a.node_id.clone(),
        public_key: identity.public_key_hex(),
        transport_peer_id: format!("loopback:{}", a.node_id),
    };
    let injector = network.attach(&a.node_id, &identity.public_key_hex());
    injector.redeliver(&b.node_id, &sender, envelope);

    let report = b.runtime.sync_tick().unwrap();
    assert_eq!(report.conflicts_detected, 1);

    // The original is untouched and the conflict is on record.
    assert_eq!(b.incidents(), vec!["the real event".to_string()]);
    assert_eq!(b.runtime.database().count_event_conflicts().unwrap(), 1);
}

// ---------------------------------------------------------------------------
// Multi-hop
// ---------------------------------------------------------------------------

#[test]
fn events_relay_through_an_intermediate_node() {
    let network = LoopbackNetwork::new();
    let a = spawn(&network);
    let b = spawn(&network);
    let c = spawn(&network);

    // A and C never meet. B is the only path between them, and B enrolls with
    // each of them separately — trust is pairwise and is never delegated.
    connect_and_enroll(&network, &a, &b);
    connect_and_enroll(&network, &b, &c);

    a.runtime
        .create_incident(incident("originated at A", "HIGH"))
        .unwrap();
    settle(&[&a, &b, &c]);
    a.runtime.request_sync().unwrap();
    b.runtime.request_sync().unwrap();
    c.runtime.request_sync().unwrap();
    settle(&[&a, &b, &c]);

    // C holds A's record, verified against A's key, having never spoken to A.
    assert_eq!(c.incidents(), vec!["originated at A".to_string()]);
    assert!(!network.is_connected(&a.node_id, &c.node_id));

    let replicated = &c.runtime.list_incidents(None).unwrap()[0];
    assert_eq!(replicated.created_by, a.node_id);
}

#[test]
fn a_three_node_mesh_converges_from_a_full_partition() {
    let network = LoopbackNetwork::new();
    let a = spawn(&network);
    let b = spawn(&network);
    let c = spawn(&network);

    a.runtime.create_incident(incident("INC-A", "LOW")).unwrap();
    b.runtime.create_incident(incident("INC-B", "LOW")).unwrap();
    c.runtime.create_incident(incident("INC-C", "LOW")).unwrap();

    connect_and_enroll(&network, &a, &b);
    connect_and_enroll(&network, &b, &c);
    for node in [&a, &b, &c] {
        node.runtime.request_sync().unwrap();
    }
    settle(&[&a, &b, &c]);

    let expected = vec![
        "INC-A".to_string(),
        "INC-B".to_string(),
        "INC-C".to_string(),
    ];
    assert_eq!(a.incidents(), expected);
    assert_eq!(b.incidents(), expected);
    assert_eq!(c.incidents(), expected);
}

// ---------------------------------------------------------------------------
// Peer and sync state
// ---------------------------------------------------------------------------

#[test]
fn peer_state_reflects_connection_and_disconnection() {
    let network = LoopbackNetwork::new();
    let a = spawn(&network);
    let b = spawn(&network);

    assert!(a.runtime.list_peers().unwrap().is_empty());

    connect_and_enroll(&network, &a, &b);

    let peers = a.runtime.list_peers().unwrap();
    assert_eq!(peers.len(), 1);
    assert_eq!(peers[0].node_id, b.node_id);
    assert_eq!(
        peers[0].connection_state,
        securemesh_lib::domain::ConnectionState::Connected
    );
    assert!(peers[0].last_seen.is_some());
    assert_eq!(peers[0].protocol_version, Some(1));

    network.disconnect(&a.node_id, &b.node_id);
    settle(&[&a, &b]);

    let peers = a.runtime.list_peers().unwrap();
    assert_eq!(
        peers[0].connection_state,
        securemesh_lib::domain::ConnectionState::Disconnected
    );
}

#[test]
fn a_synchronised_incident_stops_being_pending() {
    let network = LoopbackNetwork::new();
    let a = spawn(&network);
    let b = spawn(&network);

    let created = a
        .runtime
        .create_incident(incident("track my status", "LOW"))
        .unwrap();
    assert_eq!(created.sync_status, SyncStatus::Pending);
    assert_eq!(a.runtime.network_status().unwrap().pending_sync, 1);

    connect_and_enroll(&network, &a, &b);

    assert_eq!(
        a.runtime.get_incident(&created.id).unwrap().sync_status,
        SyncStatus::Synced
    );
    assert_eq!(a.runtime.network_status().unwrap().pending_sync, 0);
}

#[test]
fn pending_counts_survive_a_restart() {
    let network = LoopbackNetwork::new();
    let a = spawn(&network);
    let b = spawn(&network);

    connect_and_enroll(&network, &a, &b);

    a.runtime
        .create_incident(incident("queued for B", "LOW"))
        .unwrap();
    assert_eq!(
        a.runtime
            .database()
            .pending_events_for_peer(&b.node_id)
            .unwrap(),
        1
    );

    // The store-and-forward state is durable, not held in memory.
    let a = restart(&network, a);
    assert_eq!(
        a.runtime
            .database()
            .pending_events_for_peer(&b.node_id)
            .unwrap(),
        1
    );
}

#[test]
fn a_restarted_node_does_not_report_stale_peers_as_connected() {
    let network = LoopbackNetwork::new();
    let a = spawn(&network);
    let b = spawn(&network);

    connect_and_enroll(&network, &a, &b);
    assert!(a.runtime.network_status().unwrap().online);

    // Restart without reconnecting: no session survives a process exit.
    let a = restart(&network, a);
    let status = a.runtime.network_status().unwrap();
    assert!(!status.online);
    assert_eq!(status.connected_peers, 0);

    let peers = a.runtime.list_peers().unwrap();
    assert_eq!(peers.len(), 1, "the peer is still known");
    assert_eq!(
        peers[0].connection_state,
        securemesh_lib::domain::ConnectionState::Disconnected,
        "but must not be reported as reachable"
    );
}

#[test]
fn a_standalone_node_needs_no_mesh_to_function() {
    // Phase 1 behaviour must survive Phase 2 unchanged.
    let dir = TempDir::new().unwrap();
    let node = NodeRuntime::initialize(dir.path()).unwrap();

    node.create_incident(incident("no network at all", "HIGH"))
        .unwrap();

    assert!(!node.mesh_attached());
    assert_eq!(node.list_incidents(None).unwrap().len(), 1);
    assert_eq!(node.sync_tick().unwrap(), Default::default());
    assert!(node.list_peers().unwrap().is_empty());

    let status = node.network_status().unwrap();
    assert!(!status.online);
    assert_eq!(status.transport, "none");
}
