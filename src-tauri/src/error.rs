//! Central error type for the SecureMesh core.
//!
//! # Security note
//!
//! Error values cross the Tauri IPC boundary and reach the UI. They must
//! therefore never carry cryptographic secrets, key material, or file paths
//! that would help an attacker locate the keystore. Constructors in this
//! module take already-sanitised strings; the modules that build them are
//! responsible for keeping secrets out.

use serde::{Serialize, Serializer};
use std::fmt;

/// The single error type used across the SecureMesh core.
#[derive(Debug)]
pub enum CoreError {
    /// A local persistence operation failed.
    Storage(String),
    /// Identity generation, loading, or key handling failed.
    Identity(String),
    /// Caller-supplied input did not satisfy a domain invariant.
    Validation(String),
    /// The requested object does not exist locally.
    NotFound(String),
    /// An unexpected internal failure.
    Internal(String),
}

impl CoreError {
    /// Stable, machine-readable discriminant for the frontend to branch on.
    pub fn code(&self) -> &'static str {
        match self {
            CoreError::Storage(_) => "STORAGE_ERROR",
            CoreError::Identity(_) => "IDENTITY_ERROR",
            CoreError::Validation(_) => "VALIDATION_ERROR",
            CoreError::NotFound(_) => "NOT_FOUND",
            CoreError::Internal(_) => "INTERNAL_ERROR",
        }
    }

    /// Human-readable, secret-free description.
    pub fn message(&self) -> &str {
        match self {
            CoreError::Storage(m)
            | CoreError::Identity(m)
            | CoreError::Validation(m)
            | CoreError::NotFound(m)
            | CoreError::Internal(m) => m,
        }
    }

    pub fn storage(msg: impl Into<String>) -> Self {
        CoreError::Storage(msg.into())
    }

    pub fn identity(msg: impl Into<String>) -> Self {
        CoreError::Identity(msg.into())
    }

    pub fn validation(msg: impl Into<String>) -> Self {
        CoreError::Validation(msg.into())
    }

    pub fn not_found(msg: impl Into<String>) -> Self {
        CoreError::NotFound(msg.into())
    }

    pub fn internal(msg: impl Into<String>) -> Self {
        CoreError::Internal(msg.into())
    }
}

impl fmt::Display for CoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code(), self.message())
    }
}

impl std::error::Error for CoreError {}

/// Serialised to the frontend as `{ "code": ..., "message": ... }` so the UI
/// can render a stable code without parsing prose.
impl Serialize for CoreError {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut state = serializer.serialize_struct("CoreError", 2)?;
        state.serialize_field("code", self.code())?;
        state.serialize_field("message", self.message())?;
        state.end()
    }
}

impl From<rusqlite::Error> for CoreError {
    fn from(err: rusqlite::Error) -> Self {
        // rusqlite messages describe SQL and schema, never bound secret values,
        // because key material is never passed as a statement parameter.
        CoreError::Storage(err.to_string())
    }
}

impl From<serde_json::Error> for CoreError {
    fn from(err: serde_json::Error) -> Self {
        // Deliberately drops the payload excerpt serde_json may include, since
        // the keystore file is itself parsed as JSON.
        CoreError::Internal(format!("serialization failure ({})", err.classify_str()))
    }
}

/// Small helper so `From<serde_json::Error>` can describe the failure class
/// without echoing any of the offending document back.
trait ClassifyStr {
    fn classify_str(&self) -> &'static str;
}

impl ClassifyStr for serde_json::Error {
    fn classify_str(&self) -> &'static str {
        use serde_json::error::Category;
        match self.classify() {
            Category::Io => "io",
            Category::Syntax => "syntax",
            Category::Data => "data",
            Category::Eof => "eof",
        }
    }
}

/// Convenience alias used throughout the core.
pub type CoreResult<T> = Result<T, CoreError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_stable() {
        assert_eq!(CoreError::validation("x").code(), "VALIDATION_ERROR");
        assert_eq!(CoreError::not_found("x").code(), "NOT_FOUND");
        assert_eq!(CoreError::storage("x").code(), "STORAGE_ERROR");
        assert_eq!(CoreError::identity("x").code(), "IDENTITY_ERROR");
        assert_eq!(CoreError::internal("x").code(), "INTERNAL_ERROR");
    }

    #[test]
    fn serializes_as_code_and_message() {
        let json = serde_json::to_string(&CoreError::validation("bad severity")).unwrap();
        assert_eq!(
            json,
            r#"{"code":"VALIDATION_ERROR","message":"bad severity"}"#
        );
    }

    #[test]
    fn display_includes_code() {
        assert_eq!(
            CoreError::not_found("incident").to_string(),
            "NOT_FOUND: incident"
        );
    }
}
