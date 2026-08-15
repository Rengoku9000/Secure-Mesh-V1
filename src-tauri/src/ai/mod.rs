//! Local intelligence.
//!
//! Everything here runs on this device. There is no API key, no endpoint to
//! configure, and no request that leaves the machine — the inference runtime is
//! a local process reached over loopback, and the models are files an operator
//! provisioned.
//!
//! ```text
//!            application / runtime
//!                     │
//!        ┌────────────┴────────────┐
//!        │                         │
//!  LocalInferenceEngine     EmbeddingEngine        (traits)
//!        │                         │
//!        └────────────┬────────────┘
//!                     │
//!            LlamaServerEngine                     (today)
//!            TeeInferenceEngine                    (Phase 5, not built)
//! ```
//!
//! # The AI trust boundary
//!
//! The inference layer is given **only** the text it needs to reason about, and
//! it can return **only** text. It holds no handle to the identity module, the
//! trust store, the sync engine, or the filesystem. A model cannot enrol a
//! peer, revoke one, sign anything, or read a key, because nothing in this
//! module is wired to those things — the restriction is structural, not a rule
//! someone has to remember. See `docs/security/SECURITY.md`.
//!
//! # AI is optional
//!
//! Every entry point degrades. A node with no model provisioned, or whose
//! runtime failed to start, keeps creating incidents, keeps synchronising, and
//! reports its AI subsystem as unavailable. Intelligence is a layer on top of
//! SecureMesh, never a dependency of it.

pub mod dataset;
pub mod embedding;
pub mod engine;
pub mod evaluation;
pub mod indexer;
pub mod llama;
pub mod loopback_http;
pub mod prompt;
pub mod rag;
pub mod service;

pub use embedding::{Embedding, EmbeddingEngine};
pub use engine::{
    EngineHealth, GenerationRequest, LocalInferenceEngine, ModelInfo, StructuredRequest,
};
pub use indexer::{BackgroundIndexer, IncidentIndexState, IndexState};
pub use llama::{LlamaConfig, LlamaServerEngine};
pub use rag::GroundedAnswer;
pub use service::{IndexReport, IntelligenceService, IntelligenceStatus};

use crate::error::CoreError;

/// Why the intelligence layer is unavailable, when it is.
///
/// Distinguished so the dashboard can tell an operator something actionable —
/// "no model installed" and "the runtime crashed" call for different responses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unavailable {
    /// No model file at the configured path.
    ModelMissing(String),
    /// No inference runtime binary.
    RuntimeMissing(String),
    /// The runtime is present but did not start or stopped responding.
    RuntimeFailed(String),
    /// Deliberately switched off for this node.
    Disabled,
}

impl Unavailable {
    /// A short, operator-facing explanation.
    pub fn detail(&self) -> String {
        match self {
            Unavailable::ModelMissing(path) => {
                format!("No model file at {path}. See docs/ai/PROVISIONING.md.")
            }
            Unavailable::RuntimeMissing(path) => {
                format!("No inference runtime at {path}. See docs/ai/PROVISIONING.md.")
            }
            Unavailable::RuntimeFailed(reason) => {
                format!("The local inference runtime is not responding: {reason}")
            }
            Unavailable::Disabled => "Local intelligence is switched off on this node.".to_string(),
        }
    }
}

impl From<Unavailable> for CoreError {
    fn from(value: Unavailable) -> Self {
        CoreError::internal(value.detail())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_unavailable_reason_explains_itself() {
        let reasons = [
            Unavailable::ModelMissing("ai/models/llm/x.gguf".to_string()),
            Unavailable::RuntimeMissing("ai/runtime/x.exe".to_string()),
            Unavailable::RuntimeFailed("exited".to_string()),
            Unavailable::Disabled,
        ];

        for reason in reasons {
            let detail = reason.detail();
            assert!(!detail.is_empty());
            // An operator needs to know what to do, so the two provisioning
            // cases point at the instructions.
            if matches!(
                reason,
                Unavailable::ModelMissing(_) | Unavailable::RuntimeMissing(_)
            ) {
                assert!(detail.contains("PROVISIONING"));
            }
        }
    }

    #[test]
    fn unavailability_converts_to_a_core_error_without_losing_the_reason() {
        let error: CoreError = Unavailable::ModelMissing("some/path.gguf".to_string()).into();
        assert!(error.message().contains("some/path.gguf"));
    }
}
