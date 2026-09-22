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

/// SHA-256 of the generation model this build is pinned to.
///
/// # Why a filename is not an identity
///
/// `availability()` checks that a file exists at the configured path, which
/// says nothing about *which* model it is. Anything placed at that filename
/// loads, and every analysis it produces is then stamped with the `model_id`
/// belonging to the model that was supposed to be there — silently attributing
/// one model's judgement to another. `model_id` exists to make intelligence
/// attributable, and a filename alone cannot carry that.
///
/// Pinning the digest makes the artifact itself the identity. A model swapped
/// underneath a node stops it rather than mislabelling its output.
pub const GENERATION_MODEL_SHA256: &str =
    "6a1a2eb6d15622bf3c96857206351ba97e1af16c30d7a74ee38970e434e9407e";

/// SHA-256 of the embedding model this build is pinned to.
///
/// Pinned for the same reason, and deliberately not left optional: an
/// unpinned path is a bypass that nobody would notice until it mattered.
pub const EMBEDDING_MODEL_SHA256: &str =
    "ec38e8da142596baa913124ae50550de284b6916bf59577ef2f0cb9660c2f514";

/// The inference runtime's executable name on this platform.
///
/// The provisioned llama.cpp release ships `llama-server.exe` on Windows and
/// `llama-server` everywhere else. Hardcoding the Windows name made a Linux or
/// ARM node fail `availability()` with `RuntimeMissing` and silently come up
/// with no intelligence at all, which reads as a provisioning mistake rather
/// than a build one.
#[cfg(windows)]
const SERVER_BINARY: &str = "llama-server.exe";
#[cfg(not(windows))]
const SERVER_BINARY: &str = "llama-server";

/// Threads for the generation runtime.
///
/// Derived rather than fixed. The previous constant 8 oversubscribes a 4-core
/// single-board computer, where the extra threads contend for the same cores
/// and memory bandwidth that inference is already bound by. Capped at 8 so a
/// large desktop behaves exactly as it did before this became adaptive.
fn default_generation_threads() -> u32 {
    std::thread::available_parallelism()
        .map(|cores| cores.get().min(8) as u32)
        .unwrap_or(4)
}

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
    /// The digest `model_path` must have.
    ///
    /// Verified where the runtime is actually started, not in
    /// [`Self::availability`] — that is called on every dashboard poll and on
    /// every generation request, and hashing a gigabyte there would be the
    /// very side effect `health()` is written to avoid.
    pub expected_sha256: String,
}

impl LlamaConfig {
    /// Default layout for the generation model under a project root.
    pub fn generation(root: &Path, port: u16) -> Self {
        Self {
            server_binary: root.join("ai/runtime/llama-cpu").join(SERVER_BINARY),
            model_path: root.join("ai/models/llm/qwen2.5-1.5b-instruct-q4_k_m.gguf"),
            port,
            context_tokens: 2048,
            threads: default_generation_threads(),
            embeddings: false,
            model_id: "qwen2.5-1.5b-instruct-q4_k_m".to_string(),
            display_name: "Qwen2.5 1.5B Instruct".to_string(),
            quantisation: "Q4_K_M".to_string(),
            expected_sha256: GENERATION_MODEL_SHA256.to_string(),
        }
    }

    /// Default layout for the embedding model under a project root.
    pub fn embedding(root: &Path, port: u16) -> Self {
        Self {
            server_binary: root.join("ai/runtime/llama-cpu").join(SERVER_BINARY),
            model_path: root.join("ai/models/embeddings/bge-small-en-v1.5-q8_0.gguf"),
            port,
            context_tokens: 512,
            threads: 4,
            embeddings: true,
            model_id: "bge-small-en-v1.5-q8_0".to_string(),
            display_name: "BGE Small EN v1.5".to_string(),
            quantisation: "Q8_0".to_string(),
            expected_sha256: EMBEDDING_MODEL_SHA256.to_string(),
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

    /// Confirms the model file is the artifact this build pins.
    ///
    /// # Why this is not part of `availability()`
    ///
    /// `availability()` is called from `health()` on every dashboard poll and
    /// from `ensure_running()` on every generation request. Hashing a gigabyte
    /// there would reintroduce exactly the cost `health()` is written to avoid.
    /// This runs where the model is actually loaded — once, immediately before
    /// the runtime is spawned.
    ///
    /// # What it does and does not prove
    ///
    /// It proves the bytes on disk are the bytes this build expects, so an
    /// analysis stamped `model_id` really came from that model. It is an
    /// integrity check, not an authenticity one: anyone able to replace the
    /// file could also edit the pinned constant in a rebuilt binary. It defends
    /// against a model swapped underneath a working node, not against an
    /// attacker who already controls the build.
    pub fn verify_integrity(&self) -> Result<(), Unavailable> {
        let actual = sha256_file(&self.model_path).map_err(|error| Unavailable::ModelCorrupt {
            path: self.model_path.display().to_string(),
            expected: self.expected_sha256.clone(),
            actual: format!("unreadable: {error}"),
        })?;

        if actual != self.expected_sha256 {
            return Err(Unavailable::ModelCorrupt {
                path: self.model_path.display().to_string(),
                expected: self.expected_sha256.clone(),
                actual,
            });
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

/// Streams a file through SHA-256.
///
/// Deliberately chunked rather than `Sha256::digest(fs::read(..))`, which is
/// the idiom used elsewhere in this crate for small inputs: a model file is
/// over a gigabyte and reading it whole would spend that much memory to answer
/// a yes/no question.
fn sha256_file(path: &Path) -> std::io::Result<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;

    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1 << 20];

    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }

    Ok(hex::encode(hasher.finalize()))
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

        // The load point, and the only place this runs. Reached once per spawn
        // — the early return above means a node already serving does not rehash
        // a gigabyte, and `availability()` deliberately does not do this at all
        // because it is called on every poll and every request.
        //
        // A failure here stops the node using the model. There is no fallback
        // to another artifact and no degraded mode that loads it anyway: a file
        // that is not the pinned model would produce analyses stamped with a
        // `model_id` that did not make them.
        self.config.verify_integrity().map_err(CoreError::from)?;

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

    /// Digests computed outside this crate, so a test cannot agree with a
    /// broken hasher: `sha256(b"stub")` and `sha256(b"not-the-model")`.
    const STUB_SHA256: &str = "725c546b990dd1b41f3d5791b37c3c0edcb1f08cf150bdae32a73dfd166e02d7";
    const OTHER_SHA256: &str = "417aa8c8e3e49c3c86bf04bd314bf4132febbe3dd2066901a0fcad6b9d184c04";

    /// Places a runtime and a model file so only the digest is in question.
    fn provision(config: &LlamaConfig, model_bytes: &[u8]) {
        std::fs::create_dir_all(config.server_binary.parent().unwrap()).unwrap();
        std::fs::write(&config.server_binary, b"stub").unwrap();
        std::fs::create_dir_all(config.model_path.parent().unwrap()).unwrap();
        std::fs::write(&config.model_path, model_bytes).unwrap();
    }

    // --- Model integrity ----------------------------------------------------

    #[test]
    fn the_pinned_artifact_verifies() {
        let dir = TempDir::new().unwrap();
        let mut config = config_in(&dir);
        config.expected_sha256 = STUB_SHA256.to_string();
        provision(&config, b"stub");

        assert!(config.verify_integrity().is_ok());
    }

    #[test]
    fn a_model_that_is_not_the_pinned_artifact_is_refused() {
        let dir = TempDir::new().unwrap();
        let mut config = config_in(&dir);
        config.expected_sha256 = OTHER_SHA256.to_string();
        // The file exists and is readable. Only its identity is wrong — the
        // case a filename check cannot see.
        provision(&config, b"stub");

        match config.verify_integrity() {
            Err(Unavailable::ModelCorrupt {
                expected, actual, ..
            }) => {
                assert_eq!(expected, OTHER_SHA256);
                assert_eq!(actual, STUB_SHA256);
            }
            other => panic!("expected ModelCorrupt, got {other:?}"),
        }
    }

    #[test]
    fn an_integrity_failure_says_so_and_says_nothing_was_loaded() {
        let dir = TempDir::new().unwrap();
        let mut config = config_in(&dir);
        config.expected_sha256 = OTHER_SHA256.to_string();
        provision(&config, b"stub");

        let detail = config.verify_integrity().unwrap_err().detail();
        assert!(detail.contains("integrity"), "{detail}");
        assert!(detail.contains("NOT loaded"), "{detail}");
        assert!(detail.contains(OTHER_SHA256), "{detail}");
    }

    #[test]
    fn a_missing_model_stays_missing_rather_than_becoming_corrupt() {
        // The two failures call for different responses — one is provisioning
        // that never happened, the other is a file that changed underneath a
        // working node — so they must not collapse into one message.
        let dir = TempDir::new().unwrap();
        let config = config_in(&dir);
        std::fs::create_dir_all(config.server_binary.parent().unwrap()).unwrap();
        std::fs::write(&config.server_binary, b"stub").unwrap();

        match config.availability() {
            Err(Unavailable::ModelMissing(path)) => assert!(path.contains(".gguf")),
            other => panic!("expected ModelMissing, got {other:?}"),
        }
    }

    #[test]
    fn a_swapped_model_stops_the_runtime_rather_than_loading_it() {
        // End to end through the engine: the guarantee is that a wrong digest
        // prevents the load, not merely that a helper returns an error.
        let dir = TempDir::new().unwrap();
        let mut config = config_in(&dir);
        config.expected_sha256 = OTHER_SHA256.to_string();
        provision(&config, b"stub");

        let engine = LlamaServerEngine::new(config);
        let error = engine
            .generate(&GenerationRequest {
                system: "s".to_string(),
                user: "u".to_string(),
                max_tokens: 64,
                temperature: 0.0,
            })
            .unwrap_err();

        assert!(error.message().contains("integrity"), "{}", error.message());
    }

    #[test]
    fn availability_does_not_hash_the_model() {
        // `availability()` runs on every dashboard poll and every generation
        // request. If it ever starts hashing, a poll costs a gigabyte of I/O —
        // so a wrong digest must pass *this* check and be caught at the load
        // point instead.
        let dir = TempDir::new().unwrap();
        let mut config = config_in(&dir);
        config.expected_sha256 = OTHER_SHA256.to_string();
        provision(&config, b"stub");

        assert!(config.availability().is_ok());
        assert!(config.verify_integrity().is_err());
    }

    #[test]
    fn each_model_is_pinned_to_its_own_digest_and_keeps_its_own_id() {
        let root = Path::new("/p");
        let generation = LlamaConfig::generation(root, 1);
        let embedding = LlamaConfig::embedding(root, 2);

        // Neither path is left unpinned, and they are not the same artifact.
        assert_eq!(generation.expected_sha256, GENERATION_MODEL_SHA256);
        assert_eq!(embedding.expected_sha256, EMBEDDING_MODEL_SHA256);
        assert_ne!(generation.expected_sha256, embedding.expected_sha256);

        // The digest and the recorded identity describe the same artifact, so
        // an analysis stamped with this `model_id` really came from it.
        assert_eq!(generation.model_id, "qwen2.5-1.5b-instruct-q4_k_m");
        assert_eq!(generation.expected_sha256.len(), 64);
    }

    #[test]
    fn the_fine_tuned_candidate_is_not_what_production_is_pinned_to() {
        // Phase 8 does not switch production. Dropping the candidate GGUF at
        // the production filename would previously have loaded silently; now
        // it cannot, because the pinned digest is not the candidate's.
        const CANDIDATE_SHA256: &str =
            "bf03ee464040cafc5883c48afee2785a56955fc737c8033a68fefb255f36fc16";
        assert_ne!(GENERATION_MODEL_SHA256, CANDIDATE_SHA256);
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
        assert!(embedding
            .model_path
            .to_string_lossy()
            .contains("embeddings"));
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
