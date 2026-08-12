//! Synchronisation must be *caused*, not waited for.
//!
//! Phase 3 note: these tests remain valid unchanged. Local intelligence is a
//! layer above replication and cannot influence it, so none of the guarantees
//! asserted here depend on whether a model is present.
//!
//! These tests deliberately never call `request_sync()`. They drive the engine
//! only by ticking it — the equivalent of the application processing whatever
//! the transport delivered, with **the periodic timer removed**.
//!
//! That distinction is the whole point of Phase 2.6. If a test here passes only
//! because a timer eventually fired, the system does not have a trigger; it has
//! a retry loop that hides the absence of one. Every scenario below asserts
//! that some concrete event — a connection, an authorization, a local write —
//! causes convergence on its own.

use securemesh_lib::domain::{NewIncident, TrustState};
use securemesh_lib::identity::keystore::FileKeyStore;
use securemesh_lib::identity::NodeIdentity;
use securemesh_lib::networking::loopback::{LoopbackNetwork, LoopbackTransport};
use securemesh_lib::runtime::KEYSTORE_FILE;
use securemesh_lib::NodeRuntime;
use tempfile::TempDir;

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

/// Delivers whatever is already in flight, and nothing more.
///
/// **Never calls `request_sync()`.** A test that converges under this function
/// converged because something caused it to, not because a timer came round.
fn deliver(nodes: &[&TestNode]) {
    for _ in 0..60 {
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
    panic!("the mesh never went quiet — messages are looping");
}

fn incident(description: &str) -> NewIncident {
    NewIncident {
        description: description.to_string(),
        severity: "HIGH".to_string(),
        latitude: None,
        longitude: None,
    }
}

/// Connects and mutually approves, delivering messages in between.
fn connect_and_enroll(network: &LoopbackNetwork, a: &TestNode, b: &TestNode) {
    network.connect(&a.node_id, &b.node_id);
    deliver(&[a, b]);
    a.runtime.approve_peer(&b.node_id, None).unwrap();
    b.runtime.approve_peer(&a.node_id, None).unwrap();
    deliver(&[a, b]);
}

// ---------------------------------------------------------------------------
// The reproduction: authorization must itself trigger synchronisation
// ---------------------------------------------------------------------------

#[test]
fn approving_a_connected_peer_triggers_synchronisation_by_itself() {
    // This is the observed failure, reduced. Two nodes connect while neither
    // is enrolled, so the connection-time trigger correctly does nothing. The
    // operator then approves. If approval is not itself a trigger, the only
    // thing left to start replication is the periodic timer — and with the
    // timer removed, nothing happens at all.
    let network = LoopbackNetwork::new();
    let a = spawn(&network);
    let b = spawn(&network);

    a.runtime
        .create_incident(incident("recorded before enrollment"))
        .unwrap();

    network.connect(&a.node_id, &b.node_id);
    deliver(&[&a, &b]);
    assert_eq!(
        a.runtime.trust_state_of(&b.node_id).unwrap(),
        TrustState::Pending
    );
    assert_eq!(b.incidents().len(), 0, "nothing before approval, correctly");

    // The only actions from here are the two approvals.
    a.runtime.approve_peer(&b.node_id, None).unwrap();
    b.runtime.approve_peer(&a.node_id, None).unwrap();
    deliver(&[&a, &b]);

    assert_eq!(
        b.incidents(),
        vec!["recorded before enrollment".to_string()],
        "authorization must cause synchronisation, not merely permit it"
    );
}

#[test]
fn creating_an_incident_triggers_synchronisation_by_itself() {
    // A record written while a session is already open has no connection event
    // and no authorization change behind it. Something still has to carry it.
    let network = LoopbackNetwork::new();
    let a = spawn(&network);
    let b = spawn(&network);
    connect_and_enroll(&network, &a, &b);

    a.runtime
        .create_incident(incident("written mid-session"))
        .unwrap();
    deliver(&[&a, &b]);

    assert_eq!(b.incidents(), vec!["written mid-session".to_string()]);
}

#[test]
fn an_observation_also_triggers_synchronisation() {
    let network = LoopbackNetwork::new();
    let a = spawn(&network);
    let b = spawn(&network);
    connect_and_enroll(&network, &a, &b);

    let created = a
        .runtime
        .create_incident(incident("initial report"))
        .unwrap();
    deliver(&[&a, &b]);

    a.runtime
        .add_observation(&created.id, "water rising")
        .unwrap();
    deliver(&[&a, &b]);

    assert_eq!(b.runtime.list_observations(&created.id).unwrap().len(), 1);
}

// ---------------------------------------------------------------------------
// Direction independence
// ---------------------------------------------------------------------------

#[test]
fn synchronisation_does_not_depend_on_which_side_holds_the_data() {
    // Whichever node happens to have events, and whichever direction the
    // connection was opened from, both must converge.
    for data_on_a in [true, false] {
        let network = LoopbackNetwork::new();
        let a = spawn(&network);
        let b = spawn(&network);

        let holder = if data_on_a { &a } else { &b };
        holder
            .runtime
            .create_incident(incident("the only record"))
            .unwrap();

        connect_and_enroll(&network, &a, &b);

        assert_eq!(
            a.incidents(),
            vec!["the only record".to_string()],
            "A must converge (data started on A = {data_on_a})"
        );
        assert_eq!(
            b.incidents(),
            vec!["the only record".to_string()],
            "B must converge (data started on A = {data_on_a})"
        );
    }
}

#[test]
fn synchronisation_does_not_depend_on_which_side_approves_first() {
    // The two approvals are independent local decisions and can land in either
    // order. Neither order may leave the mesh stuck.
    for a_first in [true, false] {
        let network = LoopbackNetwork::new();
        let a = spawn(&network);
        let b = spawn(&network);

        a.runtime.create_incident(incident("from A")).unwrap();
        b.runtime.create_incident(incident("from B")).unwrap();

        network.connect(&a.node_id, &b.node_id);
        deliver(&[&a, &b]);

        if a_first {
            a.runtime.approve_peer(&b.node_id, None).unwrap();
            deliver(&[&a, &b]);
            b.runtime.approve_peer(&a.node_id, None).unwrap();
        } else {
            b.runtime.approve_peer(&a.node_id, None).unwrap();
            deliver(&[&a, &b]);
            a.runtime.approve_peer(&b.node_id, None).unwrap();
        }
        deliver(&[&a, &b]);

        let expected = vec!["from A".to_string(), "from B".to_string()];
        assert_eq!(
            a.incidents(),
            expected,
            "A stuck (A approved first = {a_first})"
        );
        assert_eq!(
            b.incidents(),
            expected,
            "B stuck (A approved first = {a_first})"
        );
    }
}

#[test]
fn a_node_that_is_behind_catches_up_without_asking_first() {
    // A holds three events, B holds one of them. B is behind but has no reason
    // to know it. Reconnecting must reconcile regardless of who speaks first.
    let network = LoopbackNetwork::new();
    let a = spawn(&network);
    let b = spawn(&network);

    a.runtime.create_incident(incident("A-1")).unwrap();
    connect_and_enroll(&network, &a, &b);
    assert_eq!(b.event_count(), 1);

    network.disconnect(&a.node_id, &b.node_id);
    deliver(&[&a, &b]);
    a.runtime.create_incident(incident("A-2")).unwrap();
    a.runtime.create_incident(incident("A-3")).unwrap();

    network.connect(&a.node_id, &b.node_id);
    deliver(&[&a, &b]);

    assert_eq!(b.event_count(), 3, "B must catch up on reconnection alone");
    assert_eq!(a.event_count(), 3);
}

// ---------------------------------------------------------------------------
// Reconnection and restart
// ---------------------------------------------------------------------------

#[test]
fn reconnection_alone_resynchronises() {
    let network = LoopbackNetwork::new();
    let a = spawn(&network);
    let b = spawn(&network);
    connect_and_enroll(&network, &a, &b);

    network.disconnect(&a.node_id, &b.node_id);
    deliver(&[&a, &b]);
    a.runtime.create_incident(incident("while apart")).unwrap();
    deliver(&[&a, &b]);
    assert_eq!(b.incidents().len(), 0);

    network.connect(&a.node_id, &b.node_id);
    deliver(&[&a, &b]);

    assert_eq!(b.incidents(), vec!["while apart".to_string()]);
}

#[test]
fn restart_alone_resynchronises() {
    let network = LoopbackNetwork::new();
    let a = spawn(&network);
    let b = spawn(&network);
    connect_and_enroll(&network, &a, &b);

    network.disconnect(&a.node_id, &b.node_id);
    deliver(&[&a, &b]);
    a.runtime
        .create_incident(incident("created while B was away"))
        .unwrap();

    // B restarts. Trust is durable, so no re-enrollment is needed — but the
    // reconnection must still start a round on its own.
    let b = restart(&network, b);
    network.connect(&a.node_id, &b.node_id);
    deliver(&[&a, &b]);

    assert_eq!(b.incidents(), vec!["created while B was away".to_string()]);
}

#[test]
fn a_restart_of_the_data_holder_also_resynchronises() {
    let network = LoopbackNetwork::new();
    let a = spawn(&network);
    let b = spawn(&network);
    connect_and_enroll(&network, &a, &b);

    network.disconnect(&a.node_id, &b.node_id);
    deliver(&[&a, &b]);
    a.runtime
        .create_incident(incident("survives A restarting"))
        .unwrap();

    let a = restart(&network, a);
    network.connect(&a.node_id, &b.node_id);
    deliver(&[&a, &b]);

    assert_eq!(b.incidents(), vec!["survives A restarting".to_string()]);
}

// ---------------------------------------------------------------------------
// The failure-recovery scenario from the brief
// ---------------------------------------------------------------------------

#[test]
fn concurrent_writes_then_disconnect_then_restart_then_reconnect_converges() {
    let network = LoopbackNetwork::new();
    let a = spawn(&network);
    let b = spawn(&network);
    connect_and_enroll(&network, &a, &b);

    // Both write immediately, then both drop off.
    a.runtime.create_incident(incident("A wrote this")).unwrap();
    b.runtime.create_incident(incident("B wrote this")).unwrap();
    network.disconnect(&a.node_id, &b.node_id);
    deliver(&[&a, &b]);

    // One of them restarts while apart.
    let b = restart(&network, b);

    network.connect(&a.node_id, &b.node_id);
    deliver(&[&a, &b]);

    let expected = vec!["A wrote this".to_string(), "B wrote this".to_string()];
    assert_eq!(a.incidents(), expected);
    assert_eq!(b.incidents(), expected);
    assert_eq!(a.event_count(), 2, "no duplicates");
    assert_eq!(b.event_count(), 2, "no duplicates");
}

#[test]
fn partition_recovery_needs_no_restart_and_no_manual_step() {
    let network = LoopbackNetwork::new();
    let a = spawn(&network);
    let b = spawn(&network);
    connect_and_enroll(&network, &a, &b);

    network.disconnect(&a.node_id, &b.node_id);
    deliver(&[&a, &b]);
    a.runtime.create_incident(incident("A2")).unwrap();
    b.runtime.create_incident(incident("B2")).unwrap();

    network.connect(&a.node_id, &b.node_id);
    deliver(&[&a, &b]);

    let expected = vec!["A2".to_string(), "B2".to_string()];
    assert_eq!(a.incidents(), expected);
    assert_eq!(b.incidents(), expected);
}

// ---------------------------------------------------------------------------
// Repetition and idempotency
// ---------------------------------------------------------------------------

#[test]
fn repeated_rounds_of_traffic_never_duplicate_or_lose_events() {
    let network = LoopbackNetwork::new();
    let a = spawn(&network);
    let b = spawn(&network);
    connect_and_enroll(&network, &a, &b);

    // Alternate writers over many rounds, delivering after each.
    for round in 0..12 {
        let writer = if round % 2 == 0 { &a } else { &b };
        writer
            .runtime
            .create_incident(incident(&format!("round {round}")))
            .unwrap();
        deliver(&[&a, &b]);
    }

    assert_eq!(a.event_count(), 12);
    assert_eq!(b.event_count(), 12);
    assert_eq!(a.incidents(), b.incidents());
    assert_eq!(a.runtime.database().count_event_conflicts().unwrap(), 0);
    assert_eq!(b.runtime.database().count_event_conflicts().unwrap(), 0);
}

#[test]
fn the_bidirectional_round_trip_from_the_brief() {
    // Create on A, verify on B. Then create on B, verify on A. No restart.
    let network = LoopbackNetwork::new();
    let a = spawn(&network);
    let b = spawn(&network);
    connect_and_enroll(&network, &a, &b);

    a.runtime.create_incident(incident("A to B")).unwrap();
    deliver(&[&a, &b]);
    assert!(b.incidents().contains(&"A to B".to_string()));

    b.runtime.create_incident(incident("B to A")).unwrap();
    deliver(&[&a, &b]);
    assert!(a.incidents().contains(&"B to A".to_string()));

    // Repeatedly, to catch a trigger that only works once.
    for n in 0..5 {
        a.runtime
            .create_incident(incident(&format!("A round {n}")))
            .unwrap();
        deliver(&[&a, &b]);
        b.runtime
            .create_incident(incident(&format!("B round {n}")))
            .unwrap();
        deliver(&[&a, &b]);
    }

    assert_eq!(a.incidents(), b.incidents());
    assert_eq!(a.event_count(), 12);
}

// ---------------------------------------------------------------------------
// Authorization changes mid-session
// ---------------------------------------------------------------------------

#[test]
fn revocation_stops_replication_and_reinstatement_resumes_it() {
    let network = LoopbackNetwork::new();
    let a = spawn(&network);
    let b = spawn(&network);
    connect_and_enroll(&network, &a, &b);

    a.runtime.revoke_peer(&b.node_id, None).unwrap();
    a.runtime
        .create_incident(incident("while revoked"))
        .unwrap();
    deliver(&[&a, &b]);
    assert_eq!(b.incidents().len(), 0, "a revoked peer receives nothing");

    // Reinstating must resume without a reconnection: the queued work is not
    // lost, it was simply never authorized to leave.
    a.runtime.approve_peer(&b.node_id, None).unwrap();
    deliver(&[&a, &b]);

    assert_eq!(b.incidents(), vec!["while revoked".to_string()]);
}

#[test]
fn replication_requires_trust_in_both_directions() {
    // A one-sided approval must not move data. B has not authorized A, so B
    // must neither accept A's records nor hand over its own.
    let network = LoopbackNetwork::new();
    let a = spawn(&network);
    let b = spawn(&network);

    a.runtime.create_incident(incident("from A")).unwrap();
    b.runtime.create_incident(incident("from B")).unwrap();

    network.connect(&a.node_id, &b.node_id);
    deliver(&[&a, &b]);

    a.runtime.approve_peer(&b.node_id, None).unwrap();
    deliver(&[&a, &b]);

    assert_eq!(a.incidents(), vec!["from A".to_string()]);
    assert_eq!(b.incidents(), vec!["from B".to_string()]);

    // Once B reciprocates, both converge — again with no timer involved.
    b.runtime.approve_peer(&a.node_id, None).unwrap();
    deliver(&[&a, &b]);

    let expected = vec!["from A".to_string(), "from B".to_string()];
    assert_eq!(a.incidents(), expected);
    assert_eq!(b.incidents(), expected);
}

// ---------------------------------------------------------------------------
// Three nodes
// ---------------------------------------------------------------------------

#[test]
fn a_relay_forwards_without_any_periodic_sweep() {
    let network = LoopbackNetwork::new();
    let a = spawn(&network);
    let b = spawn(&network);
    let c = spawn(&network);

    connect_and_enroll(&network, &a, &b);
    connect_and_enroll(&network, &b, &c);

    a.runtime
        .create_incident(incident("originated at A"))
        .unwrap();
    deliver(&[&a, &b, &c]);

    assert_eq!(c.incidents(), vec!["originated at A".to_string()]);
    assert!(!network.is_connected(&a.node_id, &c.node_id));
}
