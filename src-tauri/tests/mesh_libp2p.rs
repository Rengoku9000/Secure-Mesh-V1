//! Two real SecureMesh nodes over the actual libp2p QUIC transport.
//!
//! `mesh_sync.rs` proves the *logic* of replication deterministically. This
//! file proves the wire: that two independent nodes discover each other on a
//! local network with no server, authenticate using their existing Ed25519
//! identities, and converge over an encrypted QUIC session.
//!
//! It is necessarily timing-dependent — discovery is asynchronous and depends
//! on the host's network stack — so it polls with a generous budget rather than
//! sleeping a fixed amount.
//!
//! **This test needs a working local network interface and permission to bind
//! a UDP port.** On a machine where a firewall blocks that, it fails for
//! environmental reasons rather than because SecureMesh is broken; the failure
//! message says so.

use securemesh_lib::domain::NewIncident;
use securemesh_lib::identity::keystore::FileKeyStore;
use securemesh_lib::identity::NodeIdentity;
use securemesh_lib::networking::libp2p_transport::Libp2pTransport;
use securemesh_lib::runtime::KEYSTORE_FILE;
use securemesh_lib::NodeRuntime;
use std::time::{Duration, Instant};
use tempfile::TempDir;

/// Total time allowed for discovery, handshake, and convergence.
const BUDGET: Duration = Duration::from_secs(45);

/// How often the nodes are ticked while waiting.
const TICK: Duration = Duration::from_millis(100);

struct MeshNode {
    _dir: TempDir,
    runtime: NodeRuntime,
    node_id: String,
}

fn spawn() -> MeshNode {
    let dir = TempDir::new().unwrap();

    // The identity is created first so the node ID is known before the
    // transport starts; the runtime then reuses the same keystore.
    let identity =
        NodeIdentity::load_or_create(&FileKeyStore::new(dir.path().join(KEYSTORE_FILE))).unwrap();
    let node_id = identity.node_id().to_string();

    let transport = Libp2pTransport::start(&identity)
        .expect("the mesh transport should start; check that UDP sockets can be bound");
    let runtime = NodeRuntime::initialize_with_transport(dir.path(), Box::new(transport)).unwrap();

    MeshNode {
        _dir: dir,
        runtime,
        node_id,
    }
}

/// Ticks both nodes until `condition` holds or the budget runs out.
fn wait_until(nodes: &[&MeshNode], label: &str, condition: impl Fn() -> bool) {
    let deadline = Instant::now() + BUDGET;

    while Instant::now() < deadline {
        for node in nodes {
            node.runtime.sync_tick().expect("a tick should not fail");
        }
        if condition() {
            return;
        }
        // Re-ask periodically: an already-open session produces no further
        // connection event, so a new record needs a fresh round to travel.
        for node in nodes {
            let _ = node.runtime.request_sync();
        }
        std::thread::sleep(TICK);
    }

    panic!(
        "timed out after {}s waiting for: {label}. \
         If this machine blocks mDNS or UDP, this is an environment failure \
         rather than a SecureMesh one.",
        BUDGET.as_secs()
    );
}

fn incident(description: &str) -> NewIncident {
    NewIncident {
        description: description.to_string(),
        severity: "HIGH".to_string(),
        latitude: None,
        longitude: None,
    }
}

#[test]
fn two_nodes_discover_authenticate_and_synchronise_over_quic() {
    let a = spawn();
    let b = spawn();

    assert_ne!(a.node_id, b.node_id, "nodes must have distinct identities");

    // 1. Discovery and authentication, with no server involved.
    wait_until(&[&a, &b], "the two nodes to discover each other", || {
        !a.runtime.connected_peers().is_empty() && !b.runtime.connected_peers().is_empty()
    });

    // The peer each node authenticated is the other's real identity: the
    // node ID is re-derived from the key libp2p proved possession of.
    let peer_of_a = &a.runtime.connected_peers()[0];
    assert_eq!(peer_of_a.node_id, b.node_id);
    assert_eq!(
        peer_of_a.node_id,
        securemesh_lib::identity::node_id_for_public_key(&peer_of_a.public_key),
        "the authenticated node ID must be the fingerprint of the authenticated key"
    );

    let status = a.runtime.network_status().unwrap();
    assert!(status.online);
    assert_eq!(status.transport, "quic");

    // 2. Authentication is not authorization: nothing replicates until each
    //    operator has explicitly enrolled the other.
    //
    //    Asserted as "not authorized" rather than as a specific state: the peer
    //    is UNKNOWN the instant it is discovered and becomes PENDING once its
    //    HELLO is processed, and which of the two is observed here is a race.
    //    Neither permits anything, which is the property that matters.
    let discovered_state = a.runtime.trust_state_of(&b.node_id).unwrap();
    assert!(
        !discovered_state.permits_authorized_operations(),
        "a discovered peer must not be authorized on sight, but was {discovered_state}"
    );
    assert_ne!(discovered_state, securemesh_lib::domain::TrustState::Trusted);

    a.runtime.approve_peer(&b.node_id, Some("demo peer")).unwrap();
    b.runtime.approve_peer(&a.node_id, Some("demo peer")).unwrap();

    // 3. Replication over the encrypted session, now that it is authorized.
    a.runtime.create_incident(incident("QUIC replication works")).unwrap();

    wait_until(&[&a, &b], "the incident to reach node B", || {
        b.runtime.list_incidents(None).unwrap().len() == 1
    });

    let replicated = &b.runtime.list_incidents(None).unwrap()[0];
    assert_eq!(replicated.description, "QUIC replication works");
    assert_eq!(
        replicated.created_by, a.node_id,
        "authorship must survive replication"
    );

    // 4. Bidirectional.
    b.runtime.create_incident(incident("and back the other way")).unwrap();
    wait_until(&[&a, &b], "B's incident to reach node A", || {
        a.runtime.list_incidents(None).unwrap().len() == 2
    });

    assert_eq!(a.runtime.list_incidents(None).unwrap().len(), 2);
    assert_eq!(b.runtime.list_incidents(None).unwrap().len(), 2);

    // 5. Converged, with no duplication from repeated sync rounds.
    assert_eq!(a.runtime.database().count_events().unwrap(), 2);
    assert_eq!(b.runtime.database().count_events().unwrap(), 2);
    assert_eq!(a.runtime.database().count_event_conflicts().unwrap(), 0);
    assert_eq!(b.runtime.database().count_event_conflicts().unwrap(), 0);
}

#[test]
fn a_node_with_no_peers_starts_and_operates_normally() {
    // Starting the transport must never be a precondition for the node
    // working: an isolated node is the normal field case.
    let node = spawn();

    node.runtime.create_incident(incident("alone on the mesh")).unwrap();
    node.runtime.sync_tick().unwrap();

    assert_eq!(node.runtime.list_incidents(None).unwrap().len(), 1);

    let status = node.runtime.network_status().unwrap();
    assert_eq!(status.transport, "quic", "a transport is attached");
    assert!(
        !status.online || status.connected_peers > 0,
        "online must mean at least one authenticated peer"
    );
}
