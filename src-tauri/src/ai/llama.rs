//! The llama.cpp adapter: a supervised local runtime reached over loopback.
//!
//! # Why a subprocess rather than linked bindings
//!
//! Binding llama.cpp into the binary needs CMake, Clang/libclang and Ninja at
//! build time, none of which this project otherwise requires. Running the
//! official prebuilt server as a child process avoids that entirely, and gives
//! two properties worth having on their own:
//!
//! - **Failure isolation.** A model that exhausts memory or crashes takes down
//!   a child process, not the node. Incident capture and replication continue.
//! - **A clean boundary.** The runtime shares no memory with SecureMesh and is
//!   handed nothing but text — the same shape a Phase 5 trusted backend would
//!   need. It is *not* confidential computing today, and is not described as
//!   such.
//!
//! The cost is honest: an extra process, a loopback hop, and a startup delay on
//! first use.
//!
//! # Nothing is downloaded
//!
//! The engine locates a runtime and a model on disk. If either is missing it
//! reports [`Unavailable`] and the node carries on without intelligence. It
//! never fetches anything.

use crate::ai::engine::{
    EngineHealth, GenerationRequest, LocalInferenceEngine, ModelInfo, StructuredRequest,
};
use crate::ai::loopback_http;
use crate::ai::Unavailable;
use crate::error::{CoreError, CoreResult};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// How long to wait for the runtime to become ready after starting it.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(120);
/// How long a single inference may take before it is abandoned.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(180);
/// Interval between readiness probes while starting.
const PROBE_INTERVAL: Duration = Duration::from_millis(250);

/// Where the runtime and model live, and how to run them.
#[derive(Debug, Clone)]
pub struct LlamaConfig {
    /// `llama-server` executable.
    pub server_binary: PathBuf,
    /// GGUF model file.
    pub model_path: PathBuf,
    /// Loopback port. Chosen per node so two instances on one machine do not
    /// collide.
    pub port: u16,
    pub context_tokens: u32,
    pub threads: u32,
    /// Serve embeddings rather than chat completions.
    pub embeddings: bool,
    /// Recorded with every analysis so intelligence is attributable.
    pub model_id: String,
    pub display_name: String,
    pub quantisation: String,
}

impl LlamaConfig {
    /// Default layout for the generation model under a project root.
    pub fn generation(root: &Path, port: u16) -> Self {
        Self {
            server_binary: root.join("ai/runtime/llama-cpu/llama-server.exe"),
            model_path: root.join("ai/models/llm/qwen2.5-1.5b-instruct-q4_k_m.gguf"),
            port,
            context_tokens: 2048,
            threads: 8,
            embeddings: false,
            model_id: "qwen2.5-1.5b-instruct-q4_k_m".to_string(),
            display_name: "Qwen2.5 1.5B Instruct".to_string(),
            quantisation: "Q4_K_M".to_string(),
        }
    }

    /// Default layout for the embedding model under a project root.
    pub fn embedding(root: &Path, port: u16) -> Self {
        Self {
            server_binary: root.join("ai/runtime/llama-cpu/llama-server.exe"),
            model_path: root.join("ai/models/embeddings/bge-small-en-v1.5-q8_0.gguf"),
            port,
            context_tokens: 512,
            threads: 4,
            embeddings: true,
            model_id: "bge-small-en-v1.5-q8_0".to_string(),
            display_name: "BGE Small EN v1.5".to_string(),
            quantisation: "Q8_0".to_string(),
        }
    }

    /// Whether the files this configuration names actually exist.
    ///
    /// Checked before anything is started, so a missing model is reported as a
    /// provisioning problem rather than a process failure.
    pub fn availability(&self) -> Result<(), Unavailable> {
        if !self.server_binary.exists() {
            return Err(Unavailable::RuntimeMissing(
                self.server_binary.display().to_string(),
            ));
        }
        if !self.model_path.exists() {
            return Err(Unavailable::ModelMissing(
                self.model_path.display().to_string(),
            ));
        }
        Ok(())
    }

    fn model_info(&self, backend: &str) -> ModelInfo {
        ModelInfo {
            model_id: self.model_id.clone(),
            display_name: self.display_name.clone(),
            quantisation: self.quantisation.clone(),
            context_tokens: self.context_tokens,
            backend: backend.to_string(),
        }
    }
}

/// The supervised child, if one is running.
struct Runtime {
    child: Child,
}

impl Drop for Runtime {
    fn drop(&mut self) {
        // A model server outliving its node would hold a GPU or a gigabyte of
        // RAM for nothing, and would keep its port bound.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A local inference engine backed by `llama-server`.
pub struct LlamaServerEngine {
    config: LlamaConfig,
    /// `None` until the runtime is started. Started lazily: a node that never
    /// asks for intelligence never pays for loading a model.
    runtime: Mutex<Option<Runtime>>,
}

impl LlamaServerEngine {
    pub fn new(config: LlamaConfig) -> Self {
        Self {
            config,
            runtime: Mutex::new(None),
        }
    }

    pub fn config(&self) -> &LlamaConfig {
        &self.config
    }

    fn lock_runtime(&self) -> MutexGuard<'_, Option<Runtime>> {
        self.runtime
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Starts the runtime if it is not already serving.
    ///
    /// Idempotent, and safe to call from several threads: the mutex is held for
    /// the whole start-and-wait, so two concurrent requests cannot race to spawn
    /// two servers on one port.
    fn ensure_running(&self) -> CoreResult<()> {
        self.config.availability().map_err(CoreError::from)?;

        let mut guard = self.lock_runtime();

        // Already up and answering.
        if let Some(runtime) = guard.as_mut() {
            match runtime.child.try_wait() {
                Ok(None) => return Ok(()), // Still alive.
                // Exited; fall through and restart it.
                Ok(Some(_)) | Err(_) => {
                    *guard = None;
                }
            }
        }

        let mut command = Command::new(&self.config.server_binary);
        command
            .arg("-m")
            .arg(&self.config.model_path)
            .arg("--host")
            .arg("127.0.0.1")
            .arg("--port")
            .arg(self.config.port.to_string())
            .arg("-c")
            .arg(self.config.context_tokens.to_string())
            .arg("-t")
            .arg(self.config.threads.to_string())
            .arg("--no-warmup")
            // The runtime's own logs are not SecureMesh's; discard them rather
            // than interleaving them with audit output.
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .stdin(Stdio::null());

        if self.config.embeddings {
            command.arg("--embeddings");
        }

        #[cfg(windows)]
        {
            // Do not flash a console window when the desktop app starts a model.
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }

        let child = command.spawn().map_err(|e| {
            CoreError::from(Unavailable::RuntimeFailed(format!(
                "could not start the runtime ({})",
                e.kind()
            )))
        })?;

        *guard = Some(Runtime { child });

        // Hold the lock while waiting: a second caller must not conclude the
        // server is absent simply because it is still loading.
        self.await_ready(&mut guard)
    }

    /// Polls until the server answers, or gives up.
    fn await_ready(&self, guard: &mut MutexGuard<'_, Option<Runtime>>) -> CoreResult<()> {
        let deadline = Instant::now() + STARTUP_TIMEOUT;

        while Instant::now() < deadline {
            if let Some(runtime) = guard.as_mut() {
                if let Ok(Some(status)) = runtime.child.try_wait() {
                    **guard = None;
                    return Err(CoreError::from(Unavailable::RuntimeFailed(format!(
                        "the runtime exited during startup ({status})"
                    ))));
                }
            }

            if self.probe_health().is_ok() {
                return Ok(());
            }
            std::thread::sleep(PROBE_INTERVAL);
        }

        **guard = None;
        Err(CoreError::from(Unavailable::RuntimeFailed(
            "the runtime did not become ready in time".to_string(),
        )))
    }

    /// A cheap request that succeeds only once the model is loaded.
    ///
    /// `/health` is a GET endpoint and answers 404 to a POST, which would make
    /// this probe fail permanently and be indistinguishable from a runtime that
    /// never starts. It returns 503 while loading and 200 once ready, which is
    /// exactly the signal wanted here.
    fn probe_health(&self) -> CoreResult<()> {
        loopback_http::get(self.config.port, "/health", Duration::from_millis(500)).map(|_| ())
    }

    /// Sends a chat completion and returns the assistant's message.
    fn chat(&self, body: serde_json::Value) -> CoreResult<String> {
        self.ensure_running()?;

        let response = loopback_http::post_json(
            self.config.port,
            "/v1/chat/completions",
            &body.to_string(),
            REQUEST_TIMEOUT,
        )?;

        let parsed: serde_json::Value = serde_json::from_str(&response)
            .map_err(|_| CoreError::internal("the local runtime sent an unparsable reply"))?;

        parsed["choices"][0]["message"]["content"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| CoreError::internal("the local runtime sent a reply with no content"))
    }

    /// Embeds one text, returning the raw vector.
    pub(crate) fn embed_one(&self, text: &str) -> CoreResult<Vec<f32>> {
        self.ensure_running()?;

        let body = json!({ "input": text });
        let response = loopback_http::post_json(
            self.config.port,
            "/v1/embeddings",
            &body.to_string(),
            REQUEST_TIMEOUT,
        )?;

        let parsed: serde_json::Value = serde_json::from_str(&response)
            .map_err(|_| CoreError::internal("the local runtime sent an unparsable reply"))?;

        let values = parsed["data"][0]["embedding"]
            .as_array()
            .ok_or_else(|| CoreError::internal("the local runtime returned no embedding"))?;

        let vector: Vec<f32> = values
            .iter()
            .filter_map(|v| v.as_f64().map(|f| f as f32))
            .collect();

        if vector.len() != values.len() {
            return Err(CoreError::internal(
                "the local runtime returned a malformed embedding",
            ));
        }
        if vector.is_empty() {
            return Err(CoreError::internal(
                "the local runtime returned an empty embedding",
            ));
        }

        Ok(vector)
    }
}

impl LocalInferenceEngine for LlamaServerEngine {
    fn health(&self) -> EngineHealth {
        if let Err(reason) = self.config.availability() {
            return EngineHealth::Unavailable(reason);
        }

        // Report readiness without starting anything: a dashboard poll should
        // not load a gigabyte of weights as a side effect.
        let running = {
            let mut guard = self.lock_runtime();
            match guard.as_mut() {
                Some(runtime) => matches!(runtime.child.try_wait(), Ok(None)),
                None => false,
            }
        };

        if running && self.probe_health().is_ok() {
            EngineHealth::Ready(self.config.model_info("local-cpu"))
        } else if running {
            EngineHealth::Loading
        } else {
            // Provisioned but not yet started. Ready in the sense that matters
            // to an operator: it will answer when asked.
            EngineHealth::Ready(self.config.model_info("local-cpu"))
        }
    }

    fn generate(&self, request: &GenerationRequest) -> CoreResult<String> {
        request.validate()?;

        self.chat(json!({
            "messages": [
                { "role": "system", "content": request.system },
                { "role": "user", "content": request.user }
            ],
            "temperature": request.temperature,
            "max_tokens": request.max_tokens,
            "stream": false
        }))
    }

    fn generate_structured(&self, request: &StructuredRequest) -> CoreResult<String> {
        request.validate()?;

        // `json_schema` makes the runtime constrain decoding, so the output is
        // guaranteed to *parse*. It guarantees nothing about whether the
        // content is true, which is why it is still validated afterwards.
        self.chat(json!({
            "messages": [
                { "role": "system", "content": request.system },
                { "role": "user", "content": request.user }
            ],
            "temperature": 0.0,
            "max_tokens": request.max_tokens,
            "stream": false,
            "json_schema": request.schema
        }))
    }

    fn unload(&self) {
        // Dropping the runtime kills the child and frees the model.
        *self.lock_runtime() = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn config_in(dir: &TempDir) -> LlamaConfig {
        LlamaConfig::generation(dir.path(), 18_000)
    }

    #[test]
    fn a_missing_runtime_is_reported_as_a_provisioning_problem() {
        let dir = TempDir::new().unwrap();
        let engine = LlamaServerEngine::new(config_in(&dir));

        match engine.health() {
            EngineHealth::Unavailable(Unavailable::RuntimeMissing(path)) => {
                assert!(path.contains("llama-server"));
            }
            other => panic!("expected RuntimeMissing, got {other:?}"),
        }
    }

    #[test]
    fn a_missing_model_is_distinguished_from_a_missing_runtime() {
        let dir = TempDir::new().unwrap();
        let config = config_in(&dir);

        // Provide the runtime but not the model.
        std::fs::create_dir_all(config.server_binary.parent().unwrap()).unwrap();
        std::fs::write(&config.server_binary, b"stub").unwrap();

        let engine = LlamaServerEngine::new(config);
        match engine.health() {
            EngineHealth::Unavailable(Unavailable::ModelMissing(path)) => {
                assert!(path.contains(".gguf"));
            }
            other => panic!("expected ModelMissing, got {other:?}"),
        }
    }

    #[test]
    fn generation_fails_cleanly_when_nothing_is_provisioned() {
        let dir = TempDir::new().unwrap();
        let engine = LlamaServerEngine::new(config_in(&dir));

        let result = engine.generate(&GenerationRequest {
            system: "s".to_string(),
            user: "u".to_string(),
            max_tokens: 64,
            temperature: 0.0,
        });

        // An error, never a panic and never a hang.
        assert!(result.is_err());
        assert!(result.unwrap_err().message().contains("PROVISIONING"));
    }

    #[test]
    fn an_oversized_request_is_refused_before_the_runtime_is_touched() {
        let dir = TempDir::new().unwrap();
        let engine = LlamaServerEngine::new(config_in(&dir));

        let result = engine.generate(&GenerationRequest {
            system: "s".repeat(crate::ai::engine::MAX_PROMPT_CHARS + 1),
            user: String::new(),
            max_tokens: 64,
            temperature: 0.0,
        });

        // Validation, not a provisioning error: the request never got that far.
        assert_eq!(result.unwrap_err().code(), "VALIDATION_ERROR");
    }

    #[test]
    fn unloading_an_engine_that_never_started_is_harmless() {
        let dir = TempDir::new().unwrap();
        let engine = LlamaServerEngine::new(config_in(&dir));
        engine.unload();
        engine.unload();
    }

    #[test]
    fn the_default_layout_points_at_the_provisioned_paths() {
        let root = Path::new("/project");

        let generation = LlamaConfig::generation(root, 1);
        assert!(generation.model_path.to_string_lossy().contains("llm"));
        assert!(!generation.embeddings);

        let embedding = LlamaConfig::embedding(root, 2);
        assert!(embedding.model_path.to_string_lossy().contains("embeddings"));
        assert!(embedding.embeddings);
        assert_ne!(generation.model_id, embedding.model_id);
    }

    #[test]
    fn the_backend_is_reported_as_local_never_as_a_service() {
        let info = LlamaConfig::generation(Path::new("/p"), 1).model_info("local-cpu");
        assert_eq!(info.backend, "local-cpu");
        // Nothing here may imply a remote dependency.
        assert!(!info.backend.contains("http"));
    }
}
