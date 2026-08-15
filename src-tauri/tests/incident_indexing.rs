//! Every persisted incident eventually becomes locally searchable.
//!
//! # The regression this exists for
//!
//! `index_pending` was fully functional and nothing ever called it. An incident
//! was stored, signed and replicated correctly, and remained permanently
//! invisible to retrieval — so every question was refused with `generation 0 ms`
//! and the model was never consulted. Diagnosis confirmed the model, the
//! embedder, the threshold and the refusal logic were all behaving correctly;
//! the gap was a missing *caller*.
//!
//! The invariant asserted here belongs in the core rather than the UI, because
//! incidents arrive from more than one place — an operator, a peer over QUIC,
//! a test, and in future a CLI — and every one of them must end up searchable.
//!
//! # Why the engines are stubs
//!
//! Indexing behaviour is about *when* embedding is requested and what happens
//! when it fails, not about embedding quality. A stub makes failure injectable
//! and the tests fast and deterministic; `mesh_libp2p.rs` and the evaluation
//! harness cover the real model.

use securemesh_lib::ai::embedding::{Embedding, EmbeddingEngine};
use securemesh_lib::ai::engine::{
    EngineHealth, GenerationRequest, LocalInferenceEngine, ModelInfo, StructuredRequest,
};
use securemesh_lib::ai::{IndexState, IntelligenceService, Unavailable};
use securemesh_lib::domain::{NewIncident, SyncStatus};
use securemesh_lib::networking::loopback::LoopbackNetwork;
use securemesh_lib::NodeRuntime;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tempfile::TempDir;

/// How long a test waits for the background indexer to settle.
const BUDGET: Duration = Duration::from_secs(10);

/// An embedder whose availability a test can switch at will.
///
/// This is what makes "the model went away" a thing that can be asserted about
/// rather than described.
struct SwitchableEmbedder {
    available: AtomicBool,
}

impl SwitchableEmbedder {
    fn new(available: bool) -> Self {
        Self {
            available: AtomicBool::new(available),
        }
    }

    fn set(&self, available: bool) {
        self.available.store(available, Ordering::SeqCst);
    }
}

impl EmbeddingEngine for SwitchableEmbedder {
    fn health(&self) -> EngineHealth {
        if self.available.load(Ordering::SeqCst) {
            EngineHealth::Ready(info("stub-embed"))
        } else {
            EngineHealth::Unavailable(Unavailable::Disabled)
        }
    }

    fn embed(&self, text: &str) -> securemesh_lib::CoreResult<Embedding> {
        if !self.available.load(Ordering::SeqCst) {
            return Err(securemesh_lib::CoreError::internal("embedder offline"));
        }
        // Deterministic and content-dependent: identical text embeds
        // identically, different text does not.
        let mut vector = vec![0.0f32; 8];
        for (index, byte) in text.bytes().enumerate() {
            vector[index % 8] += byte as f32 / 255.0;
        }
        Embedding::new(vector, "stub-embed")
    }

    fn model_id(&self) -> String {
        "stub-embed".to_string()
    }
}

fn info(id: &str) -> ModelInfo {
    ModelInfo {
        model_id: id.to_string(),
        display_name: id.to_string(),
        quantisation: "none".to_string(),
        context_tokens: 512,
        backend: "local-cpu".to_string(),
    }
}

/// A generator that answers from whatever context it is given, and counts how
/// often it was actually asked.
///
/// The count exists because the symptom being guarded against was precisely
/// "the model is never invoked". Latency cannot stand in for that here: a stub
/// returns in microseconds, so `generation_ms` rounds to 0 and would look
/// identical to never having been called. The counter asserts the thing itself.
#[derive(Default)]
struct StubGenerator {
    invocations: std::sync::atomic::AtomicUsize,
}

impl StubGenerator {
    fn invocations(&self) -> usize {
        self.invocations.load(Ordering::SeqCst)
    }
}

impl LocalInferenceEngine for StubGenerator {
    fn health(&self) -> EngineHealth {
        EngineHealth::Ready(info("stub-model"))
    }

    fn generate(&self, _request: &GenerationRequest) -> securemesh_lib::CoreResult<String> {
        self.invocations.fetch_add(1, Ordering::SeqCst);
        Ok("stub".to_string())
    }

    fn generate_structured(
        &self,
        _request: &StructuredRequest,
    ) -> securemesh_lib::CoreResult<String> {
        self.invocations.fetch_add(1, Ordering::SeqCst);
        // A grounded answer citing the first supplied passage.
        Ok(
            r#"{"answer":"Answered from local records.","sources":[1],"sufficient":true}"#
                .to_string(),
        )
    }

    fn unload(&self) {}
}

struct Node {
    _dir: TempDir,
    runtime: NodeRuntime,
    node_id: String,
    embedder: Arc<SwitchableEmbedder>,
    generator: Arc<StubGenerator>,
}

/// Attaches intelligence to an already-built runtime.
fn with_intelligence(
    runtime: &mut NodeRuntime,
    available: bool,
) -> (Arc<SwitchableEmbedder>, Arc<StubGenerator>) {
    let embedder = Arc::new(SwitchableEmbedder::new(available));
    let generator = Arc::new(StubGenerator::default());
    runtime.attach_intelligence(IntelligenceService::new(
        runtime.database_handle(),
        Arc::clone(&generator) as Arc<dyn LocalInferenceEngine>,
        Arc::clone(&embedder) as Arc<dyn EmbeddingEngine>,
    ));
    (embedder, generator)
}

/// A standalone node — no mesh, which is a fully supported mode.
fn node(embedder_available: bool) -> Node {
    let dir = TempDir::new().unwrap();
    let mut runtime = NodeRuntime::initialize(dir.path()).unwrap();
    let (embedder, generator) = with_intelligence(&mut runtime, embedder_available);
    let node_id = runtime.public_identity().node_id;

    Node {
        _dir: dir,
        runtime,
        node_id,
        embedder,
        generator,
    }
}

/// A node attached to the deterministic loopback network.
///
/// The identity is created before attaching because the network is keyed by
/// node ID, and the node ID is derived from the key.
fn mesh_node(network: &LoopbackNetwork, embedder_available: bool) -> Node {
    use securemesh_lib::identity::keystore::FileKeyStore;
    use securemesh_lib::identity::NodeIdentity;
    use securemesh_lib::runtime::KEYSTORE_FILE;

    let dir = TempDir::new().unwrap();
    let identity =
        NodeIdentity::load_or_create(&FileKeyStore::new(dir.path().join(KEYSTORE_FILE))).unwrap();
    let node_id = identity.node_id().to_string();
    let transport = network.attach(&node_id, &identity.public_key_hex());

    let mut runtime =
        NodeRuntime::initialize_with_transport(dir.path(), Box::new(transport)).unwrap();
    let (embedder, generator) = with_intelligence(&mut runtime, embedder_available);

    Node {
        _dir: dir,
        runtime,
        node_id,
        embedder,
        generator,
    }
}

/// Connects two loopback nodes and completes mutual enrollment.
fn connect_and_enroll(network: &LoopbackNetwork, a: &Node, b: &Node) {
    network.connect(&a.node_id, &b.node_id);
    wait_for("the handshake to complete", &[a, b], || {
        !a.runtime.connected_peers().is_empty() && !b.runtime.connected_peers().is_empty()
    });
    a.runtime.approve_peer(&b.node_id, None).unwrap();
    b.runtime.approve_peer(&a.node_id, None).unwrap();
}

fn incident(description: &str) -> NewIncident {
    NewIncident {
        description: description.to_string(),
        severity: "HIGH".to_string(),
        latitude: None,
        longitude: None,
    }
}

/// Waits for `condition`, ticking any transports so replication proceeds.
fn wait_for(label: &str, nodes: &[&Node], condition: impl Fn() -> bool) {
    let deadline = Instant::now() + BUDGET;
    while Instant::now() < deadline {
        for node in nodes {
            let _ = node.runtime.sync_tick();
        }
        if condition() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("timed out after {}s waiting for: {label}", BUDGET.as_secs());
}

fn state_of(node: &Node, incident_id: &str) -> Option<IndexState> {
    node.runtime
        .incident_index_states()
        .unwrap()
        .into_iter()
        .find(|entry| entry.incident_id == incident_id)
        .map(|entry| entry.state)
}

fn vectors(node: &Node) -> u64 {
    node.runtime.intelligence_status().vectors_stored
}

// ---------------------------------------------------------------------------
// 1. A local incident becomes searchable on its own
// ---------------------------------------------------------------------------

#[test]
fn a_locally_created_incident_is_indexed_without_being_asked() {
    let node = node(true);
    let created = node
        .runtime
        .create_incident(incident("avalanche at mount abu"))
        .unwrap();

    wait_for("the local incident to be indexed", &[&node], || {
        state_of(&node, &created.id) == Some(IndexState::Indexed)
    });

    assert_eq!(vectors(&node), 1, "exactly one vector for one incident");
}

// ---------------------------------------------------------------------------
// 2. Indexing happens after the commit, and never gates it
// ---------------------------------------------------------------------------

#[test]
fn the_incident_is_durable_the_instant_create_returns() {
    // create_incident must not wait for embedding: the record is authoritative
    // and the vector is derived, so the write returns as soon as it is
    // committed, whatever the indexer is doing.
    let node = node(true);
    let created = node
        .runtime
        .create_incident(incident("stored first"))
        .unwrap();

    // Readable immediately, before any indexing could plausibly have finished.
    let stored = node.runtime.get_incident(&created.id).unwrap();
    assert_eq!(stored.description, "stored first");
    assert_eq!(stored.sync_status, SyncStatus::Pending);
}

// ---------------------------------------------------------------------------
// 3 & 4. AI failure never touches the authoritative record
// ---------------------------------------------------------------------------

#[test]
fn an_incident_survives_an_embedder_that_is_completely_unavailable() {
    let node = node(false);

    // Creation must succeed with no model at all.
    let created = node
        .runtime
        .create_incident(incident("no model here"))
        .unwrap();
    assert_eq!(
        node.runtime.get_incident(&created.id).unwrap().description,
        "no model here"
    );
    assert_eq!(node.runtime.list_incidents(None).unwrap().len(), 1);

    // And the event log — the thing that replicates — is intact.
    assert_eq!(node.runtime.database().count_events().unwrap(), 1);
    assert_eq!(vectors(&node), 0, "nothing could be embedded");
}

#[test]
fn a_failed_index_pass_is_reported_without_losing_the_incident() {
    let node = node(false);
    let created = node
        .runtime
        .create_incident(incident("will fail to embed"))
        .unwrap();

    wait_for("the failure to be reported", &[&node], || {
        matches!(
            state_of(&node, &created.id),
            Some(IndexState::IndexFailed) | Some(IndexState::NotIndexed)
        )
    });

    // Whatever the transient state says, the record itself is untouched.
    assert_eq!(node.runtime.list_incidents(None).unwrap().len(), 1);
    assert_ne!(
        state_of(&node, &created.id),
        Some(IndexState::Indexed),
        "it cannot be indexed when the embedder is offline"
    );
}

// ---------------------------------------------------------------------------
// 5. Recovery: the model comes back and the backlog is repaired
// ---------------------------------------------------------------------------

#[test]
fn an_incident_is_indexed_once_the_embedder_returns() {
    let node = node(false);
    let created = node
        .runtime
        .create_incident(incident("indexed later"))
        .unwrap();
    assert_eq!(vectors(&node), 0);

    // The model comes back. No operator action, no restart.
    node.embedder.set(true);
    node.runtime.index_intelligence().unwrap();

    wait_for("the backlog to be repaired", &[&node], || {
        state_of(&node, &created.id) == Some(IndexState::Indexed)
    });
    assert_eq!(vectors(&node), 1);
}

// ---------------------------------------------------------------------------
// 6 & 15. Idempotency
// ---------------------------------------------------------------------------

#[test]
fn repeated_indexing_does_not_duplicate_vectors() {
    let node = node(true);
    let created = node
        .runtime
        .create_incident(incident("index me twice"))
        .unwrap();

    wait_for("the first index", &[&node], || {
        state_of(&node, &created.id) == Some(IndexState::Indexed)
    });
    let after_first = vectors(&node);

    // Index again, explicitly, several times.
    for _ in 0..3 {
        node.runtime.index_intelligence().unwrap();
    }

    assert_eq!(
        vectors(&node),
        after_first,
        "re-indexing must not create a second vector for the same incident"
    );
    assert_eq!(after_first, 1);
}

// ---------------------------------------------------------------------------
// 7 & 8. Restart reconciles what a previous run left behind
// ---------------------------------------------------------------------------

#[test]
fn a_restart_indexes_incidents_created_while_no_model_was_present() {
    let dir = TempDir::new().unwrap();

    // First run: incidents are created with no intelligence attached at all,
    // which is exactly the state every node was in before this feature existed.
    let created = {
        let runtime = NodeRuntime::initialize(dir.path()).unwrap();
        let first = runtime
            .create_incident(incident("created before indexing existed"))
            .unwrap();
        runtime
            .create_incident(incident("and a second one"))
            .unwrap();
        assert_eq!(runtime.intelligence_status().vectors_stored, 0);
        first
    };

    // Second run: a model is now provisioned. Attaching intelligence must
    // repair the backlog by itself — no manual step, no re-creation.
    let mut runtime = NodeRuntime::initialize(dir.path()).unwrap();
    let (embedder, generator) = with_intelligence(&mut runtime, true);
    let node_id = runtime.public_identity().node_id;
    let restarted = Node {
        _dir: dir,
        runtime,
        node_id,
        embedder,
        generator,
    };

    wait_for(
        "startup reconciliation to index both",
        &[&restarted],
        || restarted.runtime.intelligence_status().vectors_stored == 2,
    );

    assert_eq!(state_of(&restarted, &created.id), Some(IndexState::Indexed));
}

// ---------------------------------------------------------------------------
// 2 & 10. A replicated incident is indexed by the receiving node itself
// ---------------------------------------------------------------------------

#[test]
fn an_incident_arriving_from_a_peer_is_indexed_locally() {
    // The property that makes this more than a UI fix: a vector is derived
    // *local* state. Node B never receives A's embedding — it builds its own,
    // with its own model, from the replicated record.
    let network = LoopbackNetwork::new();
    let a = mesh_node(&network, true);
    let b = mesh_node(&network, true);
    connect_and_enroll(&network, &a, &b);

    let created = a
        .runtime
        .create_incident(incident("avalanche at mount abu"))
        .unwrap();

    wait_for("the incident to replicate to B", &[&a, &b], || {
        b.runtime.list_incidents(None).unwrap().len() == 1
    });

    // B must index it on its own initiative.
    wait_for("B to index the replicated incident", &[&a, &b], || {
        state_of(&b, &created.id) == Some(IndexState::Indexed)
    });

    // Each node holds its own vector; none was transmitted.
    assert_eq!(vectors(&a), 1);
    assert_eq!(vectors(&b), 1);

    // 13 & 14: B can retrieve it, attributed to the incident.
    let answer = b
        .runtime
        .ask_intelligence("avalanche at mount abu", Some(5))
        .unwrap();
    assert!(!answer.refused, "B should answer from its own index");
    assert!(answer.grounded);
    assert!(
        answer.sources.iter().any(|s| s.subject_id == created.id),
        "the answer must be attributed to the replicated incident"
    );
    assert!(
        b.generator.invocations() > 0,
        "the model must actually be invoked once evidence exists"
    );
}

// ---------------------------------------------------------------------------
// 9 & 10. AI absence never blocks capture or replication
// ---------------------------------------------------------------------------

#[test]
fn replication_is_unaffected_by_an_unavailable_embedder() {
    let network = LoopbackNetwork::new();
    // Neither node can embed anything.
    let a = mesh_node(&network, false);
    let b = mesh_node(&network, false);
    connect_and_enroll(&network, &a, &b);

    a.runtime
        .create_incident(incident("replicates without AI"))
        .unwrap();

    wait_for(
        "the incident to replicate despite no embedder",
        &[&a, &b],
        || b.runtime.list_incidents(None).unwrap().len() == 1,
    );

    assert_eq!(
        b.runtime.list_incidents(None).unwrap()[0].description,
        "replicates without AI"
    );
    assert_eq!(
        vectors(&b),
        0,
        "nothing could be embedded, and that is fine"
    );
}

// ---------------------------------------------------------------------------
// 11 & 13. Status reflects reality, and retrieval works once indexed
// ---------------------------------------------------------------------------

#[test]
fn successful_indexing_updates_status_and_enables_retrieval() {
    let node = node(true);
    let created = node
        .runtime
        .create_incident(incident("avalanche at mount abu"))
        .unwrap();

    wait_for("indexing to complete", &[&node], || {
        state_of(&node, &created.id) == Some(IndexState::Indexed)
    });

    // Counts come from the database, not from a cached number.
    let status = node.runtime.intelligence_status();
    assert_eq!(status.vectors_stored, 1);

    let answer = node
        .runtime
        .ask_intelligence("avalanche at mount abu", Some(5))
        .unwrap();
    assert!(!answer.refused);
    assert!(answer.grounded);
    assert!(
        node.generator.invocations() > 0,
        "the model must be reached once there is evidence to ground an answer"
    );
    assert!(answer.sources.iter().any(|s| s.subject_id == created.id));
}

// ---------------------------------------------------------------------------
// A node with no intelligence at all reports nothing rather than failing
// ---------------------------------------------------------------------------

#[test]
fn a_node_without_a_model_reports_no_index_states() {
    let dir = TempDir::new().unwrap();
    let runtime = NodeRuntime::initialize(dir.path()).unwrap();
    runtime
        .create_incident(incident("no intelligence attached"))
        .unwrap();

    // Absence, not an error: the UI renders no AI column rather than a failure.
    assert!(runtime.incident_index_states().unwrap().is_empty());
    assert_eq!(runtime.list_incidents(None).unwrap().len(), 1);
}
