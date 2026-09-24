//! Phase 1 composite transport: QUIC plus an optional, isolated LoRa side.
//!
//! ```text
//! SyncEngine<Box<dyn MeshTransport>>
//!               │
//!       CompositeTransport
//!          /          \
//! Libp2pTransport   LoraTransport
//!  QUIC + mDNS         E22 UART
//! ```
//!
//! Everything the sync engine relies on — sending an envelope, discovering
//! peers, receiving mesh events — is answered entirely by the QUIC side.
//! LoRa never participates in [`MeshTransport::send`], so no sync traffic is
//! ever duplicated onto it; see [`super::lora_transport`] for why that
//! transport is deliberately inert at the trait level in Phase 1. LoRa's own
//! diagnostic capability is reached through this type's inherent methods
//! instead, which is the "explicit/test-only" path the integration plan
//! called for.
//!
//! Generic over the QUIC transport (`Q: MeshTransport`) rather than hardcoded
//! to [`super::libp2p_transport::Libp2pTransport`], so this can be tested
//! against [`super::loopback::LoopbackTransport`] without a real network —
//! the same reason the rest of the codebase keeps `MeshTransport` abstract.

use super::lora_transport::{LoraFrame, LoraTransport};
use super::protocol::Envelope;
use super::{MeshEvent, MeshTransport, PeerDescriptor};
use crate::error::{CoreError, CoreResult};

/// Wraps a QUIC transport with an optional LoRa side.
///
/// `lora` is `None` whenever LoRa is unconfigured or unavailable. Every
/// method on this type behaves identically to the wrapped QUIC transport
/// alone in that case — there is no code path in which a missing or failed
/// LoRa device changes QUIC's behaviour.
pub struct CompositeTransport<Q: MeshTransport> {
    quic: Q,
    lora: Option<LoraTransport>,
}

impl<Q: MeshTransport> CompositeTransport<Q> {
    /// Wraps `quic`, attaching `lora` if one is available.
    pub fn new(quic: Q, lora: Option<LoraTransport>) -> Self {
        Self { quic, lora }
    }

    /// Takes any diagnostic frames received over LoRa since the last call.
    /// Empty whenever LoRa is not attached.
    pub fn poll_lora_diagnostic_frames(&self) -> Vec<LoraFrame> {
        self.lora
            .as_ref()
            .map(LoraTransport::poll_diagnostic_frames)
            .unwrap_or_default()
    }
}

impl<Q: MeshTransport> MeshTransport for CompositeTransport<Q> {
    fn local_node_id(&self) -> String {
        self.quic.local_node_id()
    }

    /// Delegates to QUIC only. LoRa never carries a SecureMesh envelope in
    /// Phase 1, so there is nothing to choose between here yet — this is the
    /// hook where a future phase would route by peer or fall back.
    fn send(&self, to: &str, envelope: &Envelope) -> CoreResult<()> {
        self.quic.send(to, envelope)
    }

    /// Merges both sides. In Phase 1 this is exactly QUIC's peers, since
    /// [`LoraTransport::connected_peers`] always returns empty — written as a
    /// merge anyway so a later phase that teaches LoRa to authenticate peers
    /// needs no change here.
    fn connected_peers(&self) -> Vec<PeerDescriptor> {
        let mut peers = self.quic.connected_peers();
        if let Some(lora) = &self.lora {
            peers.extend(lora.connected_peers());
        }
        peers
    }

    /// Merges both sides, for the same reason as [`Self::connected_peers`].
    fn poll_events(&self) -> Vec<MeshEvent> {
        let mut events = self.quic.poll_events();
        if let Some(lora) = &self.lora {
            events.extend(lora.poll_events());
        }
        events
    }

    /// Overrides the trait default: this is the one transport in Phase 2
    /// that actually has a LoRa side to send over. Fails cleanly if none is
    /// attached, and never touches `self.quic` either way.
    fn send_lora_diagnostic(&self, payload: &[u8]) -> CoreResult<()> {
        match &self.lora {
            Some(lora) => lora.send_diagnostic(payload),
            None => Err(CoreError::internal("LoRa transport is not available")),
        }
    }

    fn lora_available(&self) -> bool {
        self.lora.is_some()
    }

    /// Overrides the trait default for the same reason as
    /// [`Self::send_lora_diagnostic`]: this is the one transport with an
    /// actual LoRa side. Empty whenever LoRa is not attached. Deliberately
    /// **not** merged into [`Self::poll_events`] — see that trait method's
    /// own doc comment on why a `SecureMeshEvent` frame must never be treated
    /// as an authenticated peer event.
    fn poll_lora_event_frames(&self) -> Vec<LoraFrame> {
        self.lora
            .as_ref()
            .map(LoraTransport::poll_event_frames)
            .unwrap_or_default()
    }

    /// Overrides the trait default for the same reason as
    /// [`Self::send_lora_diagnostic`]: this is the one transport with an
    /// actual LoRa side to send an event payload over. Fails cleanly if none
    /// is attached, and never touches `self.quic` either way.
    fn send_lora_event_payload(&self, payload: &[u8]) -> CoreResult<()> {
        match &self.lora {
            Some(lora) => lora.send_event_payload(payload),
            None => Err(CoreError::internal("LoRa transport is not available")),
        }
    }

    /// Empty whenever LoRa is not attached. Never merged into
    /// [`Self::poll_events`], for the same reason as
    /// [`Self::poll_lora_event_frames`].
    fn poll_lora_sync_requests(&self, max: usize) -> Vec<LoraFrame> {
        self.lora
            .as_ref()
            .map(|lora| lora.poll_sync_requests(max))
            .unwrap_or_default()
    }

    /// Fails cleanly if no LoRa side is attached, and never touches
    /// `self.quic` either way.
    fn send_lora_sync_request(&self, payload: &[u8]) -> CoreResult<()> {
        match &self.lora {
            Some(lora) => lora.send_sync_request(payload),
            None => Err(CoreError::internal("LoRa transport is not available")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::keystore::FileKeyStore;
    use crate::identity::NodeIdentity;
    use crate::networking::loopback::LoopbackNetwork;
    use crate::networking::protocol::MessageBody;
    use tempfile::TempDir;

    fn node(dir: &TempDir) -> NodeIdentity {
        NodeIdentity::load_or_create(&FileKeyStore::new(dir.path().join("id.json"))).unwrap()
    }

    fn ping(identity: &NodeIdentity) -> Envelope {
        Envelope::create(identity, MessageBody::Ping { nonce: 1 }).unwrap()
    }

    #[test]
    fn lora_unavailable_does_not_prevent_quic_from_working() {
        let dir_a = TempDir::new().unwrap();
        let dir_b = TempDir::new().unwrap();
        let (a, b) = (node(&dir_a), node(&dir_b));

        let network = LoopbackNetwork::new();
        let quic_a = network.attach(a.node_id(), &a.public_key_hex());
        let quic_b = network.attach(b.node_id(), &b.public_key_hex());
        network.connect(a.node_id(), b.node_id());
        quic_b.poll_events(); // drain the PeerConnected event from `connect`

        let composite = CompositeTransport::new(quic_a, None);
        assert!(!composite.lora_available());

        composite.send(b.node_id(), &ping(&a)).unwrap();
        let events = quic_b.poll_events();
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn composite_transport_preserves_quic_peer_discovery_when_lora_is_unavailable() {
        let dir_a = TempDir::new().unwrap();
        let dir_b = TempDir::new().unwrap();
        let (a, b) = (node(&dir_a), node(&dir_b));

        let network = LoopbackNetwork::new();
        let quic_a = network.attach(a.node_id(), &a.public_key_hex());
        network.attach(b.node_id(), &b.public_key_hex());
        network.connect(a.node_id(), b.node_id());

        let composite = CompositeTransport::new(quic_a, None);
        let peers = composite.connected_peers();
        assert_eq!(peers.len(), 1);
        assert_eq!(peers[0].node_id, b.node_id());
    }

    #[test]
    fn sending_a_lora_diagnostic_without_lora_attached_fails_cleanly() {
        let dir = TempDir::new().unwrap();
        let identity = node(&dir);
        let network = LoopbackNetwork::new();
        let quic = network.attach(identity.node_id(), &identity.public_key_hex());

        let composite = CompositeTransport::new(quic, None);
        assert!(composite.send_lora_diagnostic(b"test").is_err());
        assert!(composite.poll_lora_diagnostic_frames().is_empty());
    }

    #[test]
    fn quic_behavior_is_unchanged_when_a_working_lora_side_is_attached() {
        use crate::networking::lora_transport::test_support::MockSerial;

        let dir_a = TempDir::new().unwrap();
        let dir_b = TempDir::new().unwrap();
        let (a, b) = (node(&dir_a), node(&dir_b));

        let network = LoopbackNetwork::new();
        let quic_a = network.attach(a.node_id(), &a.public_key_hex());
        let quic_b = network.attach(b.node_id(), &b.public_key_hex());
        network.connect(a.node_id(), b.node_id());
        quic_b.poll_events();

        let lora = LoraTransport::open_with_io(MockSerial::default(), a.node_id().to_string());
        let composite = CompositeTransport::new(quic_a, Some(lora));
        assert!(composite.lora_available());

        // QUIC send/receive is byte-for-byte the same as the LoRa-absent case.
        composite.send(b.node_id(), &ping(&a)).unwrap();
        assert_eq!(quic_b.poll_events().len(), 1);

        // And the LoRa side, now genuinely present, actually accepts a
        // diagnostic send rather than erroring.
        assert!(composite.send_lora_diagnostic(b"test").is_ok());
    }

    #[test]
    fn sync_request_paths_fail_cleanly_without_lora_attached() {
        let dir = TempDir::new().unwrap();
        let identity = node(&dir);
        let network = LoopbackNetwork::new();
        let quic = network.attach(identity.node_id(), &identity.public_key_hex());

        let composite = CompositeTransport::new(quic, None);
        let payload = vec![0; crate::networking::lora_sync::SYNC_REQUEST_PAYLOAD_BYTES];
        assert!(composite.send_lora_sync_request(&payload).is_err());
        assert!(composite.poll_lora_sync_requests(8).is_empty());
    }

    #[test]
    fn sync_request_paths_are_forwarded_to_an_attached_lora_side() {
        use crate::networking::lora_transport::test_support::MockSerial;
        use crate::networking::lora_transport::LoraMessageType;

        let dir = TempDir::new().unwrap();
        let identity = node(&dir);
        let network = LoopbackNetwork::new();
        let quic = network.attach(identity.node_id(), &identity.public_key_hex());

        // Inbound: one queued request, delivered through the trait method.
        let inbound = LoraFrame {
            message_type: LoraMessageType::SyncRequest,
            source_node_id: [0x12; 32],
            sequence: 1,
            payload: vec![0; crate::networking::lora_sync::SYNC_REQUEST_PAYLOAD_BYTES],
        };
        let mock = MockSerial::default();
        mock.queue_inbound(&inbound.encode().unwrap());
        let written = std::sync::Arc::clone(&mock.written);

        let lora = LoraTransport::open_with_io(mock, identity.node_id().to_string());
        let composite: Box<dyn MeshTransport> = Box::new(CompositeTransport::new(quic, Some(lora)));

        let mut received = Vec::new();
        for _ in 0..200 {
            received = composite.poll_lora_sync_requests(8);
            if !received.is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(received, vec![inbound.clone()]);
        // A sync request is never surfaced as an event frame or a mesh event.
        assert!(composite.poll_lora_event_frames().is_empty());
        assert!(composite.poll_events().is_empty());

        // Outbound: the payload leaves as a type-3 frame.
        composite.send_lora_sync_request(&inbound.payload).unwrap();
        for _ in 0..200 {
            if !written.lock().unwrap().is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let frame = LoraFrame::decode(&written.lock().unwrap()).unwrap();
        assert_eq!(frame.message_type, LoraMessageType::SyncRequest);
    }
}
