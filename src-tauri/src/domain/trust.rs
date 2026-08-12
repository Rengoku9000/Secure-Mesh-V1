//! Peer authorization: whether a node is *allowed here*, as distinct from
//! whether it is *who it says it is*.
//!
//! Phase 2 established **authentication**: the QUIC handshake proves a peer
//! holds the private key behind its node ID. That is a statement about
//! identity, and identity alone is not permission — anyone can generate a
//! keypair. Phase 2.5 adds **authorization**: an explicit, persisted, operator
//! decision that a particular cryptographic identity may participate.
//!
//! ```text
//!   UNKNOWN ──sends HELLO──▶ PENDING ──operator approves──▶ TRUSTED
//!      │                        │                              │
//!      └────────rejected────────┴──────────revoked─────────────┘
//!                               ▼
//!                            REVOKED ──operator reinstates──▶ TRUSTED
//! ```
//!
//! # Trust is bound to the key, not to anything a peer can rename
//!
//! Every decision is keyed by `node_id`, which is `SHA-256(public key)`. A peer
//! that changes its display name, IP address, or transport peer ID keeps
//! exactly the authorization it had. A peer that changes its *keypair* is a
//! different node ID, and therefore a different node requiring its own
//! decision — it does not inherit the old one, in either direction.
//!
//! # This is local policy, not a public key infrastructure
//!
//! A node's trust store records what **this node** will accept. It is closer to
//! SSH's `authorized_keys` than to a certificate authority: there is no issuer,
//! no chain, and no cross-node delegation. One node approving a peer says
//! nothing about what any other node accepts.
//!
//! That is a deliberate MVP boundary, and it has a real consequence documented
//! in `docs/security/SECURITY.md`: **revocation does not propagate**. Revoking a
//! peer on one node does not revoke it elsewhere.

use crate::error::{CoreError, CoreResult};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// Whether a peer is authorized to participate.
///
/// Distinct from `ConnectionState`, which says whether a peer is *reachable*.
/// A peer can be CONNECTED and REVOKED at the same time: the session exists,
/// but nothing is authorized to flow through it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum TrustState {
    /// Seen, but has made no enrollment request and has no decision recorded.
    /// The default for any node encountered — including one learned only as the
    /// origin of a relayed event.
    Unknown,
    /// Has presented itself and is awaiting an operator decision.
    Pending,
    /// Explicitly authorized by an operator.
    Trusted,
    /// Explicitly denied. Covers both "rejected at enrollment" and "previously
    /// trusted, then revoked" — the audit log distinguishes the two, while the
    /// enforced effect is identical.
    Revoked,
}

impl TrustState {
    pub fn as_str(self) -> &'static str {
        match self {
            TrustState::Unknown => "UNKNOWN",
            TrustState::Pending => "PENDING",
            TrustState::Trusted => "TRUSTED",
            TrustState::Revoked => "REVOKED",
        }
    }

    /// Whether this state permits authorized operations at all.
    ///
    /// The single predicate the sync engine consults. Written as an exhaustive
    /// match rather than `== Trusted` so that adding a state later forces a
    /// deliberate decision here instead of silently defaulting to "denied" or,
    /// worse, "allowed".
    pub fn permits_authorized_operations(self) -> bool {
        match self {
            TrustState::Trusted => true,
            TrustState::Unknown | TrustState::Pending | TrustState::Revoked => false,
        }
    }

    /// A short operator-facing explanation of what this state means in practice.
    pub fn describe(self) -> &'static str {
        match self {
            TrustState::Unknown => "Not enrolled — no data is exchanged",
            TrustState::Pending => "Enrollment required — awaiting approval",
            TrustState::Trusted => "Authorized for incident synchronisation",
            TrustState::Revoked => "Access denied — synchronisation refused",
        }
    }
}

impl fmt::Display for TrustState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for TrustState {
    type Err = CoreError;

    fn from_str(value: &str) -> CoreResult<Self> {
        match value.trim().to_ascii_uppercase().as_str() {
            "UNKNOWN" => Ok(TrustState::Unknown),
            "PENDING" => Ok(TrustState::Pending),
            "TRUSTED" => Ok(TrustState::Trusted),
            "REVOKED" => Ok(TrustState::Revoked),
            other => Err(CoreError::storage(format!(
                "database holds an unrecognised trust state: {other}"
            ))),
        }
    }
}

/// What a node is permitted to do.
///
/// Deliberately coarse. A finer-grained scheme would be more expressive and
/// much easier to get subtly wrong, and nothing in the current domain needs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Capability {
    /// Author incidents and observations locally.
    IncidentCreate,
    /// Read incidents held on this node.
    IncidentRead,
    /// Exchange the replicated event log with this node.
    IncidentSync,
    /// Take part in discovery and the handshake.
    PeerDiscover,
    /// Approve a peer's enrollment.
    PeerEnroll,
    /// Revoke or reinstate a peer.
    PeerRevoke,
}

impl Capability {
    pub fn as_str(self) -> &'static str {
        match self {
            Capability::IncidentCreate => "INCIDENT_CREATE",
            Capability::IncidentRead => "INCIDENT_READ",
            Capability::IncidentSync => "INCIDENT_SYNC",
            Capability::PeerDiscover => "PEER_DISCOVER",
            Capability::PeerEnroll => "PEER_ENROLL",
            Capability::PeerRevoke => "PEER_REVOKE",
        }
    }
}

impl fmt::Display for Capability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Capability {
    type Err = CoreError;

    fn from_str(value: &str) -> CoreResult<Self> {
        match value.trim().to_ascii_uppercase().as_str() {
            "INCIDENT_CREATE" => Ok(Capability::IncidentCreate),
            "INCIDENT_READ" => Ok(Capability::IncidentRead),
            "INCIDENT_SYNC" => Ok(Capability::IncidentSync),
            "PEER_DISCOVER" => Ok(Capability::PeerDiscover),
            "PEER_ENROLL" => Ok(Capability::PeerEnroll),
            "PEER_REVOKE" => Ok(Capability::PeerRevoke),
            other => Err(CoreError::validation(format!(
                "unrecognised capability: {other}"
            ))),
        }
    }
}

/// The role a node holds, which determines its capabilities.
///
/// Two roles, not a role *system*. Anything richer would be speculative: there
/// is no deployment yet whose needs it would be modelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum PeerRole {
    /// An ordinary field node: it synchronises, and nothing more.
    ///
    /// Explicitly **cannot** enroll or revoke. That is the property that stops
    /// an approved peer from turning itself, or anyone else, into an operator.
    Node,
    /// A node whose operator administers this device's trust store.
    Admin,
}

impl PeerRole {
    pub fn as_str(self) -> &'static str {
        match self {
            PeerRole::Node => "NODE",
            PeerRole::Admin => "ADMIN",
        }
    }

    /// The capabilities this role confers.
    ///
    /// Capabilities are derived from the role rather than stored per node, so
    /// there is no way for a stored capability list to drift out of step with
    /// the role it is supposed to reflect.
    pub fn capabilities(self) -> Vec<Capability> {
        let mut capabilities = vec![
            Capability::IncidentCreate,
            Capability::IncidentRead,
            Capability::IncidentSync,
            Capability::PeerDiscover,
        ];
        if self == PeerRole::Admin {
            capabilities.push(Capability::PeerEnroll);
            capabilities.push(Capability::PeerRevoke);
        }
        capabilities
    }

    pub fn grants(self, capability: Capability) -> bool {
        self.capabilities().contains(&capability)
    }
}

impl fmt::Display for PeerRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for PeerRole {
    type Err = CoreError;

    fn from_str(value: &str) -> CoreResult<Self> {
        match value.trim().to_ascii_uppercase().as_str() {
            "NODE" => Ok(PeerRole::Node),
            "ADMIN" => Ok(PeerRole::Admin),
            other => Err(CoreError::storage(format!(
                "database holds an unrecognised peer role: {other}"
            ))),
        }
    }
}

/// A security-relevant change to a peer's authorization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TrustEventKind {
    /// A peer presented itself and moved to PENDING.
    EnrollmentRequested,
    /// An operator approved a peer.
    EnrollmentApproved,
    /// An operator refused a peer that had never been trusted.
    EnrollmentRejected,
    /// An operator withdrew authorization from a previously trusted peer.
    Revoked,
    /// An operator restored authorization to a revoked peer.
    Reinstated,
}

impl TrustEventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            TrustEventKind::EnrollmentRequested => "peer.enrollment.requested",
            TrustEventKind::EnrollmentApproved => "peer.enrollment.approved",
            TrustEventKind::EnrollmentRejected => "peer.enrollment.rejected",
            TrustEventKind::Revoked => "peer.revoked",
            TrustEventKind::Reinstated => "peer.reinstated",
        }
    }
}

impl fmt::Display for TrustEventKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for TrustEventKind {
    type Err = CoreError;

    fn from_str(value: &str) -> CoreResult<Self> {
        match value.trim() {
            "peer.enrollment.requested" => Ok(TrustEventKind::EnrollmentRequested),
            "peer.enrollment.approved" => Ok(TrustEventKind::EnrollmentApproved),
            "peer.enrollment.rejected" => Ok(TrustEventKind::EnrollmentRejected),
            "peer.revoked" => Ok(TrustEventKind::Revoked),
            "peer.reinstated" => Ok(TrustEventKind::Reinstated),
            other => Err(CoreError::storage(format!(
                "database holds an unrecognised trust event: {other}"
            ))),
        }
    }
}

/// An entry in the local, append-only trust audit log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrustEvent {
    pub id: String,
    /// Local monotonic ordering. Wall-clock time is recorded for display but is
    /// not what orders the log, for the same reason it does not order the
    /// replicated event log: device clocks drift.
    pub sequence: u64,
    /// The peer the decision concerns.
    pub node_id: String,
    pub kind: TrustEventKind,
    pub from_state: Option<TrustState>,
    pub to_state: TrustState,
    /// The node whose operator made the decision. For locally made decisions
    /// this is the local node.
    pub actor_node: String,
    pub occurred_at: chrono::DateTime<chrono::Utc>,
    /// Optional operator note.
    pub detail: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_trusted_permits_authorized_operations() {
        assert!(TrustState::Trusted.permits_authorized_operations());
        assert!(!TrustState::Unknown.permits_authorized_operations());
        assert!(!TrustState::Pending.permits_authorized_operations());
        assert!(!TrustState::Revoked.permits_authorized_operations());
    }

    #[test]
    fn trust_state_round_trips_through_its_string_form() {
        for state in [
            TrustState::Unknown,
            TrustState::Pending,
            TrustState::Trusted,
            TrustState::Revoked,
        ] {
            assert_eq!(state.as_str().parse::<TrustState>().unwrap(), state);
        }
    }

    #[test]
    fn an_unrecognised_trust_state_is_a_storage_error() {
        assert_eq!(
            "SORT_OF_TRUSTED".parse::<TrustState>().unwrap_err().code(),
            "STORAGE_ERROR"
        );
    }

    #[test]
    fn trust_state_serialises_as_an_uppercase_label() {
        assert_eq!(
            serde_json::to_string(&TrustState::Trusted).unwrap(),
            r#""TRUSTED""#
        );
    }

    // --- Roles and capabilities -------------------------------------------

    #[test]
    fn an_ordinary_node_cannot_enroll_or_revoke() {
        // The property that stops an approved peer promoting itself or others.
        assert!(!PeerRole::Node.grants(Capability::PeerEnroll));
        assert!(!PeerRole::Node.grants(Capability::PeerRevoke));
    }

    #[test]
    fn an_ordinary_node_can_still_do_its_job() {
        for capability in [
            Capability::IncidentCreate,
            Capability::IncidentRead,
            Capability::IncidentSync,
            Capability::PeerDiscover,
        ] {
            assert!(
                PeerRole::Node.grants(capability),
                "{capability} should be granted"
            );
        }
    }

    #[test]
    fn an_admin_holds_every_capability() {
        for capability in [
            Capability::IncidentCreate,
            Capability::IncidentRead,
            Capability::IncidentSync,
            Capability::PeerDiscover,
            Capability::PeerEnroll,
            Capability::PeerRevoke,
        ] {
            assert!(
                PeerRole::Admin.grants(capability),
                "{capability} should be granted"
            );
        }
    }

    #[test]
    fn admin_capabilities_are_a_strict_superset_of_node_capabilities() {
        let node = PeerRole::Node.capabilities();
        let admin = PeerRole::Admin.capabilities();

        assert!(node.iter().all(|c| admin.contains(c)));
        assert!(admin.len() > node.len());
    }

    #[test]
    fn peer_role_round_trips_and_rejects_nonsense() {
        for role in [PeerRole::Node, PeerRole::Admin] {
            assert_eq!(role.as_str().parse::<PeerRole>().unwrap(), role);
        }
        assert!("SUPERUSER".parse::<PeerRole>().is_err());
    }

    #[test]
    fn capability_round_trips_and_rejects_nonsense() {
        for capability in PeerRole::Admin.capabilities() {
            assert_eq!(
                capability.as_str().parse::<Capability>().unwrap(),
                capability
            );
        }
        assert!("DO_ANYTHING".parse::<Capability>().is_err());
    }

    // --- Audit vocabulary --------------------------------------------------

    #[test]
    fn trust_event_names_are_stable_and_round_trip() {
        let expected = [
            (
                TrustEventKind::EnrollmentRequested,
                "peer.enrollment.requested",
            ),
            (
                TrustEventKind::EnrollmentApproved,
                "peer.enrollment.approved",
            ),
            (
                TrustEventKind::EnrollmentRejected,
                "peer.enrollment.rejected",
            ),
            (TrustEventKind::Revoked, "peer.revoked"),
            (TrustEventKind::Reinstated, "peer.reinstated"),
        ];

        for (kind, name) in expected {
            assert_eq!(kind.as_str(), name);
            assert_eq!(name.parse::<TrustEventKind>().unwrap(), kind);
        }
    }

    #[test]
    fn every_state_has_an_operator_facing_description() {
        for state in [
            TrustState::Unknown,
            TrustState::Pending,
            TrustState::Trusted,
            TrustState::Revoked,
        ] {
            assert!(!state.describe().is_empty());
        }
    }
}
