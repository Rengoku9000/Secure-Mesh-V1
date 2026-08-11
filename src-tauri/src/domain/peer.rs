//! Peers: other SecureMesh nodes this node has met.
//!
//! The identity subsystem remains authoritative for *what a node is*. A peer
//! record adds only what identity cannot know: whether we can currently reach
//! that node, when we last did, and how far replication has got with it.
//!
//! [`Peer`] is **assembled**, not stored: the durable half comes from the
//! `nodes` table and the live half from the mesh service. That is why
//! [`ConnectionState`] can express the transient `Connecting`, which is never
//! written to the database — persisting a state that is only true mid-handshake
//! would leave stale rows behind after a crash.
//!
//! Nothing here is transport-specific. The libp2p peer ID is carried as an
//! opaque string so the domain layer never depends on libp2p types.

use crate::domain::trust::{Capability, PeerRole, TrustState};
use crate::error::{CoreError, CoreResult};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// Reachability of a peer, from this node's point of view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum ConnectionState {
    /// Known, but no session is open.
    Disconnected,
    /// A session is being established.
    Connecting,
    /// An authenticated, encrypted session is open.
    Connected,
}

impl ConnectionState {
    pub fn as_str(self) -> &'static str {
        match self {
            ConnectionState::Disconnected => "DISCONNECTED",
            ConnectionState::Connecting => "CONNECTING",
            ConnectionState::Connected => "CONNECTED",
        }
    }
}

impl fmt::Display for ConnectionState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ConnectionState {
    type Err = CoreError;

    fn from_str(value: &str) -> CoreResult<Self> {
        match value.trim().to_ascii_uppercase().as_str() {
            "DISCONNECTED" => Ok(ConnectionState::Disconnected),
            "CONNECTING" => Ok(ConnectionState::Connecting),
            "CONNECTED" => Ok(ConnectionState::Connected),
            other => Err(CoreError::storage(format!(
                "database holds an unrecognised connection state: {other}"
            ))),
        }
    }
}

/// A peer node and the state of replication with it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Peer {
    /// SecureMesh node ID — `SHA-256(public key)`, the authoritative identity.
    pub node_id: String,
    /// Operator-facing name, derived from the node ID.
    pub node_name: String,
    /// Hex-encoded Ed25519 public key, verified during the handshake.
    pub public_key: String,
    /// Transport-level peer identifier, opaque to the domain.
    pub transport_peer_id: Option<String>,
    pub connection_state: ConnectionState,
    /// Last time an authenticated session was observed.
    pub last_seen: Option<DateTime<Utc>>,
    /// Protocol version this peer announced at its last handshake.
    pub protocol_version: Option<u16>,
    /// Capabilities the peer announced, for forward compatibility.
    pub capabilities: Vec<String>,
    /// True once this peer has been caught equivocating — signing two
    /// different events at the same sequence number. Records already received
    /// are kept, but replication from it stops advancing.
    pub equivocating: bool,
    /// Events held locally that this peer has not acknowledged.
    pub pending_events: u64,
    pub first_seen: DateTime<Utc>,

    // --- Authorization (Phase 2.5) ----------------------------------------
    /// Whether this peer is authorized to participate. Independent of
    /// `connection_state`: a peer can be connected and revoked at once.
    pub trust_state: TrustState,
    /// The role this peer holds, which determines its capabilities.
    pub role: PeerRole,
    /// Capabilities derived from the role, for display.
    pub granted_capabilities: Vec<Capability>,
    pub enrolled_at: Option<DateTime<Utc>>,
    /// Node whose operator approved this peer.
    pub enrolled_by: Option<String>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub revoked_by: Option<String>,
    /// Operator note attached to the most recent decision.
    pub trust_notes: Option<String>,
}

impl Peer {
    /// Whether replication with this peer should proceed.
    ///
    /// Authorization comes first: an unauthorized peer is not replicated with
    /// however reachable it is. This is a convenience for display and for
    /// callers that already hold a `Peer`; the sync engine enforces the same
    /// rule directly against the trust store, so nothing depends on this
    /// assembled value being fresh.
    pub fn is_replicating(&self) -> bool {
        self.trust_state.permits_authorized_operations()
            && self.connection_state == ConnectionState::Connected
            && !self.equivocating
    }

    /// Whether an operator decision is outstanding for this peer.
    pub fn awaiting_decision(&self) -> bool {
        self.trust_state == TrustState::Pending
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(state: ConnectionState, equivocating: bool) -> Peer {
        trusted_peer(state, equivocating, TrustState::Trusted)
    }

    fn trusted_peer(
        state: ConnectionState,
        equivocating: bool,
        trust_state: TrustState,
    ) -> Peer {
        Peer {
            trust_state,
            role: PeerRole::Node,
            granted_capabilities: PeerRole::Node.capabilities(),
            enrolled_at: None,
            enrolled_by: None,
            revoked_at: None,
            revoked_by: None,
            trust_notes: None,
            node_id: "a".repeat(64),
            node_name: "SM-AAAAA".to_string(),
            public_key: "b".repeat(64),
            transport_peer_id: None,
            connection_state: state,
            last_seen: None,
            protocol_version: Some(1),
            capabilities: vec![],
            equivocating,
            pending_events: 0,
            first_seen: Utc::now(),
        }
    }

    #[test]
    fn connection_state_round_trips_through_its_string_form() {
        for state in [
            ConnectionState::Disconnected,
            ConnectionState::Connecting,
            ConnectionState::Connected,
        ] {
            assert_eq!(state.as_str().parse::<ConnectionState>().unwrap(), state);
        }
    }

    #[test]
    fn an_unknown_connection_state_is_a_storage_error() {
        assert_eq!(
            "TELEPORTING".parse::<ConnectionState>().unwrap_err().code(),
            "STORAGE_ERROR"
        );
    }

    #[test]
    fn only_connected_peers_replicate() {
        assert!(peer(ConnectionState::Connected, false).is_replicating());
        assert!(!peer(ConnectionState::Disconnected, false).is_replicating());
        assert!(!peer(ConnectionState::Connecting, false).is_replicating());
    }

    #[test]
    fn an_equivocating_peer_never_replicates_even_when_connected() {
        assert!(!peer(ConnectionState::Connected, true).is_replicating());
    }

    #[test]
    fn an_unauthorized_peer_never_replicates_however_reachable_it_is() {
        for state in [TrustState::Unknown, TrustState::Pending, TrustState::Revoked] {
            let peer = trusted_peer(ConnectionState::Connected, false, state);
            assert!(
                !peer.is_replicating(),
                "{state} must not replicate even while connected"
            );
        }
    }

    #[test]
    fn only_a_pending_peer_awaits_a_decision() {
        assert!(trusted_peer(ConnectionState::Connected, false, TrustState::Pending)
            .awaiting_decision());
        for state in [TrustState::Unknown, TrustState::Trusted, TrustState::Revoked] {
            assert!(!trusted_peer(ConnectionState::Connected, false, state).awaiting_decision());
        }
    }
}
