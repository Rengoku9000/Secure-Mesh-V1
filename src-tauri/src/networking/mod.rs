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

pub mod composite_transport;
pub mod libp2p_transport;
pub mod loopback;
pub mod lora_event_codec;
pub mod lora_event_ingest;
pub mod lora_sync;
pub mod lora_sync_requester;
pub mod lora_sync_responder;
pub mod lora_transport;
pub mod protocol;

use crate::error::{CoreError, CoreResult};
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

    /// Sends a small diagnostic payload over this transport's LoRa side, if
    /// it has one.
    ///
    /// **Not part of SecureMesh sync.** This never carries an [`Envelope`],
    /// is never called by the sync engine, and exists purely so an operator
    /// or test can prove a LoRa link is alive. The default implementation
    /// covers every transport with no LoRa side (which, in Phase 2, is every
    /// transport except [`composite_transport::CompositeTransport`]).
    fn send_lora_diagnostic(&self, _payload: &[u8]) -> CoreResult<()> {
        Err(CoreError::internal(
            "this transport has no LoRa diagnostic path",
        ))
    }

    /// Whether this transport has a LoRa side currently attached.
    fn lora_available(&self) -> bool {
        false
    }

    /// Takes any `SecureMeshEvent` frames received over this transport's
    /// LoRa side since the last call.
    ///
    /// **Not [`poll_events`](Self::poll_events).** A LoRa `SecureMeshEvent`
    /// frame is never a proof of key possession by itself — it is only a
    /// candidate for [`lora_event_ingest::ingest_event_frame`], which
    /// performs the actual trust and signature checks before anything from
    /// it can be treated as authenticated. Keeping this off `poll_events`
    /// entirely is what stops a LoRa frame from ever being mistaken for an
    /// authenticated `MeshEvent::PeerConnected`/`MessageReceived`. The
    /// default implementation covers every transport with no LoRa side.
    ///
    /// [`lora_event_ingest::ingest_event_frame`]: lora_event_ingest::ingest_event_frame
    fn poll_lora_event_frames(&self) -> Vec<lora_transport::LoraFrame> {
        Vec::new()
    }

    /// Sends an already-encoded `SecureMeshEvent` payload over this
    /// transport's LoRa side, if it has one.
    ///
    /// **Not part of SecureMesh sync**, and not `poll_events`-adjacent in the
    /// other direction either — this is Phase 3A's local-event TX mirror of
    /// [`send_lora_diagnostic`](Self::send_lora_diagnostic). `payload` must
    /// already be the output of [`lora_event_codec::encode`]: this method
    /// performs no encoding, no signing, and no size decision of its own —
    /// only framing (magic, version, this transport's own node ID, a frame
    /// sequence number, CRC) and handing the result to the LoRa I/O thread.
    /// The caller is responsible for deciding *whether* to transmit — this
    /// transport never re-derives that decision. The default implementation
    /// covers every transport with no LoRa side.
    ///
    /// [`lora_event_codec::encode`]: lora_event_codec::encode
    fn send_lora_event_payload(&self, _payload: &[u8]) -> CoreResult<()> {
        Err(CoreError::internal(
            "this transport has no LoRa event transmission path",
        ))
    }

    /// Takes up to `max` `SyncRequest` frames received over this transport's
    /// LoRa side, oldest first. Frames beyond `max` stay queued.
    ///
    /// Like [`poll_lora_event_frames`](Self::poll_lora_event_frames), these
    /// are unauthenticated candidates: the caller must verify them with
    /// [`lora_sync::verify`] against a key from its own trust state. The
    /// default implementation covers every transport with no LoRa side.
    fn poll_lora_sync_requests(&self, _max: usize) -> Vec<lora_transport::LoraFrame> {
        Vec::new()
    }

    /// Sends an already-signed `SyncRequest` payload (the output of
    /// [`lora_sync::encode`]) over this transport's LoRa side. Framing only;
    /// the caller decides whether to transmit. The default implementation
    /// covers every transport with no LoRa side.
    fn send_lora_sync_request(&self, _payload: &[u8]) -> CoreResult<()> {
        Err(CoreError::internal(
            "this transport has no LoRa sync request path",
        ))
    }
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

    fn send_lora_diagnostic(&self, payload: &[u8]) -> CoreResult<()> {
        (**self).send_lora_diagnostic(payload)
    }

    fn lora_available(&self) -> bool {
        (**self).lora_available()
    }

    fn poll_lora_event_frames(&self) -> Vec<lora_transport::LoraFrame> {
        (**self).poll_lora_event_frames()
    }

    fn send_lora_event_payload(&self, payload: &[u8]) -> CoreResult<()> {
        (**self).send_lora_event_payload(payload)
    }

    fn poll_lora_sync_requests(&self, max: usize) -> Vec<lora_transport::LoraFrame> {
        (**self).poll_lora_sync_requests(max)
    }

    fn send_lora_sync_request(&self, payload: &[u8]) -> CoreResult<()> {
        (**self).send_lora_sync_request(payload)
    }
}
