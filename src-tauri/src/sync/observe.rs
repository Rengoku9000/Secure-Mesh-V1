//! Structured synchronisation logging.
//!
//! The Phase 2.5 failure was hard to diagnose for one reason: from outside the
//! process, "connected and idle because everything is up to date" and
//! "connected and idle because nothing ever started" looked identical. These
//! records exist to make that distinction visible without a debugger.
//!
//! Each event is one line with a stable `sync.*` name and key=value fields, so
//! it can be grepped or parsed without a log framework.
//!
//! # What is never logged
//!
//! No key material, and no incident payloads. Events are identified by ID,
//! origin and sequence number — enough to reconstruct *what moved* without
//! writing operational content into a log file that has none of the event
//! log's protections.

use crate::domain::TrustState;
use crate::sync::link::SyncTrigger;
use chrono::Utc;

/// A synchronisation observation.
///
/// Borrowed rather than owned throughout: these are emitted on the hot path and
/// should cost nothing beyond formatting.
pub enum SyncLog<'a> {
    /// A transport session was established.
    Connected { peer: &'a str },
    /// The session ended.
    Disconnected { peer: &'a str },
    /// Handshake complete: version and identity confirmed.
    Authenticated {
        peer: &'a str,
        protocol_version: u16,
        state: TrustState,
    },
    /// The peer's authorization state changed locally.
    Authorization { peer: &'a str, state: TrustState },
    /// A round was opened, and why.
    Started { peer: &'a str, trigger: SyncTrigger },
    /// What this node told the peer it holds.
    LocalKnowledge {
        peer: &'a str,
        origins: usize,
        events: u64,
    },
    /// What the peer said it holds.
    RemoteKnowledge { peer: &'a str, origins: usize },
    /// The peer holds something this node lacks.
    PeerAhead { peer: &'a str },
    /// Events handed to a peer.
    Sent {
        peer: &'a str,
        origin: &'a str,
        count: usize,
    },
    /// An event was accepted and applied.
    Applied {
        peer: &'a str,
        origin: &'a str,
        sequence: u64,
    },
    /// An event was refused, with the reason.
    EventRejected {
        peer: &'a str,
        origin: &'a str,
        reason: &'a str,
    },
    /// A message was refused, with the reason.
    Rejected { peer: &'a str, reason: &'a str },
    /// A round finished, with its wall-clock duration.
    Completed {
        peer: &'a str,
        applied: usize,
        duration_ms: u128,
    },
    /// Something went wrong that was not a peer's fault.
    Failed { peer: &'a str, reason: &'a str },
}

impl SyncLog<'_> {
    fn name(&self) -> &'static str {
        match self {
            SyncLog::Connected { .. } => "sync.connected",
            SyncLog::Disconnected { .. } => "sync.disconnected",
            SyncLog::Authenticated { .. } => "sync.authenticated",
            SyncLog::Authorization { .. } => "sync.authorization",
            SyncLog::Started { .. } => "sync.started",
            SyncLog::LocalKnowledge { .. } => "sync.local_knowledge",
            SyncLog::RemoteKnowledge { .. } => "sync.remote_knowledge",
            SyncLog::PeerAhead { .. } => "sync.peer_ahead",
            SyncLog::Sent { .. } => "sync.sent",
            SyncLog::Applied { .. } => "sync.event_applied",
            SyncLog::EventRejected { .. } => "sync.event_rejected",
            SyncLog::Rejected { .. } => "sync.rejected",
            SyncLog::Completed { .. } => "sync.completed",
            SyncLog::Failed { .. } => "sync.failed",
        }
    }

    fn fields(&self) -> String {
        match self {
            SyncLog::Connected { peer }
            | SyncLog::Disconnected { peer }
            | SyncLog::PeerAhead { peer } => format!("peer={}", short(peer)),
            SyncLog::Authenticated {
                peer,
                protocol_version,
                state,
            } => format!(
                "peer={} protocol={protocol_version} trust={state}",
                short(peer)
            ),
            SyncLog::Authorization { peer, state } => {
                format!("peer={} trust={state}", short(peer))
            }
            SyncLog::Started { peer, trigger } => {
                format!("peer={} trigger={trigger}", short(peer))
            }
            SyncLog::LocalKnowledge {
                peer,
                origins,
                events,
            } => {
                format!("peer={} origins={origins} events={events}", short(peer))
            }
            SyncLog::RemoteKnowledge { peer, origins } => {
                format!("peer={} origins={origins}", short(peer))
            }
            SyncLog::Sent {
                peer,
                origin,
                count,
            } => format!(
                "peer={} origin={} count={count}",
                short(peer),
                short(origin)
            ),
            SyncLog::Applied {
                peer,
                origin,
                sequence,
            } => format!(
                "peer={} origin={} seq={sequence}",
                short(peer),
                short(origin)
            ),
            SyncLog::EventRejected {
                peer,
                origin,
                reason,
            } => format!(
                "peer={} origin={} reason={}",
                short(peer),
                short(origin),
                sanitize(reason)
            ),
            SyncLog::Rejected { peer, reason } => {
                format!("peer={} reason={}", short(peer), sanitize(reason))
            }
            SyncLog::Completed {
                peer,
                applied,
                duration_ms,
            } => format!(
                "peer={} applied={applied} duration_ms={duration_ms}",
                short(peer)
            ),
            SyncLog::Failed { peer, reason } => {
                format!("peer={} reason={}", short(peer), sanitize(reason))
            }
        }
    }
}

/// Node IDs are 64 hex characters; the first 8 identify a peer unambiguously
/// in practice and keep a log line readable.
fn short(node_id: &str) -> &str {
    if node_id.len() > 8 {
        &node_id[..8]
    } else {
        node_id
    }
}

/// Keeps a reason on one line so it cannot forge additional records.
fn sanitize(reason: &str) -> String {
    let cleaned: String = reason
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(160)
        .collect();
    format!("\"{}\"", cleaned.trim())
}

/// Emits one observation.
pub fn observe(event: SyncLog<'_>) {
    eprintln!(
        "[sync] ts={} event={} {}",
        Utc::now().to_rfc3339(),
        event.name(),
        event.fields()
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_names_are_stable() {
        assert_eq!(
            SyncLog::Started {
                peer: "abc",
                trigger: SyncTrigger::LocalAuthorization
            }
            .name(),
            "sync.started"
        );
        assert_eq!(
            SyncLog::Completed {
                peer: "abc",
                applied: 0,
                duration_ms: 0
            }
            .name(),
            "sync.completed"
        );
    }

    #[test]
    fn node_ids_are_shortened_for_readability() {
        assert_eq!(short(&"a".repeat(64)), "aaaaaaaa");
        assert_eq!(short("short"), "short");
    }

    #[test]
    fn a_reason_cannot_forge_extra_log_records() {
        let forged = sanitize("denied\n[sync] ts=1 event=sync.completed applied=99");
        assert!(!forged.contains('\n'));
    }

    #[test]
    fn a_long_reason_is_truncated() {
        assert!(sanitize(&"x".repeat(500)).len() <= 164);
    }

    #[test]
    fn a_completed_round_reports_its_duration_and_count() {
        let fields = SyncLog::Completed {
            peer: &"c".repeat(64),
            applied: 3,
            duration_ms: 142,
        }
        .fields();

        assert!(fields.contains("applied=3"));
        assert!(fields.contains("duration_ms=142"));
    }

    #[test]
    fn the_trigger_is_recorded_so_a_round_can_be_attributed() {
        for trigger in [
            SyncTrigger::Connected,
            SyncTrigger::LocalAuthorization,
            SyncTrigger::LocalEvent,
            SyncTrigger::PeerAhead,
            SyncTrigger::Manual,
            SyncTrigger::Reconciliation,
        ] {
            let fields = SyncLog::Started {
                peer: "abc",
                trigger,
            }
            .fields();
            assert!(fields.contains(&format!("trigger={trigger}")));
        }
    }
}
