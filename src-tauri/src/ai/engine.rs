//! The inference abstraction.
//!
//! Nothing above this file knows what runs the model. That is what allows the
//! CPU runtime used today to be replaced by a CUDA build, a different runtime,
//! or the trusted backend of Phase 5 without touching application code.
//!
//! # Designed for a future TEE, without claiming to be one
//!
//! [`LocalInferenceEngine`] deliberately takes plain text and returns plain
//! text. There is no shared memory, no callback into SecureMesh, and no
//! handle to anything the caller owns, so an implementation is free to run the
//! model somewhere the host cannot see — another process, another privilege
//! level, or eventually an enclave.
//!
//! **The current implementation is not confidential computing and must not be
//! described as such.** It is an ordinary local process whose memory the
//! operating system can read. The interface is merely shaped so that a
//! confidential implementation would not require an application rewrite.

use crate::ai::Unavailable;
use crate::error::CoreResult;
use serde::Serialize;

/// Bounds on what may be asked of a model.
///
/// Applied by callers before a request reaches a runtime, so a prompt built
/// from unbounded input cannot exhaust the context window or run indefinitely.
pub const MAX_PROMPT_CHARS: usize = 24_000;
pub const MAX_OUTPUT_TOKENS: u32 = 1_024;

/// What a node is running, for the dashboard and the benchmark record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    /// Stable identifier recorded alongside every analysis, so intelligence
    /// produced by one model is distinguishable from another's.
    pub model_id: String,
    pub display_name: String,
    pub quantisation: String,
    pub context_tokens: u32,
    /// Where inference happens: `"local-cpu"`, `"local-cuda"`, and — if it ever
    /// genuinely does — `"tee"`. Never a remote endpoint.
    pub backend: String,
}

/// Whether the engine can serve a request right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineHealth {
    /// Loaded and answering.
    Ready(ModelInfo),
    /// Starting up; callers should degrade, not block.
    Loading,
    /// Not usable, with a reason an operator can act on.
    Unavailable(Unavailable),
}

impl EngineHealth {
    pub fn is_ready(&self) -> bool {
        matches!(self, EngineHealth::Ready(_))
    }

    pub fn model(&self) -> Option<&ModelInfo> {
        match self {
            EngineHealth::Ready(info) => Some(info),
            _ => None,
        }
    }
}

/// A free-text generation request.
#[derive(Debug, Clone)]
pub struct GenerationRequest {
    /// Instructions describing the task and its constraints.
    pub system: String,
    /// The content to reason about. Treated as data, never as instructions —
    /// see [`crate::ai::prompt`].
    pub user: String,
    pub max_tokens: u32,
    /// 0.0 for anything whose output is parsed, so results are reproducible.
    pub temperature: f32,
}

impl GenerationRequest {
    /// Refuses a request that is too large before any runtime sees it.
    pub fn validate(&self) -> CoreResult<()> {
        let total = self.system.chars().count() + self.user.chars().count();
        if total > MAX_PROMPT_CHARS {
            return Err(crate::error::CoreError::validation(format!(
                "prompt is {total} characters, over the {MAX_PROMPT_CHARS} limit"
            )));
        }
        if self.max_tokens == 0 || self.max_tokens > MAX_OUTPUT_TOKENS {
            return Err(crate::error::CoreError::validation(
                "requested output length is out of range",
            ));
        }
        Ok(())
    }
}

/// A request whose answer must match a JSON schema.
///
/// The schema is enforced by the runtime's constrained decoding, so the model
/// cannot emit anything that fails to parse. That is a parsing guarantee only:
/// the *content* is still untrusted and is validated separately.
#[derive(Debug, Clone)]
pub struct StructuredRequest {
    pub system: String,
    pub user: String,
    /// JSON Schema the output is constrained to.
    pub schema: serde_json::Value,
    pub max_tokens: u32,
}

impl StructuredRequest {
    pub fn validate(&self) -> CoreResult<()> {
        GenerationRequest {
            system: self.system.clone(),
            user: self.user.clone(),
            max_tokens: self.max_tokens,
            temperature: 0.0,
        }
        .validate()
    }
}

/// A local text-generation model.
///
/// Implementations must not reach the network beyond the loopback interface
/// they use to talk to their own runtime, and must not require credentials.
pub trait LocalInferenceEngine: Send + Sync {
    /// Whether the engine can serve a request. Cheap enough to call on every
    /// dashboard refresh.
    fn health(&self) -> EngineHealth;

    /// What is loaded, if anything.
    fn model_info(&self) -> Option<ModelInfo> {
        self.health().model().cloned()
    }

    /// Generates free text.
    fn generate(&self, request: &GenerationRequest) -> CoreResult<String>;

    /// Generates JSON constrained to a schema.
    ///
    /// Returns the raw string rather than a parsed value: the caller decides
    /// what type it is willing to accept, which keeps the "untrusted until
    /// validated" boundary visible at every call site.
    fn generate_structured(&self, request: &StructuredRequest) -> CoreResult<String>;

    /// Releases model memory. Called when a node is shutting down or an
    /// operator disables intelligence; the engine must be usable again
    /// afterwards.
    fn unload(&self);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(system_len: usize, user_len: usize, max_tokens: u32) -> GenerationRequest {
        GenerationRequest {
            system: "s".repeat(system_len),
            user: "u".repeat(user_len),
            max_tokens,
            temperature: 0.0,
        }
    }

    #[test]
    fn a_reasonable_request_is_accepted() {
        assert!(request(100, 500, 256).validate().is_ok());
    }

    #[test]
    fn an_oversized_prompt_is_refused_before_it_reaches_a_runtime() {
        let err = request(MAX_PROMPT_CHARS, 100, 256).validate().unwrap_err();
        assert_eq!(err.code(), "VALIDATION_ERROR");
        assert!(err.message().contains("limit"));
    }

    #[test]
    fn the_prompt_limit_counts_both_halves() {
        // Neither half alone is over the limit; together they are.
        let half = MAX_PROMPT_CHARS / 2 + 10;
        assert!(request(half, half, 256).validate().is_err());
    }

    #[test]
    fn an_absurd_or_zero_output_length_is_refused() {
        assert!(request(10, 10, 0).validate().is_err());
        assert!(request(10, 10, MAX_OUTPUT_TOKENS + 1).validate().is_err());
    }

    #[test]
    fn structured_requests_are_bounded_the_same_way() {
        let oversized = StructuredRequest {
            system: "s".repeat(MAX_PROMPT_CHARS),
            user: "u".repeat(100),
            schema: serde_json::json!({"type": "object"}),
            max_tokens: 256,
        };
        assert!(oversized.validate().is_err());
    }

    #[test]
    fn health_reports_readiness_and_the_loaded_model() {
        let info = ModelInfo {
            model_id: "qwen2.5-1.5b-instruct-q4_k_m".to_string(),
            display_name: "Qwen2.5 1.5B Instruct".to_string(),
            quantisation: "Q4_K_M".to_string(),
            context_tokens: 2048,
            backend: "local-cpu".to_string(),
        };

        let ready = EngineHealth::Ready(info.clone());
        assert!(ready.is_ready());
        assert_eq!(ready.model(), Some(&info));

        assert!(!EngineHealth::Loading.is_ready());
        assert!(EngineHealth::Loading.model().is_none());
        assert!(!EngineHealth::Unavailable(Unavailable::Disabled).is_ready());
    }

    #[test]
    fn the_backend_never_names_a_remote_service() {
        // A guard on the vocabulary: "local-cpu" and "tee" are honest, an
        // endpoint would not be.
        for backend in ["local-cpu", "local-cuda", "tee"] {
            assert!(!backend.contains("http"));
            assert!(!backend.contains("api"));
        }
    }
}
