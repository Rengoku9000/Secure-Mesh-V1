//! The networking boundary.
//!
//! Everything above this module — the sync engine, the domain, the runtime —
//! talks only to [`MeshTransport`]. No libp2p type appears in any signature
//! here, which is what allows the entire distributed-systems behaviour of
//! SecureMesh to be tested against a deterministic in-process transport, and
//! allows the real transport to be replaced without touching a line of
//! application logic.
//!
//! ```text
//!   sync engine ─▶ MeshTransport (trait)
//!                       ├── Libp2pTransport   real: QUIC + mDNS
//!                       └── LoopbackTransport tests: deterministic, no sockets
//! ```

pub mod libp2p_transport;
pub mod loopback;
pub mod protocol;

use crate::error::CoreResult;
use protocol::Envelope;

/// An authenticated peer, as the transport sees it.
///
/// The transport is responsible for proving that `node_id` and `public_key`
/// belong to whoever is on the other end of the connection. Layers above take
/// that as established fact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerDescriptor {
    /// SecureMesh node ID: `SHA-256(public key)`.
    pub node_id: String,
    /// Hex-encoded Ed25519 public key, verified by the transport handshake.
    pub public_key: String,
    /// Transport-level identifier, opaque above this layer.
    pub transport_peer_id: String,
}

/// Something that happened on the mesh.
#[derive(Debug, Clone)]
pub enum MeshEvent {
    /// An authenticated session was established.
    PeerConnected(PeerDescriptor),
    /// A session ended. Not an error: intermittent connectivity is normal.
    PeerDisconnected { node_id: String },
    /// A validated envelope arrived from an authenticated peer.
    MessageReceived {
        from: PeerDescriptor,
        envelope: Envelope,
    },
}

/// A transport capable of carrying SecureMesh protocol messages between nodes.
///
/// Implementations must guarantee that:
///
/// 1. every peer surfaced through [`MeshEvent::PeerConnected`] has proved
///    possession of the private key matching its `public_key`;
/// 2. `node_id` equals `SHA-256(public_key)`;
/// 3. traffic is encrypted and integrity-protected in transit.
///
/// A transport that cannot meet these must not be used: the layers above
/// perform no peer authentication of their own.
pub trait MeshTransport: Send + Sync {
    /// This node's identity as the transport presents it.
    fn local_node_id(&self) -> String;

    /// Sends a message to a connected peer.
    ///
    /// Returns an error if the peer is not currently reachable. Callers treat
    /// that as routine — the event log is durable, so an undelivered message
    /// costs nothing but a later retry.
    fn send(&self, to: &str, envelope: &Envelope) -> CoreResult<()>;

    /// Peers with an open authenticated session.
    fn connected_peers(&self) -> Vec<PeerDescriptor>;

    /// Takes any mesh events that have arrived since the last call.
    ///
    /// Polling rather than callbacks keeps the sync engine synchronous and
    /// deterministic, and keeps the async runtime confined to the libp2p
    /// implementation.
    fn poll_events(&self) -> Vec<MeshEvent>;
}

/// Lets the runtime hold whichever transport it was given without being
/// generic over it, so the same `NodeRuntime` type serves the real mesh and the
/// test harness.
impl MeshTransport for Box<dyn MeshTransport> {
    fn local_node_id(&self) -> String {
        (**self).local_node_id()
    }

    fn send(&self, to: &str, envelope: &Envelope) -> CoreResult<()> {
        (**self).send(to, envelope)
    }

    fn connected_peers(&self) -> Vec<PeerDescriptor> {
        (**self).connected_peers()
    }

    fn poll_events(&self) -> Vec<MeshEvent> {
        (**self).poll_events()
    }
}
