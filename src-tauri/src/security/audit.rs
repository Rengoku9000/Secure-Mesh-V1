//! Minimal audit trail for security-relevant operations.
//!
//! Audit records are written to stderr as single-line, structured text. They
//! record *that* a sensitive operation happened, never the material it acted
//! on: there is no code path here that accepts key bytes.
//!
//! This is deliberately simple. A durable, tamper-evident audit log is a
//! Phase 5 concern and is listed as a known limitation in
//! `docs/security/SECURITY.md`.

use chrono::Utc;
use std::fmt;

/// The class of security-relevant operation being recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditEvent {
    /// A new node identity keypair was generated.
    IdentityCreated,
    /// An existing node identity was loaded from the keystore.
    IdentityLoaded,
    /// The public half of the identity was released to the UI.
    PublicIdentityDisclosed,
    /// The local database was opened and migrated.
    DatabaseOpened,
    /// An incident record was written locally.
    IncidentCreated,
    /// A peer's authorization changed: enrolled, approved, rejected, revoked,
    /// or reinstated.
    PeerTrustChanged,
    /// An operation was refused because the caller lacked authorization.
    AuthorizationDenied,
}

impl AuditEvent {
    fn as_str(self) -> &'static str {
        match self {
            AuditEvent::IdentityCreated => "identity.created",
            AuditEvent::IdentityLoaded => "identity.loaded",
            AuditEvent::PublicIdentityDisclosed => "identity.public_disclosed",
            AuditEvent::DatabaseOpened => "storage.database_opened",
            AuditEvent::IncidentCreated => "incident.created",
            AuditEvent::PeerTrustChanged => "peer.trust_changed",
            AuditEvent::AuthorizationDenied => "authorization.denied",
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

/// Records a security-relevant operation.
///
/// `detail` must be a short, non-sensitive description chosen by the caller.
/// Never pass key material, passphrases, or raw user payloads.
pub fn audit(event: AuditEvent, outcome: AuditOutcome, detail: &str) {
    eprintln!(
        "[audit] ts={} event={} outcome={} detail={}",
        Utc::now().to_rfc3339(),
        event.as_str(),
        outcome.as_str(),
        sanitize(detail)
    );
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
}
