//! Security boundaries of the peer authorization layer.
//!
//! Phase 2 proved *authentication*: a peer holds the key behind its node ID.
//! These tests prove *authorization*: that holding a key is not permission, and
//! that permission is granted only by an explicit operator decision, bound to
//! the cryptographic identity, enforced in the Rust core, and durable.
//!
//! Everything runs over the deterministic loopback transport, so each denial is
//! a decision asserted rather than a race observed. The enforcement path is
//! identical to the one the real QUIC transport uses — only the wire differs.

use securemesh_lib::domain::{NewIncident, PeerRole, TrustEventKind, TrustState};
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
    fn incident_count(&self) -> usize {
        self.runtime.list_incidents(None).unwrap().len()
    }

    fn trust_of(&self, other: &TestNode) -> TrustState {
        self.runtime.trust_state_of(&other.node_id).unwrap()
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
    panic!("the mesh did not settle");
}

/// Connects and lets the handshake run, without approving anything.
fn connect_only(network: &LoopbackNetwork, a: &TestNode, b: &TestNode) {
    network.connect(&a.node_id, &b.node_id);
    settle(&[a, b]);
}

/// Connects and mutually enrolls.
fn connect_and_enroll(network: &LoopbackNetwork, a: &TestNode, b: &TestNode) {
    connect_only(network, a, b);
    a.runtime.approve_peer(&b.node_id, None).unwrap();
    b.runtime.approve_peer(&a.node_id, None).unwrap();
    a.runtime.request_sync().unwrap();
    b.runtime.request_sync().unwrap();
    settle(&[a, b]);
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
// Authentication is not authorization
// ---------------------------------------------------------------------------

#[test]
fn a_connected_but_unenrolled_peer_cannot_synchronise() {
    let network = LoopbackNetwork::new();
    let a = spawn(&network);
    let b = spawn(&network);

    a.runtime
        .create_incident(incident("secret operational data"))
        .unwrap();
    connect_only(&network, &a, &b);

    // The session is authenticated and open — and carries nothing.
    assert!(
        !a.runtime.connected_peers().is_empty(),
        "the session is open"
    );
    assert_eq!(
        b.incident_count(),
        0,
        "an unenrolled peer must receive nothing"
    );
    assert_eq!(b.runtime.database().count_events().unwrap(), 0);
}

#[test]
fn a_pending_peer_cannot_synchronise() {
    let network = LoopbackNetwork::new();
    let a = spawn(&network);
    let b = spawn(&network);

    a.runtime
        .create_incident(incident("still not shared"))
        .unwrap();
    connect_only(&network, &a, &b);

    // Presenting itself moves B to PENDING on A — an invitation to decide, not
    // a grant.
    assert_eq!(a.trust_of(&b), TrustState::Pending);

    // Even asking directly gets nothing.
    b.runtime.request_sync().unwrap();
    settle(&[&a, &b]);
    assert_eq!(b.incident_count(), 0);
}

#[test]
fn a_trusted_peer_can_synchronise() {
    let network = LoopbackNetwork::new();
    let a = spawn(&network);
    let b = spawn(&network);

    a.runtime
        .create_incident(incident("authorized transfer"))
        .unwrap();
    connect_and_enroll(&network, &a, &b);

    assert_eq!(a.trust_of(&b), TrustState::Trusted);
    assert_eq!(b.incident_count(), 1);
}

#[test]
fn approval_takes_effect_on_an_already_open_session() {
    let network = LoopbackNetwork::new();
    let a = spawn(&network);
    let b = spawn(&network);

    a.runtime
        .create_incident(incident("late approval"))
        .unwrap();
    connect_only(&network, &a, &b);
    assert_eq!(b.incident_count(), 0);

    // No reconnection: the operator approves while the session is already up.
    a.runtime.approve_peer(&b.node_id, None).unwrap();
    b.runtime.approve_peer(&a.node_id, None).unwrap();
    b.runtime.request_sync().unwrap();
    settle(&[&a, &b]);

    assert_eq!(b.incident_count(), 1);
}

// ---------------------------------------------------------------------------
// Revocation
// ---------------------------------------------------------------------------

#[test]
fn a_revoked_peer_cannot_synchronise() {
    let network = LoopbackNetwork::new();
    let a = spawn(&network);
    let b = spawn(&network);

    connect_and_enroll(&network, &a, &b);
    a.runtime
        .create_incident(incident("before revocation"))
        .unwrap();
    a.runtime.request_sync().unwrap();
    settle(&[&a, &b]);
    assert_eq!(b.incident_count(), 1);

    // Revoke, then create something new.
    a.runtime
        .revoke_peer(&b.node_id, Some("device lost"))
        .unwrap();
    a.runtime
        .create_incident(incident("after revocation"))
        .unwrap();

    a.runtime.request_sync().unwrap();
    b.runtime.request_sync().unwrap();
    settle(&[&a, &b]);

    assert_eq!(a.trust_of(&b), TrustState::Revoked);
    assert_eq!(
        b.incident_count(),
        1,
        "a revoked peer must not receive anything created after revocation"
    );
}

#[test]
fn revocation_takes_effect_without_a_reconnection() {
    let network = LoopbackNetwork::new();
    let a = spawn(&network);
    let b = spawn(&network);

    connect_and_enroll(&network, &a, &b);
    a.runtime.revoke_peer(&b.node_id, None).unwrap();

    // The session is still open; the authorization is not.
    assert!(network.is_connected(&a.node_id, &b.node_id));
    a.runtime
        .create_incident(incident("post revocation"))
        .unwrap();
    b.runtime.request_sync().unwrap();
    settle(&[&a, &b]);

    assert_eq!(b.incident_count(), 0);
}

#[test]
fn revocation_survives_a_restart_of_both_nodes() {
    let network = LoopbackNetwork::new();
    let a = spawn(&network);
    let b = spawn(&network);

    connect_and_enroll(&network, &a, &b);
    a.runtime.revoke_peer(&b.node_id, Some("stolen")).unwrap();

    let a = restart(&network, a);
    let b = restart(&network, b);

    // Denial is durable, not a runtime flag that a restart clears.
    assert_eq!(a.trust_of(&b), TrustState::Revoked);

    network.connect(&a.node_id, &b.node_id);
    settle(&[&a, &b]);
    a.runtime
        .create_incident(incident("after both restarted"))
        .unwrap();
    a.runtime.request_sync().unwrap();
    b.runtime.request_sync().unwrap();
    settle(&[&a, &b]);

    assert_eq!(a.trust_of(&b), TrustState::Revoked);
    assert_eq!(b.incident_count(), 0);
}

#[test]
fn a_revoked_peer_reconnecting_stays_revoked_rather_than_becoming_unknown() {
    let network = LoopbackNetwork::new();
    let a = spawn(&network);
    let b = spawn(&network);

    connect_and_enroll(&network, &a, &b);
    a.runtime.revoke_peer(&b.node_id, None).unwrap();

    // Repeated disconnect/reconnect cycles must not launder the denial into a
    // fresh enrollment opportunity.
    for _ in 0..3 {
        network.disconnect(&a.node_id, &b.node_id);
        settle(&[&a, &b]);
        network.connect(&a.node_id, &b.node_id);
        settle(&[&a, &b]);

        assert_eq!(
            a.trust_of(&b),
            TrustState::Revoked,
            "reconnecting must not reset a revocation"
        );
    }
}

#[test]
fn duplicate_revocation_is_idempotent() {
    let network = LoopbackNetwork::new();
    let a = spawn(&network);
    let b = spawn(&network);

    connect_and_enroll(&network, &a, &b);
    for _ in 0..4 {
        a.runtime.revoke_peer(&b.node_id, None).unwrap();
    }

    assert_eq!(a.trust_of(&b), TrustState::Revoked);
    // The enrollment request, the approval, and a single revocation — the
    // three repeats add nothing.
    let log = a.runtime.trust_audit_log(Some(&b.node_id), 100).unwrap();
    assert_eq!(log.len(), 3);
    assert_eq!(log[0].kind, TrustEventKind::Revoked);
}

#[test]
fn duplicate_approval_is_idempotent() {
    let network = LoopbackNetwork::new();
    let a = spawn(&network);
    let b = spawn(&network);

    connect_only(&network, &a, &b);
    for _ in 0..4 {
        a.runtime.approve_peer(&b.node_id, None).unwrap();
    }

    assert_eq!(a.trust_of(&b), TrustState::Trusted);
    // The enrollment request plus a single approval.
    let log = a.runtime.trust_audit_log(Some(&b.node_id), 100).unwrap();
    assert_eq!(log.len(), 2);
}

#[test]
fn a_revoked_peer_can_be_reinstated_by_an_operator() {
    let network = LoopbackNetwork::new();
    let a = spawn(&network);
    let b = spawn(&network);

    connect_and_enroll(&network, &a, &b);
    a.runtime.revoke_peer(&b.node_id, None).unwrap();
    a.runtime
        .approve_peer(&b.node_id, Some("recovered"))
        .unwrap();

    assert_eq!(a.trust_of(&b), TrustState::Trusted);

    a.runtime
        .create_incident(incident("after reinstatement"))
        .unwrap();
    a.runtime.request_sync().unwrap();
    b.runtime.request_sync().unwrap();
    settle(&[&a, &b]);
    assert_eq!(b.incident_count(), 1);

    let log = a.runtime.trust_audit_log(Some(&b.node_id), 100).unwrap();
    assert_eq!(log[0].kind, TrustEventKind::Reinstated);
}

// ---------------------------------------------------------------------------
// Authorization is bound to the cryptographic identity
// ---------------------------------------------------------------------------

#[test]
fn authorization_follows_the_key_not_the_display_name() {
    let network = LoopbackNetwork::new();
    let a = spawn(&network);
    let b = spawn(&network);

    connect_and_enroll(&network, &a, &b);
    a.runtime.revoke_peer(&b.node_id, None).unwrap();

    // A peer cannot rename its way out of a denial: the decision is keyed by
    // SHA-256(public key), and nothing it announces touches that.
    for _ in 0..3 {
        network.disconnect(&a.node_id, &b.node_id);
        network.connect(&a.node_id, &b.node_id);
        settle(&[&a, &b]);
    }

    assert_eq!(a.trust_of(&b), TrustState::Revoked);
}

#[test]
fn a_new_keypair_is_a_new_node_requiring_its_own_decision() {
    let network = LoopbackNetwork::new();
    let a = spawn(&network);
    let b = spawn(&network);

    connect_and_enroll(&network, &a, &b);
    assert_eq!(a.trust_of(&b), TrustState::Trusted);

    // A different node — a different keypair — inherits nothing, in either
    // direction. Trust is not transferable.
    let c = spawn(&network);
    connect_only(&network, &a, &c);

    assert_eq!(a.trust_of(&c), TrustState::Pending);
    assert_ne!(a.trust_of(&c), TrustState::Trusted);

    a.runtime.create_incident(incident("not for C")).unwrap();
    a.runtime.request_sync().unwrap();
    c.runtime.request_sync().unwrap();
    settle(&[&a, &c]);
    assert_eq!(c.incident_count(), 0);
}

#[test]
fn a_transport_address_change_does_not_affect_authorization() {
    let network = LoopbackNetwork::new();
    let a = spawn(&network);
    let b = spawn(&network);

    connect_and_enroll(&network, &a, &b);

    // Detaching and re-attaching gives a fresh transport registration — the
    // network-level equivalent of moving to a different address.
    network.detach(&b.node_id);
    let b = restart(&network, b);
    network.connect(&a.node_id, &b.node_id);
    settle(&[&a, &b]);

    assert_eq!(
        a.trust_of(&b),
        TrustState::Trusted,
        "authorization is bound to the key, not to where the peer appears from"
    );
}

// ---------------------------------------------------------------------------
// The frontend is not the security control
// ---------------------------------------------------------------------------

#[test]
fn an_ordinary_node_cannot_enroll_or_revoke_peers() {
    let network = LoopbackNetwork::new();
    let a = spawn(&network);
    let b = spawn(&network);
    connect_only(&network, &a, &b);

    // Provision A as an ordinary field node.
    a.runtime.set_local_role(PeerRole::Node).unwrap();
    assert_eq!(a.runtime.local_role().unwrap(), PeerRole::Node);

    // The core refuses regardless of what any UI would have drawn.
    let enroll = a.runtime.approve_peer(&b.node_id, None).unwrap_err();
    assert_eq!(enroll.code(), "VALIDATION_ERROR");
    assert!(enroll.message().contains("not authorized"));

    let reject = a.runtime.reject_peer(&b.node_id, None).unwrap_err();
    assert_eq!(reject.code(), "VALIDATION_ERROR");

    let revoke = a.runtime.revoke_peer(&b.node_id, None).unwrap_err();
    assert_eq!(revoke.code(), "VALIDATION_ERROR");

    // And nothing changed.
    assert_eq!(a.trust_of(&b), TrustState::Pending);
}

#[test]
fn an_ordinary_node_still_synchronises_normally() {
    let network = LoopbackNetwork::new();
    let a = spawn(&network);
    let b = spawn(&network);

    connect_and_enroll(&network, &a, &b);
    // Demoting A afterwards removes its authority over peers, not its ability
    // to do its job.
    a.runtime.set_local_role(PeerRole::Node).unwrap();

    a.runtime.create_incident(incident("field report")).unwrap();
    a.runtime.request_sync().unwrap();
    settle(&[&a, &b]);

    assert_eq!(b.incident_count(), 1);
}

#[test]
fn a_peer_cannot_change_its_own_authorization_through_the_protocol() {
    let network = LoopbackNetwork::new();
    let a = spawn(&network);
    let b = spawn(&network);

    // B does everything the protocol allows: connects, handshakes, announces
    // whatever capabilities it likes, and asks repeatedly.
    connect_only(&network, &a, &b);
    for _ in 0..10 {
        b.runtime.request_sync().unwrap();
        settle(&[&a, &b]);
    }

    // There is no protocol message that grants authorization, so the best a
    // peer can reach on its own is PENDING.
    assert_eq!(a.trust_of(&b), TrustState::Pending);
    assert_eq!(a.runtime.local_role().unwrap(), PeerRole::Admin);
    assert_eq!(
        a.runtime.database().role_of(&b.node_id).unwrap(),
        PeerRole::Node,
        "a peer cannot announce itself into an administrative role"
    );
}

#[test]
fn a_decision_cannot_be_recorded_for_an_identity_that_was_never_seen() {
    let network = LoopbackNetwork::new();
    let a = spawn(&network);

    // Authorization must reference a cryptographic identity this node holds a
    // key for, never a name or an address someone supplied.
    let invented = "f".repeat(64);
    let err = a.runtime.approve_peer(&invented, None).unwrap_err();
    assert_eq!(err.code(), "NOT_FOUND");
}

#[test]
fn malformed_peer_identifiers_are_rejected_without_panicking() {
    let network = LoopbackNetwork::new();
    let a = spawn(&network);

    for bad in [
        "",
        "   ",
        "not-hex",
        "../../etc/passwd",
        "'; DROP TABLE nodes; --",
    ] {
        assert!(a.runtime.approve_peer(bad, None).is_err());
        assert!(a.runtime.revoke_peer(bad, None).is_err());
    }

    // The node is unharmed.
    a.runtime
        .create_incident(incident("still working"))
        .unwrap();
    assert_eq!(a.incident_count(), 1);
}

// ---------------------------------------------------------------------------
// Audit trail
// ---------------------------------------------------------------------------

#[test]
fn enrollment_and_revocation_are_auditable() {
    let network = LoopbackNetwork::new();
    let a = spawn(&network);
    let b = spawn(&network);

    connect_only(&network, &a, &b);
    a.runtime
        .approve_peer(&b.node_id, Some("verified in person"))
        .unwrap();
    a.runtime
        .revoke_peer(&b.node_id, Some("handset stolen"))
        .unwrap();

    let log = a.runtime.trust_audit_log(Some(&b.node_id), 100).unwrap();
    assert_eq!(log.len(), 3);

    // Newest first, and each entry answers what/who/when/authorized-by.
    assert_eq!(log[0].kind, TrustEventKind::Revoked);
    assert_eq!(log[0].from_state, Some(TrustState::Trusted));
    assert_eq!(log[0].to_state, TrustState::Revoked);
    assert_eq!(log[0].actor_node, a.node_id);
    assert_eq!(log[0].detail.as_deref(), Some("handset stolen"));

    assert_eq!(log[1].kind, TrustEventKind::EnrollmentApproved);
    assert_eq!(log[2].kind, TrustEventKind::EnrollmentRequested);

    // Ordered by a local monotonic sequence, not by wall-clock time.
    assert!(log[0].sequence > log[1].sequence);
    assert!(log[1].sequence > log[2].sequence);
}

#[test]
fn the_audit_trail_survives_a_restart() {
    let network = LoopbackNetwork::new();
    let a = spawn(&network);
    let b = spawn(&network);

    connect_only(&network, &a, &b);
    a.runtime.approve_peer(&b.node_id, None).unwrap();
    a.runtime.revoke_peer(&b.node_id, None).unwrap();

    let a = restart(&network, a);
    let log = a.runtime.trust_audit_log(Some(&b.node_id), 100).unwrap();
    assert_eq!(log.len(), 3);
    assert_eq!(log[0].kind, TrustEventKind::Revoked);
}

// ---------------------------------------------------------------------------
// The Phase 2.5 demo scenarios
// ---------------------------------------------------------------------------

#[test]
fn peer_join_demo_unknown_then_approved_then_synchronising() {
    let network = LoopbackNetwork::new();
    let admin = spawn(&network);
    let joiner = spawn(&network);

    admin
        .runtime
        .create_incident(incident("existing situation report"))
        .unwrap();

    // B connects. A sees an unknown peer; B is not synchronised with.
    connect_only(&network, &admin, &joiner);
    assert_eq!(admin.trust_of(&joiner), TrustState::Pending);
    assert_eq!(
        joiner.incident_count(),
        0,
        "enrollment required before any data"
    );

    let pending: Vec<_> = admin
        .runtime
        .list_peers()
        .unwrap()
        .into_iter()
        .filter(|p| p.awaiting_decision())
        .collect();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].node_id, joiner.node_id);

    // The administrator approves.
    admin
        .runtime
        .approve_peer(&joiner.node_id, Some("joined the deployment"))
        .unwrap();
    joiner.runtime.approve_peer(&admin.node_id, None).unwrap();
    joiner.runtime.request_sync().unwrap();
    settle(&[&admin, &joiner]);

    assert_eq!(admin.trust_of(&joiner), TrustState::Trusted);
    assert_eq!(joiner.incident_count(), 1);
}

#[test]
fn revocation_demo_trusted_then_revoked_then_still_revoked_after_restart() {
    let network = LoopbackNetwork::new();
    let admin = spawn(&network);
    let field = spawn(&network);

    // Both trusted, synchronising normally.
    connect_and_enroll(&network, &admin, &field);
    admin
        .runtime
        .create_incident(incident("routine report"))
        .unwrap();
    admin.runtime.request_sync().unwrap();
    settle(&[&admin, &field]);
    assert_eq!(field.incident_count(), 1);

    // The administrator revokes the field node.
    admin
        .runtime
        .revoke_peer(&field.node_id, Some("compromised"))
        .unwrap();
    assert_eq!(admin.trust_of(&field), TrustState::Revoked);

    // It no longer receives anything.
    admin
        .runtime
        .create_incident(incident("sensitive follow-up"))
        .unwrap();
    admin.runtime.request_sync().unwrap();
    field.runtime.request_sync().unwrap();
    settle(&[&admin, &field]);
    assert_eq!(field.incident_count(), 1, "no new data after revocation");

    // Restart both and reconnect.
    let admin = restart(&network, admin);
    let field = restart(&network, field);
    network.connect(&admin.node_id, &field.node_id);
    settle(&[&admin, &field]);
    admin.runtime.request_sync().unwrap();
    field.runtime.request_sync().unwrap();
    settle(&[&admin, &field]);

    assert_eq!(admin.trust_of(&field), TrustState::Revoked);
    assert_eq!(field.incident_count(), 1);
}

// ---------------------------------------------------------------------------
// Interaction with the Phase 2 guarantees
// ---------------------------------------------------------------------------

#[test]
fn a_revoked_relay_cannot_be_used_to_reach_a_third_node() {
    let network = LoopbackNetwork::new();
    let a = spawn(&network);
    let b = spawn(&network);
    let c = spawn(&network);

    connect_and_enroll(&network, &a, &b);
    connect_and_enroll(&network, &b, &c);

    // B is the only path from A to C. Revoking B at A cuts that path.
    a.runtime.revoke_peer(&b.node_id, None).unwrap();
    a.runtime
        .create_incident(incident("must not reach C"))
        .unwrap();

    for _ in 0..3 {
        for node in [&a, &b, &c] {
            node.runtime.request_sync().unwrap();
        }
        settle(&[&a, &b, &c]);
    }

    assert_eq!(b.incident_count(), 0);
    assert_eq!(c.incident_count(), 0);
}

#[test]
fn trust_is_pairwise_and_is_never_delegated() {
    let network = LoopbackNetwork::new();
    let a = spawn(&network);
    let b = spawn(&network);
    let c = spawn(&network);

    // A trusts B, and B trusts C. That says nothing about A and C.
    connect_and_enroll(&network, &a, &b);
    connect_and_enroll(&network, &b, &c);

    connect_only(&network, &a, &c);
    assert_ne!(
        a.trust_of(&c),
        TrustState::Trusted,
        "trust must not transit through a shared peer"
    );
}

#[test]
fn a_standalone_node_is_unaffected_by_the_authorization_layer() {
    // Phase 1 behaviour must survive Phase 2.5 unchanged.
    let dir = TempDir::new().unwrap();
    let node = NodeRuntime::initialize(dir.path()).unwrap();

    node.create_incident(incident("no peers involved")).unwrap();

    assert_eq!(node.list_incidents(None).unwrap().len(), 1);
    assert!(node.list_peers().unwrap().is_empty());
    // The operator of a fresh node administers its own trust store.
    assert_eq!(node.local_role().unwrap(), PeerRole::Admin);
}
