//! Cross-cutting security primitives for the SecureMesh core.
//!
//! This module deliberately contains only mechanisms that are real. Anything
//! that is *not* yet a genuine security control is documented as such in
//! `docs/security/SECURITY.md` rather than being simulated here.

pub mod audit;
pub mod secret;

pub use audit::{audit, AuditEvent, AuditOutcome};
pub use secret::Secret;
