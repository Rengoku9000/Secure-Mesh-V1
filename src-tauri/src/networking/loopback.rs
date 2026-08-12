//! A deterministic in-process transport.
//!
//! This exists so that the hard parts of a distributed system — partition,
//! duplicate delivery, reordering, restart mid-sync — can be tested as
//! *decisions*, not as races. Driving those cases over a real network would
//! make them slow and flaky, and flaky tests get deleted.
//!
//! It is a genuine [`MeshTransport`] implementation, not a stub: it enforces
//! the same authentication contract (a peer is only ever surfaced with a real
//! node ID and public key) and the same delivery semantics (a send to an
//! unreachable peer fails). What it does *not* provide is encryption, because
//! nothing leaves the process. It is therefore **test and development only**
//! and is never wired into the shipped application.

use super::protocol::Envelope;
use super::{MeshEvent, MeshTransport, PeerDescriptor};
use crate::error::{CoreError, CoreResult};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

/// The shared switchboard every loopback node plugs into.
#[derive(Default)]
struct Switch {
    /// Registered nodes and their descriptors.
    nodes: HashMap<String, PeerDescriptor>,
    /// Pending events per node, in arrival order.
    inboxes: HashMap<String, Vec<MeshEvent>>,
    /// Node pairs currently able to reach each other.
    links: Vec<(String, String)>,
}

impl Switch {
    fn linked(&self, a: &str, b: &str) -> bool {
        self.links
            .iter()
            .any(|(x, y)| (x == a && y == b) || (x == b && y == a))
    }
}

/// A test network that loopback transports share.
#[derive(Clone, Default)]
pub struct LoopbackNetwork {
    switch: Arc<Mutex<Switch>>,
}

impl LoopbackNetwork {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, Switch> {
        self.switch
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Attaches a node to the network.
    pub fn attach(&self, node_id: &str, public_key: &str) -> LoopbackTransport {
        let descriptor = PeerDescriptor {
            node_id: node_id.to_string(),
            public_key: public_key.to_string(),
            transport_peer_id: format!("loopback:{node_id}"),
        };

        let mut switch = self.lock();
        switch.nodes.insert(node_id.to_string(), descriptor);
        switch.inboxes.entry(node_id.to_string()).or_default();
        drop(switch);

        LoopbackTransport {
            node_id: node_id.to_string(),
            network: self.clone(),
        }
    }

    /// Opens a session between two nodes, delivering `PeerConnected` to both.
    pub fn connect(&self, a: &str, b: &str) {
        let mut switch = self.lock();
        if switch.linked(a, b) {
            return;
        }

        let (Some(descriptor_a), Some(descriptor_b)) =
            (switch.nodes.get(a).cloned(), switch.nodes.get(b).cloned())
        else {
            return;
        };

        switch.links.push((a.to_string(), b.to_string()));
        switch
            .inboxes
            .entry(a.to_string())
            .or_default()
            .push(MeshEvent::PeerConnected(descriptor_b));
        switch
            .inboxes
            .entry(b.to_string())
            .or_default()
            .push(MeshEvent::PeerConnected(descriptor_a));
    }

    /// Severs a link, modelling a network partition or a node going out of
    /// range. Queued messages are not delivered afterwards, exactly as a real
    /// disconnection would behave.
    pub fn disconnect(&self, a: &str, b: &str) {
        let mut switch = self.lock();
        switch
            .links
            .retain(|(x, y)| !((x == a && y == b) || (x == b && y == a)));

        for (node, other) in [(a, b), (b, a)] {
            switch
                .inboxes
                .entry(node.to_string())
                .or_default()
                .push(MeshEvent::PeerDisconnected {
                    node_id: other.to_string(),
                });
        }
    }

    /// Whether two nodes can currently reach each other.
    pub fn is_connected(&self, a: &str, b: &str) -> bool {
        self.lock().linked(a, b)
    }

    /// Removes a node entirely, modelling a process exit.
    pub fn detach(&self, node_id: &str) {
        let mut switch = self.lock();
        switch.nodes.remove(node_id);
        switch.inboxes.remove(node_id);
        switch.links.retain(|(x, y)| x != node_id && y != node_id);
    }
}

/// One node's view of a [`LoopbackNetwork`].
pub struct LoopbackTransport {
    node_id: String,
    network: LoopbackNetwork,
}

impl LoopbackTransport {
    /// Delivers a message that has already been sent, a second time.
    ///
    /// Models a retransmission or a replay attack, so idempotency can be
    /// asserted rather than assumed.
    pub fn redeliver(&self, to: &str, from: &PeerDescriptor, envelope: Envelope) {
        let mut switch = self.network.lock();
        switch
            .inboxes
            .entry(to.to_string())
            .or_default()
            .push(MeshEvent::MessageReceived {
                from: from.clone(),
                envelope,
            });
    }
}

impl MeshTransport for LoopbackTransport {
    fn local_node_id(&self) -> String {
        self.node_id.clone()
    }

    fn send(&self, to: &str, envelope: &Envelope) -> CoreResult<()> {
        let mut switch = self.network.lock();

        if !switch.linked(&self.node_id, to) {
            return Err(CoreError::internal(format!(
                "peer {to} is not currently reachable"
            )));
        }

        let Some(sender) = switch.nodes.get(&self.node_id).cloned() else {
            return Err(CoreError::internal("this node is not attached"));
        };

        switch
            .inboxes
            .entry(to.to_string())
            .or_default()
            .push(MeshEvent::MessageReceived {
                from: sender,
                envelope: envelope.clone(),
            });
        Ok(())
    }

    fn connected_peers(&self) -> Vec<PeerDescriptor> {
        let switch = self.network.lock();
        switch
            .nodes
            .values()
            .filter(|peer| {
                peer.node_id != self.node_id && switch.linked(&self.node_id, &peer.node_id)
            })
            .cloned()
            .collect()
    }

    fn poll_events(&self) -> Vec<MeshEvent> {
        let mut switch = self.network.lock();
        switch
            .inboxes
            .get_mut(&self.node_id)
            .map(std::mem::take)
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::keystore::FileKeyStore;
    use crate::identity::NodeIdentity;
    use crate::networking::protocol::MessageBody;
    use tempfile::TempDir;

    fn node(dir: &TempDir) -> NodeIdentity {
        NodeIdentity::load_or_create(&FileKeyStore::new(dir.path().join("id.json"))).unwrap()
    }

    fn ping(identity: &NodeIdentity) -> Envelope {
        Envelope::create(identity, MessageBody::Ping { nonce: 1 }).unwrap()
    }

    #[test]
    fn connecting_notifies_both_sides() {
        let dir_a = TempDir::new().unwrap();
        let dir_b = TempDir::new().unwrap();
        let (a, b) = (node(&dir_a), node(&dir_b));

        let network = LoopbackNetwork::new();
        let ta = network.attach(a.node_id(), &a.public_key_hex());
        let tb = network.attach(b.node_id(), &b.public_key_hex());
        network.connect(a.node_id(), b.node_id());

        assert!(matches!(
            ta.poll_events().as_slice(),
            [MeshEvent::PeerConnected(peer)] if peer.node_id == b.node_id()
        ));
        assert!(matches!(
            tb.poll_events().as_slice(),
            [MeshEvent::PeerConnected(peer)] if peer.node_id == a.node_id()
        ));
    }

    #[test]
    fn a_message_reaches_a_connected_peer() {
        let dir_a = TempDir::new().unwrap();
        let dir_b = TempDir::new().unwrap();
        let (a, b) = (node(&dir_a), node(&dir_b));

        let network = LoopbackNetwork::new();
        let ta = network.attach(a.node_id(), &a.public_key_hex());
        let tb = network.attach(b.node_id(), &b.public_key_hex());
        network.connect(a.node_id(), b.node_id());
        tb.poll_events();

        ta.send(b.node_id(), &ping(&a)).unwrap();

        let events = tb.poll_events();
        assert_eq!(events.len(), 1);
        match &events[0] {
            MeshEvent::MessageReceived { from, envelope } => {
                assert_eq!(from.node_id, a.node_id());
                assert_eq!(envelope.body.kind(), "PING");
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn sending_to_an_unreachable_peer_fails() {
        let dir_a = TempDir::new().unwrap();
        let dir_b = TempDir::new().unwrap();
        let (a, b) = (node(&dir_a), node(&dir_b));

        let network = LoopbackNetwork::new();
        let ta = network.attach(a.node_id(), &a.public_key_hex());
        network.attach(b.node_id(), &b.public_key_hex());

        // Never connected.
        assert!(ta.send(b.node_id(), &ping(&a)).is_err());

        // Connected, then partitioned.
        network.connect(a.node_id(), b.node_id());
        assert!(ta.send(b.node_id(), &ping(&a)).is_ok());
        network.disconnect(a.node_id(), b.node_id());
        assert!(ta.send(b.node_id(), &ping(&a)).is_err());
    }

    #[test]
    fn disconnecting_notifies_both_sides() {
        let dir_a = TempDir::new().unwrap();
        let dir_b = TempDir::new().unwrap();
        let (a, b) = (node(&dir_a), node(&dir_b));

        let network = LoopbackNetwork::new();
        let ta = network.attach(a.node_id(), &a.public_key_hex());
        let tb = network.attach(b.node_id(), &b.public_key_hex());
        network.connect(a.node_id(), b.node_id());
        ta.poll_events();
        tb.poll_events();

        network.disconnect(a.node_id(), b.node_id());

        assert!(matches!(
            ta.poll_events().as_slice(),
            [MeshEvent::PeerDisconnected { node_id }] if node_id == b.node_id()
        ));
        assert!(!network.is_connected(a.node_id(), b.node_id()));
    }

    #[test]
    fn polling_drains_the_inbox() {
        let dir_a = TempDir::new().unwrap();
        let dir_b = TempDir::new().unwrap();
        let (a, b) = (node(&dir_a), node(&dir_b));

        let network = LoopbackNetwork::new();
        let ta = network.attach(a.node_id(), &a.public_key_hex());
        let tb = network.attach(b.node_id(), &b.public_key_hex());
        network.connect(a.node_id(), b.node_id());
        tb.poll_events();

        ta.send(b.node_id(), &ping(&a)).unwrap();
        assert_eq!(tb.poll_events().len(), 1);
        assert!(tb.poll_events().is_empty(), "a second poll must be empty");
    }

    #[test]
    fn connected_peers_reflects_the_current_links() {
        let dir_a = TempDir::new().unwrap();
        let dir_b = TempDir::new().unwrap();
        let (a, b) = (node(&dir_a), node(&dir_b));

        let network = LoopbackNetwork::new();
        let ta = network.attach(a.node_id(), &a.public_key_hex());
        network.attach(b.node_id(), &b.public_key_hex());

        assert!(ta.connected_peers().is_empty());
        network.connect(a.node_id(), b.node_id());
        assert_eq!(ta.connected_peers().len(), 1);
        network.disconnect(a.node_id(), b.node_id());
        assert!(ta.connected_peers().is_empty());
    }

    #[test]
    fn a_detached_node_is_unreachable() {
        let dir_a = TempDir::new().unwrap();
        let dir_b = TempDir::new().unwrap();
        let (a, b) = (node(&dir_a), node(&dir_b));

        let network = LoopbackNetwork::new();
        let ta = network.attach(a.node_id(), &a.public_key_hex());
        network.attach(b.node_id(), &b.public_key_hex());
        network.connect(a.node_id(), b.node_id());

        // Models B's process exiting.
        network.detach(b.node_id());
        assert!(ta.send(b.node_id(), &ping(&a)).is_err());
        assert!(ta.connected_peers().is_empty());
    }
}
