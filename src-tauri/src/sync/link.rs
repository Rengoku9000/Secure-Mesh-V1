//! The per-peer synchronisation lifecycle.
//!
//! Phase 2.5 had no explicit lifecycle. A peer was either in a `connected` map
//! or not, and whether replication actually started depended on a periodic
//! timer happening to come round after every precondition was met. That is a
//! retry loop standing in for a trigger, and it fails exactly the way it failed
//! in the field: everything looks correct and nothing moves.
//!
//! ```text
//!   Disconnected
//!        │ transport reports a session
//!        ▼
//!   Connected ──────────────┐
//!        │ HELLO exchanged  │ peer not authorized
//!        ▼                  ▼
//!   Authenticated ──▶ AwaitingAuthorization
//!        │ operator approves (either side)
//!        ▼
//!   Authorized
//!        │ knowledge exchanged
//!        ▼
//!   Syncing ──▶ Synced
//! ```
//!
//! The state exists to answer one question precisely: **is there outstanding
//! work with this peer, and if not, why not?** `AwaitingAuthorization` and
//! `Synced` are both "idle", but only one of them is a problem an operator can
//! do something about.
//!
//! This is in-memory on purpose. It describes a live session, and no session
//! survives a process exit — persisting it would leave stale rows claiming a
//! peer was mid-sync after a crash. Everything durable lives in the event log
//! and the trust store.

use crate::domain::TrustState;
use serde::Serialize;
use std::fmt;
use std::time::Instant;

/// Where a peer has reached in the synchronisation lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum LinkState {
    /// No session. The peer may still be known and trusted.
    Disconnected,
    /// The transport reports an authenticated session, but no SecureMesh
    /// handshake has completed yet.
    Connected,
    /// Handshake complete: protocol version and identity confirmed.
    Authenticated,
    /// Authenticated, but not authorized by this node. Idle *and* actionable —
    /// an operator decision is what unblocks it.
    AwaitingAuthorization,
    /// Authorized, with a sync round opened and not yet finished.
    Syncing,
    /// Authorized, with nothing outstanding in either direction.
    Synced,
}

impl LinkState {
    pub fn as_str(self) -> &'static str {
        match self {
            LinkState::Disconnected => "DISCONNECTED",
            LinkState::Connected => "CONNECTED",
            LinkState::Authenticated => "AUTHENTICATED",
            LinkState::AwaitingAuthorization => "AWAITING_AUTHORIZATION",
            LinkState::Syncing => "SYNCING",
            LinkState::Synced => "SYNCED",
        }
    }

    /// Whether a sync round may be opened in this state.
    pub fn permits_sync(self) -> bool {
        matches!(self, LinkState::Syncing | LinkState::Synced)
    }
}

impl fmt::Display for LinkState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Why a synchronisation round was opened.
///
/// Recorded so that "what caused this round" is answerable from the logs. If a
/// round can only ever be attributed to [`SyncTrigger::Reconciliation`], the
/// system is relying on the sweep rather than on a cause, which is the very
/// condition Phase 2.6 exists to remove.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SyncTrigger {
    /// A session was established, or re-established after a drop.
    Connected,
    /// This node authorized the peer.
    LocalAuthorization,
    /// A local event was appended and needs to travel.
    LocalEvent,
    /// The peer's announced knowledge is ahead of this node's.
    PeerAhead,
    /// This node accepted events from one peer and other peers may lack them.
    /// Without this a relay would hold what it learned until something else
    /// happened, and a multi-hop path would never complete on its own.
    Relay,
    /// An operator or a command asked explicitly.
    Manual,
    /// The periodic safety net. Should be rare; see `sync::RECONCILE_INTERVAL`.
    Reconciliation,
}

impl SyncTrigger {
    pub fn as_str(self) -> &'static str {
        match self {
            SyncTrigger::Connected => "connected",
            SyncTrigger::LocalAuthorization => "local_authorization",
            SyncTrigger::LocalEvent => "local_event",
            SyncTrigger::PeerAhead => "peer_ahead",
            SyncTrigger::Relay => "relay",
            SyncTrigger::Manual => "manual",
            SyncTrigger::Reconciliation => "reconciliation",
        }
    }
}

impl fmt::Display for SyncTrigger {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Everything known about one live peer session.
pub struct PeerLink {
    pub node_id: String,
    pub public_key: String,
    pub transport_peer_id: String,
    pub state: LinkState,
    /// Protocol version the peer announced, once it has.
    pub protocol_version: Option<u16>,
    /// When the session was established, for latency measurement.
    pub connected_at: Instant,
    /// When the current round started, if one is open.
    pub round_started_at: Option<Instant>,
    /// What caused the current round.
    pub round_trigger: Option<SyncTrigger>,
    /// Rounds opened on this session.
    pub rounds: u64,
    /// Events accepted from this peer on this session.
    pub events_received: u64,
}

impl PeerLink {
    pub fn new(node_id: String, public_key: String, transport_peer_id: String) -> Self {
        Self {
            node_id,
            public_key,
            transport_peer_id,
            state: LinkState::Connected,
            protocol_version: None,
            connected_at: Instant::now(),
            round_started_at: None,
            round_trigger: None,
            rounds: 0,
            events_received: 0,
        }
    }

    /// Folds the current authorization decision into the lifecycle.
    ///
    /// Called wherever trust might have changed, so the state cannot drift away
    /// from the trust store. It never moves *backwards* out of a sync state on
    /// its own — losing authorization does, which is the point.
    pub fn apply_trust(&mut self, trust: TrustState) {
        match (self.state, trust.permits_authorized_operations()) {
            // Authorized while waiting: ready to sync.
            (LinkState::AwaitingAuthorization, true) | (LinkState::Authenticated, true) => {
                self.state = LinkState::Synced;
            }
            // Authorization withdrawn mid-session: stop, but stay connected.
            (LinkState::Syncing | LinkState::Synced, false) => {
                self.state = LinkState::AwaitingAuthorization;
                self.round_started_at = None;
                self.round_trigger = None;
            }
            (LinkState::Authenticated, false) => {
                self.state = LinkState::AwaitingAuthorization;
            }
            _ => {}
        }
    }

    /// Marks the handshake complete.
    pub fn authenticated(&mut self, protocol_version: u16, trust: TrustState) {
        self.protocol_version = Some(protocol_version);
        if self.state == LinkState::Connected {
            self.state = LinkState::Authenticated;
        }
        self.apply_trust(trust);
    }

    /// Opens a round, returning false if one is already open for this trigger.
    pub fn begin_round(&mut self, trigger: SyncTrigger) -> bool {
        if !self.state.permits_sync() {
            return false;
        }
        self.state = LinkState::Syncing;
        self.round_started_at = Some(Instant::now());
        self.round_trigger = Some(trigger);
        self.rounds += 1;
        true
    }

    /// Closes the current round.
    pub fn complete_round(&mut self) -> Option<u128> {
        if self.state != LinkState::Syncing {
            return None;
        }
        self.state = LinkState::Synced;
        let elapsed = self.round_started_at.take().map(|t| t.elapsed().as_millis());
        self.round_trigger = None;
        elapsed
    }
}

/// A peer session as reported to the UI and the logs.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkSnapshot {
    pub node_id: String,
    pub state: LinkState,
    pub protocol_version: Option<u16>,
    /// Milliseconds since the session was established.
    pub connected_for_ms: u128,
    pub rounds: u64,
    pub events_received: u64,
}

impl From<&PeerLink> for LinkSnapshot {
    fn from(link: &PeerLink) -> Self {
        Self {
            node_id: link.node_id.clone(),
            state: link.state,
            protocol_version: link.protocol_version,
            connected_for_ms: link.connected_at.elapsed().as_millis(),
            rounds: link.rounds,
            events_received: link.events_received,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link() -> PeerLink {
        PeerLink::new("a".repeat(64), "b".repeat(64), "loopback:a".to_string())
    }

    #[test]
    fn a_new_link_is_connected_but_not_yet_authenticated() {
        let link = link();
        assert_eq!(link.state, LinkState::Connected);
        assert!(!link.state.permits_sync());
    }

    #[test]
    fn an_unauthorized_peer_waits_rather_than_syncing() {
        let mut link = link();
        link.authenticated(1, TrustState::Pending);

        assert_eq!(link.state, LinkState::AwaitingAuthorization);
        assert!(!link.state.permits_sync());
    }

    #[test]
    fn authorizing_a_waiting_peer_makes_it_ready() {
        let mut link = link();
        link.authenticated(1, TrustState::Pending);
        link.apply_trust(TrustState::Trusted);

        assert_eq!(link.state, LinkState::Synced);
        assert!(link.state.permits_sync());
    }

    #[test]
    fn revoking_mid_session_stops_synchronisation_without_disconnecting() {
        let mut link = link();
        link.authenticated(1, TrustState::Trusted);
        assert!(link.begin_round(SyncTrigger::Connected));

        link.apply_trust(TrustState::Revoked);

        assert_eq!(link.state, LinkState::AwaitingAuthorization);
        assert!(!link.state.permits_sync());
        assert!(link.round_started_at.is_none(), "the open round is abandoned");
    }

    #[test]
    fn an_unauthorized_link_refuses_to_open_a_round() {
        let mut link = link();
        link.authenticated(1, TrustState::Revoked);

        assert!(!link.begin_round(SyncTrigger::LocalEvent));
        assert_eq!(link.state, LinkState::AwaitingAuthorization);
    }

    #[test]
    fn a_round_records_its_trigger_and_measures_its_duration() {
        let mut link = link();
        link.authenticated(1, TrustState::Trusted);

        assert!(link.begin_round(SyncTrigger::LocalAuthorization));
        assert_eq!(link.round_trigger, Some(SyncTrigger::LocalAuthorization));
        assert_eq!(link.state, LinkState::Syncing);

        let elapsed = link.complete_round();
        assert!(elapsed.is_some(), "a completed round reports its latency");
        assert_eq!(link.state, LinkState::Synced);
        assert_eq!(link.rounds, 1);
    }

    #[test]
    fn completing_a_round_that_was_never_open_is_harmless() {
        let mut link = link();
        link.authenticated(1, TrustState::Trusted);
        assert!(link.complete_round().is_none());
    }

    #[test]
    fn reauthorising_after_revocation_restores_readiness() {
        let mut link = link();
        link.authenticated(1, TrustState::Trusted);
        link.apply_trust(TrustState::Revoked);
        assert!(!link.state.permits_sync());

        link.apply_trust(TrustState::Trusted);
        assert!(link.state.permits_sync(), "reinstatement resumes without reconnecting");
    }

    #[test]
    fn trigger_and_state_labels_are_stable() {
        assert_eq!(LinkState::AwaitingAuthorization.as_str(), "AWAITING_AUTHORIZATION");
        assert_eq!(SyncTrigger::LocalAuthorization.as_str(), "local_authorization");
        assert_eq!(SyncTrigger::Reconciliation.as_str(), "reconciliation");
    }
}
