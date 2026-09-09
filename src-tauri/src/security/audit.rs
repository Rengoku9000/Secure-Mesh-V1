//! Minimal audit trail for security-relevant operations.
//!
//! Audit records are written to stderr as single-line, structured text. They
//! record *that* a sensitive operation happened, never the material it acted
//! on: there is no code path here that accepts key bytes.
//!
//! # What belongs here
//!
//! A **state change or a decision**: a key generated, a peer authorized, a
//! record written, an operation refused. Not a read.
//!
//! That distinction was learned the hard way. Reading the node's public
//! identity used to emit a record, which sounded prudent until the dashboard
//! polled it twice every two seconds — roughly 86,000 lines a day saying
//! nothing happened. An audit log that has to be filtered before it can be read
//! is not an audit log, and the noise would have buried the events that matter.
//! Reads are silent here for that reason, and adding one back needs a better
//! argument than "identity sounds sensitive".
//!
//! This is deliberately simple. A durable, tamper-evident audit log is a
//! Phase 5 concern and is listed as a known limitation in
//! `docs/security/SECURITY.md`.

use chrono::Utc;
use std::cell::RefCell;
use std::fmt;

/// The class of security-relevant operation being recorded.
///
/// Every variant is a change or a refusal. There is deliberately no variant for
/// *observing* state — see the module note.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditEvent {
    /// A new node identity keypair was generated.
    IdentityCreated,
    /// An existing node identity was loaded from the keystore.
    IdentityLoaded,
    /// The local database was opened and migrated.
    DatabaseOpened,
    /// An incident record was written locally.
    IncidentCreated,
    /// A peer's authorization changed: enrolled, approved, rejected, revoked,
    /// or reinstated.
    PeerTrustChanged,
    /// An operation was refused because the caller lacked authorization.
    AuthorizationDenied,
    /// Reference knowledge was provisioned into the local index.
    ///
    /// Audited because it changes what the node will answer questions from, and
    /// "where did this text come from?" must be answerable after the fact.
    KnowledgeInstalled,
    /// A peer reported a position for the first time, or after its previous
    /// one had expired.
    ///
    /// Only the transition is recorded. A heartbeat every five minutes is an
    /// observation, not a security decision, and logging each one would bury
    /// every real event under thousands of routine lines.
    PeerLocationAvailable,
    /// A peer's reported position aged past the point of being usable.
    PeerLocationExpired,
}

impl AuditEvent {
    fn as_str(self) -> &'static str {
        match self {
            AuditEvent::IdentityCreated => "identity.created",
            AuditEvent::IdentityLoaded => "identity.loaded",
            AuditEvent::DatabaseOpened => "storage.database_opened",
            AuditEvent::IncidentCreated => "incident.created",
            AuditEvent::PeerTrustChanged => "peer.trust_changed",
            AuditEvent::AuthorizationDenied => "authorization.denied",
            AuditEvent::KnowledgeInstalled => "knowledge.installed",
            AuditEvent::PeerLocationAvailable => "peer.location_available",
            AuditEvent::PeerLocationExpired => "peer.location_expired",
        }
    }
}

impl fmt::Display for AuditEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Whether the audited operation succeeded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditOutcome {
    Success,
    Failure,
}

impl AuditOutcome {
    fn as_str(self) -> &'static str {
        match self {
            AuditOutcome::Success => "success",
            AuditOutcome::Failure => "failure",
        }
    }
}

thread_local! {
    /// Records emitted on this thread, when something is watching.
    ///
    /// Thread-local rather than global so that tests running in parallel cannot
    /// observe each other's records — with a shared counter, asserting "this
    /// operation emitted nothing" would fail whenever an unrelated test
    /// happened to log at the same moment.
    static CAPTURE: RefCell<Option<Vec<(AuditEvent, String)>>> = const { RefCell::new(None) };
}

/// Records a security-relevant operation.
///
/// `detail` must be a short, non-sensitive description chosen by the caller.
/// Never pass key material, passphrases, or raw user payloads.
pub fn audit(event: AuditEvent, outcome: AuditOutcome, detail: &str) {
    let detail = sanitize(detail);

    let captured = CAPTURE.with(|capture| match capture.borrow_mut().as_mut() {
        Some(records) => {
            records.push((event, detail.clone()));
            true
        }
        None => false,
    });

    if !captured {
        eprintln!(
            "[audit] ts={} event={} outcome={} detail={}",
            Utc::now().to_rfc3339(),
            event.as_str(),
            outcome.as_str(),
            detail
        );
    }
}

/// Collects the audit records `body` emits on this thread.
///
/// Exists so that "this operation emits no audit record" is a claim a test can
/// *check* rather than assert by reading the source. Only the calling thread is
/// observed, and records collected here are not also written to stderr.
///
/// If `body` unwinds, capture stays armed on that thread until the next call,
/// which would divert later records away from stderr. That is tolerable because
/// this is a test seam — a panicking test has already failed, each test runs on
/// its own thread, and the next `capture` resets the buffer.
pub fn capture<T>(body: impl FnOnce() -> T) -> (T, Vec<(AuditEvent, String)>) {
    CAPTURE.with(|capture| *capture.borrow_mut() = Some(Vec::new()));
    let value = body();
    let records = CAPTURE.with(|capture| capture.borrow_mut().take().unwrap_or_default());
    (value, records)
}

/// Collapses whitespace and truncates, so a crafted detail string cannot forge
/// extra audit lines or flood the log.
fn sanitize(detail: &str) -> String {
    const MAX_DETAIL: usize = 200;
    let collapsed: String = detail
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let trimmed = collapsed.trim();
    if trimmed.chars().count() > MAX_DETAIL {
        let truncated: String = trimmed.chars().take(MAX_DETAIL).collect();
        format!("{}...", truncated)
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_names_are_stable() {
        assert_eq!(AuditEvent::IdentityCreated.to_string(), "identity.created");
        assert_eq!(AuditEvent::IncidentCreated.to_string(), "incident.created");
    }

    #[test]
    fn newlines_cannot_forge_additional_audit_lines() {
        let forged = sanitize("ok\n[audit] ts=1 event=identity.created outcome=success");
        assert!(!forged.contains('\n'));
    }

    #[test]
    fn long_details_are_truncated() {
        let sanitized = sanitize(&"a".repeat(500));
        assert!(sanitized.chars().count() <= 203);
        assert!(sanitized.ends_with("..."));
    }

    #[test]
    fn control_characters_are_stripped() {
        assert_eq!(sanitize("a\tb\rc"), "a b c");
    }

    // --- Capture -----------------------------------------------------------

    #[test]
    fn capture_collects_what_was_emitted() {
        let ((), records) = capture(|| {
            audit(AuditEvent::IdentityCreated, AuditOutcome::Success, "node=A");
            audit(AuditEvent::IncidentCreated, AuditOutcome::Success, "id=1");
        });

        assert_eq!(records.len(), 2);
        assert_eq!(records[0].0, AuditEvent::IdentityCreated);
        assert_eq!(records[0].1, "node=A");
        assert_eq!(records[1].0, AuditEvent::IncidentCreated);
    }

    #[test]
    fn capture_sees_nothing_when_nothing_is_emitted() {
        // The shape every "this is silent now" test relies on.
        let ((), records) = capture(|| {});
        assert!(records.is_empty());
    }

    #[test]
    fn capture_stops_at_the_end_of_the_block() {
        let ((), first) = capture(|| {
            audit(AuditEvent::DatabaseOpened, AuditOutcome::Success, "v=1");
        });
        assert_eq!(first.len(), 1);

        // A later capture on the same thread starts empty rather than
        // inheriting the previous block's records.
        let ((), second) = capture(|| {});
        assert!(second.is_empty());
    }

    #[test]
    fn captured_details_are_sanitised_like_written_ones() {
        // Otherwise a test could pass on a detail string that the real writer
        // would have mangled.
        let ((), records) = capture(|| {
            audit(AuditEvent::IncidentCreated, AuditOutcome::Success, "a\nb");
        });
        assert_eq!(records[0].1, "a b");
    }
}
