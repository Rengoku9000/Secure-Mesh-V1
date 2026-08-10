//! The real mesh transport: libp2p over QUIC, with mDNS discovery.
//!
//! # Peer authentication
//!
//! SecureMesh does not invent a handshake. The node's existing Ed25519 key
//! *is* its libp2p identity key, so the `PeerId` libp2p authenticates is
//! derived from the same key the node signs events with. A completed QUIC
//! session is therefore already a proof of possession of that private key, and
//! the SecureMesh node ID follows from it:
//!
//! ```text
//!   Ed25519 keypair ──▶ libp2p PeerId       (authenticated by TLS 1.3 in QUIC)
//!         │
//!         └── public key ──▶ SHA-256 ──▶ SecureMesh node ID
//! ```
//!
//! The alternative — a separate transport key plus a signed binding
//! certificate — would mean writing custom cryptography to prove something the
//! transport already proves. Reusing the key avoids that entirely.
//!
//! **Key reuse across protocols is a real hazard**, so it is handled
//! explicitly: libp2p signs its handshake material under its own domain
//! prefixes, and every SecureMesh signature is domain-separated (see
//! [`crate::domain::event::EVENT_SIGNING_DOMAIN`] and
//! [`crate::networking::protocol::ENVELOPE_SIGNING_DOMAIN`]). No signature
//! produced in one context is a valid signature in another.
//!
//! # Threading
//!
//! libp2p is async; the rest of the core is synchronous. The swarm runs on its
//! own Tokio runtime in a background thread and communicates through channels,
//! which keeps the async runtime confined to this file and leaves the sync
//! engine deterministic and directly testable.
//!
//! # Transport security
//!
//! QUIC provides TLS 1.3 — confidentiality, integrity, and forward secrecy —
//! between the two endpoints. This is **hop-by-hop**, not end-to-end: a relay
//! node necessarily sees the plaintext of what it forwards. Events carry their
//! own author signatures so a relay cannot *alter* them undetected, but it can
//! read them. That limitation is recorded in `docs/security/SECURITY.md`.

use super::protocol::Envelope;
use super::{MeshEvent, MeshTransport, PeerDescriptor};
use crate::error::{CoreError, CoreResult};
use crate::identity::NodeIdentity;
use libp2p::futures::StreamExt;
use libp2p::swarm::{NetworkBehaviour, SwarmEvent};
use libp2p::{identify, mdns, ping, request_response, Multiaddr, PeerId, StreamProtocol};
use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Application-level protocol name, carried in the libp2p stream negotiation.
///
/// Versioned, so a future incompatible protocol simply fails to negotiate
/// rather than connecting and then misinterpreting messages.
const SECUREMESH_PROTOCOL: &str = "/securemesh/sync/1.0.0";

/// Identify protocol version string, used for the same purpose at the peer
/// metadata layer.
const IDENTIFY_PROTOCOL: &str = "/securemesh/id/1.0.0";

/// Listen on all interfaces on an ephemeral UDP port.
///
/// Port 0 lets the OS assign, which is what allows two nodes to run on one
/// machine for testing and demos without a port clash.
const DEFAULT_LISTEN_ADDR: &str = "/ip4/0.0.0.0/udp/0/quic-v1";

/// How long a request may remain outstanding before libp2p abandons it.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// The combined libp2p behaviour for a SecureMesh node.
#[derive(NetworkBehaviour)]
struct SecureMeshBehaviour {
    /// Local-network peer discovery. No bootstrap server, no Internet.
    mdns: mdns::tokio::Behaviour,
    /// Carries SecureMesh protocol envelopes.
    sync: request_response::cbor::Behaviour<Vec<u8>, Vec<u8>>,
    /// Exchanges public keys and listen addresses after connecting.
    identify: identify::Behaviour,
    /// Detects dead sessions so peers do not linger as falsely connected.
    ping: ping::Behaviour,
}

/// Commands sent from the synchronous core into the swarm thread.
enum Command {
    Send { to: PeerId, bytes: Vec<u8> },
    Shutdown,
}

/// Shared state the core reads without touching the swarm.
#[derive(Default)]
struct SharedState {
    /// Authenticated peers, keyed by SecureMesh node ID.
    peers: HashMap<String, PeerDescriptor>,
    /// Mesh events awaiting collection by the sync engine.
    inbox: Vec<MeshEvent>,
    /// Addresses this node is listening on, for diagnostics.
    listen_addrs: Vec<String>,
}

/// A libp2p-backed [`MeshTransport`].
pub struct Libp2pTransport {
    local_node_id: String,
    commands: Sender<Command>,
    state: Arc<Mutex<SharedState>>,
    /// Maps SecureMesh node IDs to libp2p peer IDs for outbound sends.
    routing: Arc<Mutex<HashMap<String, PeerId>>>,
}

impl Libp2pTransport {
    /// Starts the mesh.
    ///
    /// Returns once the swarm thread is running; discovery and connection
    /// happen asynchronously afterwards, so a node is immediately usable and
    /// simply has no peers yet.
    pub fn start(identity: &NodeIdentity) -> CoreResult<Self> {
        let local_node_id = identity.node_id().to_string();
        let keypair = identity.libp2p_keypair()?;

        let (command_tx, command_rx) = mpsc::channel();
        let state = Arc::new(Mutex::new(SharedState::default()));
        let routing = Arc::new(Mutex::new(HashMap::new()));

        let thread_state = Arc::clone(&state);
        let thread_routing = Arc::clone(&routing);

        std::thread::Builder::new()
            .name("securemesh-mesh".to_string())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        eprintln!("[securemesh] mesh runtime failed to start: {error}");
                        return;
                    }
                };

                if let Err(error) =
                    runtime.block_on(run_swarm(keypair, command_rx, thread_state, thread_routing))
                {
                    eprintln!("[securemesh] mesh stopped: {error}");
                }
            })
            .map_err(|e| CoreError::internal(format!("could not start the mesh thread: {e}")))?;

        Ok(Self {
            local_node_id,
            commands: command_tx,
            state,
            routing,
        })
    }

    /// Starts the mesh for the node whose keystore lives in `data_dir`.
    ///
    /// Loading the identity here rather than taking one is what lets the
    /// application start the transport *before* the runtime exists — the
    /// transport needs the key, and the runtime needs the transport.
    pub fn start_for_data_dir(data_dir: &std::path::Path) -> CoreResult<Self> {
        let keystore =
            crate::identity::keystore::FileKeyStore::new(data_dir.join(crate::runtime::KEYSTORE_FILE));
        let identity = NodeIdentity::load_or_create(&keystore)?;
        Self::start(&identity)
    }

    /// Addresses this node is listening on.
    pub fn listen_addresses(&self) -> Vec<String> {
        self.lock_state().listen_addrs.clone()
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, SharedState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl Drop for Libp2pTransport {
    fn drop(&mut self) {
        // Best effort: if the swarm thread has already exited the send fails,
        // which is fine — there is nothing left to stop.
        let _ = self.commands.send(Command::Shutdown);
    }
}

impl MeshTransport for Libp2pTransport {
    fn local_node_id(&self) -> String {
        self.local_node_id.clone()
    }

    fn send(&self, to: &str, envelope: &Envelope) -> CoreResult<()> {
        let peer_id = {
            let routing = self
                .routing
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            routing.get(to).copied()
        };

        let Some(peer_id) = peer_id else {
            return Err(CoreError::internal(format!(
                "peer {to} is not currently reachable"
            )));
        };

        self.commands
            .send(Command::Send {
                to: peer_id,
                bytes: envelope.encode()?,
            })
            .map_err(|_| CoreError::internal("the mesh thread is no longer running"))
    }

    fn connected_peers(&self) -> Vec<PeerDescriptor> {
        self.lock_state().peers.values().cloned().collect()
    }

    fn poll_events(&self) -> Vec<MeshEvent> {
        std::mem::take(&mut self.lock_state().inbox)
    }
}

/// The swarm event loop.
async fn run_swarm(
    keypair: libp2p::identity::Keypair,
    commands: Receiver<Command>,
    state: Arc<Mutex<SharedState>>,
    routing: Arc<Mutex<HashMap<String, PeerId>>>,
) -> CoreResult<()> {
    let mut swarm = libp2p::SwarmBuilder::with_existing_identity(keypair.clone())
        .with_tokio()
        .with_quic()
        .with_behaviour(|key| {
            let mdns = mdns::tokio::Behaviour::new(
                mdns::Config::default(),
                key.public().to_peer_id(),
            )?;

            let sync = request_response::cbor::Behaviour::new(
                [(
                    StreamProtocol::new(SECUREMESH_PROTOCOL),
                    request_response::ProtocolSupport::Full,
                )],
                request_response::Config::default().with_request_timeout(REQUEST_TIMEOUT),
            );

            let identify = identify::Behaviour::new(identify::Config::new(
                IDENTIFY_PROTOCOL.to_string(),
                key.public(),
            ));

            Ok(SecureMeshBehaviour {
                mdns,
                sync,
                identify,
                ping: ping::Behaviour::default(),
            })
        })
        .map_err(|e| CoreError::internal(format!("mesh behaviour setup failed: {e}")))?
        .build();

    let listen_addr: Multiaddr = DEFAULT_LISTEN_ADDR
        .parse()
        .map_err(|e| CoreError::internal(format!("invalid listen address: {e}")))?;
    swarm
        .listen_on(listen_addr)
        .map_err(|e| CoreError::internal(format!("could not listen for peers: {e}")))?;

    // libp2p peer ID -> SecureMesh descriptor, for peers whose key we have seen.
    let mut known: HashMap<PeerId, PeerDescriptor> = HashMap::new();

    loop {
        // Commands arrive from a synchronous channel, so they are drained
        // between swarm events rather than awaited.
        let mut shutdown = false;
        while let Ok(command) = commands.try_recv() {
            match command {
                Command::Send { to, bytes } => {
                    swarm.behaviour_mut().sync.send_request(&to, bytes);
                }
                Command::Shutdown => shutdown = true,
            }
        }
        if shutdown {
            return Ok(());
        }

        let event = tokio::select! {
            event = swarm.select_next_some() => event,
            // Wake periodically so queued commands are not delayed by a quiet
            // network.
            _ = tokio::time::sleep(Duration::from_millis(50)) => continue,
        };

        match event {
            SwarmEvent::NewListenAddr { address, .. } => {
                let mut guard = state.lock().unwrap_or_else(|p| p.into_inner());
                guard.listen_addrs.push(address.to_string());
            }

            SwarmEvent::Behaviour(SecureMeshBehaviourEvent::Mdns(mdns::Event::Discovered(
                discovered,
            ))) => {
                // Discovery is not trust. Dialling only opens a session; the
                // peer still has to authenticate before it is recorded.
                for (peer_id, address) in discovered {
                    swarm.add_peer_address(peer_id, address);
                    let _ = swarm.dial(peer_id);
                }
            }

            SwarmEvent::Behaviour(SecureMeshBehaviourEvent::Identify(
                identify::Event::Received { peer_id, info, .. },
            )) => {
                // `info.public_key` is what binds a libp2p peer to a SecureMesh
                // identity. Re-deriving the peer ID from it means a peer cannot
                // announce a key other than the one it authenticated with.
                if info.public_key.to_peer_id() != peer_id {
                    continue;
                }
                let Ok(ed25519) = info.public_key.clone().try_into_ed25519() else {
                    // Only Ed25519 identities can be SecureMesh nodes.
                    continue;
                };

                let public_key = hex::encode(ed25519.to_bytes());
                let node_id = crate::identity::node_id_for_public_key(&public_key);

                let descriptor = PeerDescriptor {
                    node_id: node_id.clone(),
                    public_key,
                    transport_peer_id: peer_id.to_string(),
                };

                known.insert(peer_id, descriptor.clone());
                routing
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .insert(node_id.clone(), peer_id);

                let mut guard = state.lock().unwrap_or_else(|p| p.into_inner());
                if guard.peers.insert(node_id, descriptor.clone()).is_none() {
                    guard.inbox.push(MeshEvent::PeerConnected(descriptor));
                }
            }

            SwarmEvent::Behaviour(SecureMeshBehaviourEvent::Sync(
                request_response::Event::Message { message, peer, .. },
            )) => {
                let (payload, channel) = match message {
                    request_response::Message::Request {
                        request, channel, ..
                    } => (request, Some(channel)),
                    request_response::Message::Response { response, .. } => (response, None),
                };

                // Acknowledge immediately so the sender's request completes;
                // the reply carries no data because SecureMesh messages are
                // one-way and correlated by message ID, not by stream.
                if let Some(channel) = channel {
                    let _ = swarm.behaviour_mut().sync.send_response(channel, Vec::new());
                }

                if payload.is_empty() {
                    continue;
                }

                // Only accept traffic from a peer that has completed identify,
                // so every message is attributable to an authenticated key.
                let Some(descriptor) = known.get(&peer).cloned() else {
                    continue;
                };

                // Decoding validates the envelope, including its signature. A
                // peer sending rubbish is ignored, never fatal.
                match Envelope::decode(&payload) {
                    Ok(envelope) => {
                        let mut guard = state.lock().unwrap_or_else(|p| p.into_inner());
                        guard.inbox.push(MeshEvent::MessageReceived {
                            from: descriptor,
                            envelope,
                        });
                    }
                    Err(_) => continue,
                }
            }

            SwarmEvent::ConnectionClosed { peer_id, num_established, .. } => {
                // A peer may hold several connections; it is only gone when the
                // last one closes.
                if num_established > 0 {
                    continue;
                }
                if let Some(descriptor) = known.remove(&peer_id) {
                    routing
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .remove(&descriptor.node_id);

                    let mut guard = state.lock().unwrap_or_else(|p| p.into_inner());
                    guard.peers.remove(&descriptor.node_id);
                    guard.inbox.push(MeshEvent::PeerDisconnected {
                        node_id: descriptor.node_id,
                    });
                }
            }

            _ => {}
        }
    }
}
