//! The operational knowledge pack, and retrieval across two kinds of knowledge.
//!
//! # What is being guarded
//!
//! Before this, the index held only incidents, so a freshly provisioned node
//! could answer nothing. The fix is a larger corpus, not a looser pipeline —
//! and the tests here exist mostly to prove the *pipeline* did not move:
//!
//! 1. **Grounding is unchanged.** A question with no relevant local knowledge
//!    still refuses without calling the generator. Adding standing guidance
//!    must not become a licence for the model to answer from what it was
//!    trained on.
//! 2. **Provenance survives retrieval.** An answer that draws on both a
//!    procedure and a live field report must let a reader see which is which.
//!    Standing guidance and one unverified incident carry different weight.
//! 3. **Installation is local and idempotent.** The documents are compiled into
//!    the binary; installing twice adds nothing.
//!
//! A deterministic stub embedder is used throughout. These tests are about what
//! the service does with retrieved passages, and a real model would make them
//! slow and machine-dependent. The end-to-end check against the actual BGE and
//! Qwen models is `examples/knowledge_live_check.rs`, which is run by hand
//! because it needs provisioned models CI does not have.

use securemesh_lib::ai::embedding::{Embedding, EmbeddingEngine};
use securemesh_lib::ai::engine::{
    EngineHealth, GenerationRequest, LocalInferenceEngine, ModelInfo, StructuredRequest,
};
use securemesh_lib::ai::{knowledge_pack, IntelligenceService};
use securemesh_lib::domain::NewIncident;
use securemesh_lib::storage::intelligence::PassageSource;
use securemesh_lib::NodeRuntime;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// A generator that records whether it was called, and quotes what it was given.
///
/// The invocation count is the point of the first half: "the model was not
/// consulted" is a safety property under test, and a stub returning a refusal
/// string would be indistinguishable from one that was never asked.
///
/// Quoting is the point of the second half. A grounded answer restates its
/// context — that is what makes it grounded — and the answer-support check
/// requires it. A stub returning fixed prose unrelated to the passages would be
/// modelling an *ungrounded* answer while claiming to be a grounded one, and
/// would be refused, correctly.
struct CountingModel {
    /// Passage numbers the stub claims to have used.
    cites: Vec<usize>,
    calls: AtomicUsize,
}

impl CountingModel {
    fn citing(cites: &[usize]) -> Self {
        Self {
            cites: cites.to_vec(),
            calls: AtomicUsize::new(0),
        }
    }

    /// The text of the passages the prompt carried, so the answer can quote it.
    fn quote(request: &str) -> String {
        request
            .lines()
            .skip_while(|line| !line.starts_with("CONTEXT:"))
            .take_while(|line| !line.starts_with("QUESTION:"))
            .filter(|line| line.starts_with('['))
            .map(|line| line.trim_start_matches(|c: char| c != ' ').trim())
            .take(2)
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(300)
            .collect()
    }
}

impl LocalInferenceEngine for CountingModel {
    fn health(&self) -> EngineHealth {
        EngineHealth::Ready(ModelInfo {
            model_id: "counting".to_string(),
            display_name: "Counting".to_string(),
            quantisation: "none".to_string(),
            context_tokens: 4096,
            backend: "local-cpu".to_string(),
        })
    }

    fn generate(&self, _request: &GenerationRequest) -> securemesh_lib::CoreResult<String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok("stub".to_string())
    }

    fn generate_structured(
        &self,
        request: &StructuredRequest,
    ) -> securemesh_lib::CoreResult<String> {
        self.calls.fetch_add(1, Ordering::SeqCst);

        let quoted = Self::quote(&request.user);
        let list = self
            .cites
            .iter()
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join(",");

        Ok(format!(
            r#"{{"answer":{},"sources":[{list}],"sufficient":true}}"#,
            serde_json::to_string(&quoted).unwrap()
        ))
    }

    fn unload(&self) {}
}

/// A bag-of-words embedder: two texts sharing vocabulary score highly.
///
/// Crude, deterministic, and enough to distinguish "avalanche" content from
/// "flood" content, which is all these tests need. The real semantic behaviour
/// is BGE's and is not under test here.
struct WordEmbedder;

const DIMENSIONS: usize = 96;

impl EmbeddingEngine for WordEmbedder {
    fn health(&self) -> EngineHealth {
        EngineHealth::Ready(ModelInfo {
            model_id: "word-embed".to_string(),
            display_name: "Word".to_string(),
            quantisation: "none".to_string(),
            context_tokens: 512,
            backend: "local-cpu".to_string(),
        })
    }

    fn embed(&self, text: &str) -> securemesh_lib::CoreResult<Embedding> {
        let mut vector = vec![0.0f32; DIMENSIONS];
        for word in text
            .split(|c: char| !c.is_ascii_alphanumeric())
            .filter(|word| word.len() > 3)
        {
            let lowered = word.to_ascii_lowercase();
            // A stable per-word slot. Collisions are acceptable: this only has
            // to separate topics, not rank them well.
            let slot = lowered.bytes().fold(7usize, |acc, byte| {
                acc.wrapping_mul(31).wrapping_add(byte as usize)
            }) % DIMENSIONS;
            vector[slot] += 1.0;
        }
        Embedding::new(vector, "word-embed")
    }

    fn model_id(&self) -> String {
        "word-embed".to_string()
    }
}

struct Harness {
    _dir: TempDir,
    runtime: NodeRuntime,
    model: Arc<CountingModel>,
}

fn harness(cites: &[usize]) -> Harness {
    let dir = TempDir::new().unwrap();
    let mut runtime = NodeRuntime::initialize(dir.path()).unwrap();
    let model = Arc::new(CountingModel::citing(cites));
    runtime.attach_intelligence(IntelligenceService::new(
        runtime.database_handle(),
        model.clone(),
        Arc::new(WordEmbedder),
    ));

    Harness {
        _dir: dir,
        runtime,
        model,
    }
}

/// Installs the pack and embeds it, as the application does.
fn provisioned(cites: &[usize]) -> Harness {
    let harness = harness(cites);
    harness.runtime.install_operational_knowledge().unwrap();
    harness.runtime.index_intelligence().unwrap();
    harness
}

fn record(harness: &Harness, description: &str) -> String {
    let incident = harness
        .runtime
        .create_incident(NewIncident {
            description: description.to_string(),
            severity: "HIGH".to_string(),
            latitude: None,
            longitude: None,
            accuracy_meters: None,
            location_source: None,
            location_captured_at: None,
        })
        .unwrap();
    harness.runtime.index_intelligence().unwrap();
    incident.id
}

// ---------------------------------------------------------------------------
// 1. Ingestion
// ---------------------------------------------------------------------------

#[test]
fn installing_the_pack_ingests_every_document() {
    let harness = harness(&[1]);

    let report = harness.runtime.install_operational_knowledge().unwrap();

    assert_eq!(report.documents_installed, knowledge_pack::DOCUMENTS.len());
    assert_eq!(report.documents_already_present, 0);
    assert!(
        report.chunks_created >= knowledge_pack::DOCUMENTS.len() as u64,
        "each document should produce at least one chunk"
    );

    let documents = harness.runtime.knowledge_documents().unwrap();
    for expected in knowledge_pack::DOCUMENTS {
        assert!(
            documents.iter().any(|d| d.title == expected.title),
            "{} was not ingested",
            expected.title
        );
    }
}

#[test]
fn every_ingested_document_records_its_source_and_type() {
    let harness = harness(&[1]);
    harness.runtime.install_operational_knowledge().unwrap();

    for document in harness.runtime.knowledge_documents().unwrap() {
        assert_eq!(document.source_type, knowledge_pack::SOURCE_TYPE);
        // The filename, so a citation can be traced back to the text on disk.
        assert!(
            document.source.ends_with(".md"),
            "{} has no traceable source",
            document.title
        );
        assert!(!document.content_hash.is_empty());
    }
}

// ---------------------------------------------------------------------------
// 2. Content hashing and 3. duplicate installation
// ---------------------------------------------------------------------------

#[test]
fn each_document_is_keyed_by_a_distinct_content_hash() {
    let harness = harness(&[1]);
    harness.runtime.install_operational_knowledge().unwrap();

    let mut hashes: Vec<String> = harness
        .runtime
        .knowledge_documents()
        .unwrap()
        .into_iter()
        .map(|document| document.content_hash)
        .collect();
    let total = hashes.len();
    hashes.sort();
    hashes.dedup();

    assert_eq!(hashes.len(), total, "two documents share a content hash");
}

#[test]
fn installing_twice_adds_nothing() {
    let harness = harness(&[1]);
    harness.runtime.install_operational_knowledge().unwrap();
    harness.runtime.index_intelligence().unwrap();

    let before = harness.runtime.knowledge_summary().unwrap();

    let second = harness.runtime.install_operational_knowledge().unwrap();
    harness.runtime.index_intelligence().unwrap();

    assert_eq!(second.documents_installed, 0);
    assert_eq!(
        second.documents_already_present,
        knowledge_pack::DOCUMENTS.len()
    );
    assert_eq!(second.chunks_created, 0);
    assert!(second.was_already_installed());

    let after = harness.runtime.knowledge_summary().unwrap();
    assert_eq!(after.chunks, before.chunks, "chunks were duplicated");
    assert_eq!(after.vectors, before.vectors, "vectors were duplicated");
    assert_eq!(after.operational_documents, before.operational_documents);
}

// ---------------------------------------------------------------------------
// 4-6. Retrieval by topic
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// 7. Combined retrieval
// ---------------------------------------------------------------------------

#[test]
fn one_question_can_retrieve_guidance_and_a_live_incident_together() {
    let harness = provisioned(&[1, 2]);
    record(
        &harness,
        "Avalanche reported at Mount Abu, approach road buried.",
    );

    let answer = harness
        .runtime
        .ask_intelligence(
            "An avalanche was reported at Mount Abu. What should the field team do?",
            Some(6),
        )
        .unwrap();

    assert!(!answer.refused);

    let kinds: Vec<PassageSource> = answer.sources.iter().map(|s| s.source).collect();
    assert!(
        kinds.contains(&PassageSource::OperationalKnowledge),
        "no procedure retrieved: {:?}",
        answer.sources.iter().map(|s| &s.title).collect::<Vec<_>>()
    );
    assert!(
        kinds.contains(&PassageSource::LiveIncident),
        "no incident retrieved: {:?}",
        answer.sources.iter().map(|s| &s.title).collect::<Vec<_>>()
    );
}

// ---------------------------------------------------------------------------
// 8-9. Source attribution and display
// ---------------------------------------------------------------------------

#[test]
fn every_citation_carries_the_kind_of_knowledge_it_came_from() {
    let harness = provisioned(&[1, 2]);
    record(&harness, "Avalanche at Mount Abu blocking the approach.");

    let answer = harness
        .runtime
        .ask_intelligence("avalanche Mount Abu approach", Some(6))
        .unwrap();

    for source in &answer.sources {
        // Never IMPORTED_DOCUMENT here: nothing was imported by hand, so a
        // passage classified that way would mean pack documents lost their
        // source type on the way through retrieval.
        assert!(
            matches!(
                source.source,
                PassageSource::OperationalKnowledge | PassageSource::LiveIncident
            ),
            "{} was classified {:?}",
            source.title,
            source.source
        );
        assert!(!source.title.trim().is_empty(), "a citation has no title");
    }
}

#[test]
fn an_operational_citation_names_the_document_not_the_chunk() {
    let harness = provisioned(&[1]);

    let answer = harness
        .runtime
        .ask_intelligence("evacuation route destination roll call", Some(3))
        .unwrap();

    let cited = answer
        .sources
        .iter()
        .find(|source| source.source == PassageSource::OperationalKnowledge)
        .expect("a procedure should have been retrieved");

    // The title an operator can act on, not a UUID they cannot.
    assert!(
        knowledge_pack::DOCUMENTS
            .iter()
            .any(|document| document.title == cited.title),
        "citation title {:?} is not a pack document",
        cited.title
    );
}

#[test]
fn a_document_imported_by_hand_is_not_promoted_to_operational_knowledge() {
    let harness = provisioned(&[1]);
    harness
        .runtime
        .ingest_document(
            "Operator note",
            "typed by the duty officer",
            "OPERATOR_IMPORT",
            "The generator at the northern checkpoint runs for six hours on a full tank.",
        )
        .unwrap();
    harness.runtime.index_intelligence().unwrap();

    let answer = harness
        .runtime
        .ask_intelligence("generator northern checkpoint tank hours", Some(4))
        .unwrap();

    let note = answer
        .sources
        .iter()
        .find(|source| source.title == "Operator note")
        .expect("the imported note should be retrievable");

    assert_eq!(note.source, PassageSource::ImportedDocument);
}

// ---------------------------------------------------------------------------
// 10. Refusal is preserved
// ---------------------------------------------------------------------------

#[test]
fn the_relevance_threshold_is_unchanged() {
    // The corpus grew; the gate did not move. Stated as a test because a
    // quietly lowered threshold is exactly how "it answers more questions now"
    // turns into "it answers questions it should refuse".
    assert_eq!(securemesh_lib::ai::rag::MIN_RELEVANCE, 0.35);
    assert_eq!(securemesh_lib::ai::rag::DEFAULT_TOP_K, 5);
}

// ---------------------------------------------------------------------------
// 11. No model
// ---------------------------------------------------------------------------

#[test]
fn a_node_with_no_model_reports_an_empty_knowledge_base_rather_than_failing() {
    let dir = TempDir::new().unwrap();
    let runtime = NodeRuntime::initialize(dir.path()).unwrap();

    let summary = runtime.knowledge_summary().unwrap();
    assert_eq!(summary.operational_documents, 0);
    assert_eq!(summary.chunks, 0);
    assert!(!summary.pack_installed);
    // Still reports what the pack *would* provide, so the UI can offer it.
    assert_eq!(
        summary.pack_documents_available,
        knowledge_pack::DOCUMENTS.len()
    );

    // Installing without a model refuses cleanly rather than writing documents
    // that could never be embedded.
    assert!(runtime.install_operational_knowledge().is_err());

    // And the node is otherwise entirely functional.
    assert!(runtime
        .create_incident(NewIncident {
            description: "recorded without any model".to_string(),
            severity: "LOW".to_string(),
            latitude: None,
            longitude: None,
            accuracy_meters: None,
            location_source: None,
            location_captured_at: None,
        })
        .is_ok());
}

// ---------------------------------------------------------------------------
// 12. Offline
// ---------------------------------------------------------------------------

#[test]
fn the_knowledge_pack_reaches_no_network_and_reads_no_file() {
    // The documents are compiled in with `include_str!`, so installation cannot
    // depend on a path being populated or a host being reachable. Asserted over
    // the source because a runtime check could only show that this machine
    // happened not to need the network.
    let source = std::fs::read_to_string("src/ai/knowledge_pack.rs").unwrap();
    let code: String = source
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");

    for forbidden in [
        "http://",
        "https://",
        "reqwest",
        "ureq",
        "TcpStream",
        "fs::read",
        "File::open",
        "download",
    ] {
        assert!(
            !code.contains(forbidden),
            "the knowledge pack references {forbidden}"
        );
    }
    assert!(
        code.contains("include_str!"),
        "the documents are no longer compiled into the binary"
    );
}

#[test]
fn an_empty_knowledge_base_refuses_without_calling_the_model() {
    // Brief behaviour: no operational documents and no relevant incidents means
    // the question is refused outright. The model is never consulted, because
    // with nothing retrieved it could only invent.
    let harness = harness(&[1]);

    let answer = harness
        .runtime
        .ask_intelligence("What should I do in heavy rain?", Some(5))
        .unwrap();

    assert!(answer.refused);
    assert!(answer.sources.is_empty());
    assert_eq!(answer.generation_ms, 0);
    assert_eq!(
        harness.model.calls.load(Ordering::SeqCst),
        0,
        "the model was called with nothing to ground an answer in"
    );
}

#[test]
fn installation_never_happens_on_its_own() {
    // Provisioning is an operator action. A node that installed knowledge at
    // startup would leave nobody able to say where its answers came from.
    let harness = harness(&[1]);

    let summary = harness.runtime.knowledge_summary().unwrap();
    assert_eq!(
        summary.operational_documents, 0,
        "the pack installed itself without being asked"
    );
    assert!(!summary.pack_installed);
}

// ---------------------------------------------------------------------------
// 13-15. Existing behaviour is intact
// ---------------------------------------------------------------------------

#[test]
fn incidents_are_still_indexed_alongside_the_pack() {
    let harness = provisioned(&[1]);

    let before = harness.runtime.knowledge_summary().unwrap();
    record(&harness, "Road blockage at the northern checkpoint.");
    let after = harness.runtime.knowledge_summary().unwrap();

    assert_eq!(after.live_incidents_total, before.live_incidents_total + 1);
    assert_eq!(
        after.live_incidents_indexed,
        before.live_incidents_indexed + 1,
        "the incident did not get a vector"
    );
    // The pack is untouched by incident indexing.
    assert_eq!(after.operational_documents, before.operational_documents);
}

#[test]
fn an_incident_keeps_its_location_after_the_pack_is_installed() {
    let harness = provisioned(&[1]);

    let incident = harness
        .runtime
        .create_incident(NewIncident {
            description: "Heavy rain causing road blockage near checkpoint.".to_string(),
            severity: "HIGH".to_string(),
            latitude: Some(13.133599),
            longitude: Some(77.565330),
            accuracy_meters: Some(6.0),
            location_source: Some(securemesh_lib::domain::LocationSource::Gnss),
            location_captured_at: Some(chrono::Utc::now()),
        })
        .unwrap();

    assert_eq!(incident.accuracy_meters, Some(6.0));
    assert_eq!(
        incident.location_source,
        securemesh_lib::domain::LocationSource::Gnss
    );
}

#[test]
fn the_summary_counts_the_two_kinds_of_knowledge_separately() {
    let harness = provisioned(&[1]);
    record(&harness, "Landslide across the valley track.");
    record(&harness, "Communications lost with the eastern team.");

    let summary = harness.runtime.knowledge_summary().unwrap();

    assert_eq!(
        summary.operational_documents,
        knowledge_pack::DOCUMENTS.len() as u64
    );
    assert_eq!(summary.live_incidents_total, 2);
    assert_eq!(summary.live_incidents_indexed, 2);
    assert!(summary.pack_installed);
    // Vectors cover both: chunks from the pack, plus one per incident.
    assert_eq!(summary.vectors, summary.chunks + 2);
}

// ---------------------------------------------------------------------------
// Live models
// ---------------------------------------------------------------------------
//
// Everything above uses a stub embedder, which is fine for plumbing but cannot
// judge retrieval *quality*: a bag-of-words vector for a six-word question
// scores far below the 0.35 threshold against an 800-character passage, however
// well the two match in meaning. Whether "What should I do in heavy rain?"
// finds the heavy rain procedure is a property of BGE and of the documents
// themselves, and only the real model can answer it.
//
// These tests therefore load the provisioned models. When none are installed
// they report that and pass, because a machine without models is a normal
// machine — not a failing one. The skip is printed rather than silent, so a run
// that proved nothing says so.

mod live {
    use super::*;
    use securemesh_lib::ai::{LlamaConfig, LlamaServerEngine};
    use std::path::{Path, PathBuf};

    fn project_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("project root")
            .to_path_buf()
    }

    /// A node backed by the real BGE and Qwen, or `None` if not provisioned.
    ///
    /// Ports are offset from the application's so a running node is not
    /// disturbed by a test run.
    fn live_node() -> Option<(TempDir, NodeRuntime)> {
        let root = project_root();
        let embedding = LlamaConfig::embedding(&root, 19_411);
        let generation = LlamaConfig::generation(&root, 19_410);

        for config in [&embedding, &generation] {
            if let Err(reason) = config.availability() {
                println!("SKIPPED — models are not provisioned: {}", reason.detail());
                return None;
            }
        }

        let dir = TempDir::new().unwrap();
        let mut runtime = NodeRuntime::initialize(dir.path()).unwrap();
        runtime.attach_intelligence(IntelligenceService::new(
            runtime.database_handle(),
            Arc::new(LlamaServerEngine::new(generation)),
            Arc::new(LlamaServerEngine::new(embedding)),
        ));
        Some((dir, runtime))
    }

    fn sources_of(answer: &securemesh_lib::ai::GroundedAnswer) -> Vec<(String, PassageSource)> {
        answer
            .sources
            .iter()
            .map(|source| (source.title.clone(), source.source))
            .collect()
    }

    /// One model load, five questions.
    ///
    /// Written as a single test on purpose: starting `llama-server` costs
    /// seconds, and five tests would pay that five times for no extra
    /// information. The questions are the ones the feature was specified
    /// against, checked in order.
    #[test]
    fn the_specified_questions_behave_as_required_against_the_real_models() {
        let Some((_dir, runtime)) = live_node() else {
            return;
        };

        runtime.install_operational_knowledge().unwrap();
        runtime.index_intelligence().unwrap();
        runtime
            .create_incident(NewIncident {
                description:
                    "Avalanche reported at Mount Abu. The approach road is buried and two \
                     vehicles are cut off beyond the slide."
                        .to_string(),
                severity: "CRITICAL".to_string(),
                latitude: None,
                longitude: None,
                accuracy_meters: None,
                location_source: None,
                location_captured_at: None,
            })
            .unwrap();
        runtime.index_intelligence().unwrap();

        // --- A. Operational knowledge, no matching incident ----------------
        let a = runtime
            .ask_intelligence("What should I do in heavy rain?", Some(5))
            .unwrap();
        assert!(!a.refused, "A refused: {}", a.answer);
        assert!(
            a.sources
                .iter()
                .any(|s| s.source == PassageSource::OperationalKnowledge),
            "A retrieved no guidance: {:?}",
            sources_of(&a)
        );

        // --- B. A different procedure --------------------------------------
        let b = runtime
            .ask_intelligence("What should I do during an avalanche?", Some(5))
            .unwrap();
        assert!(!b.refused, "B refused: {}", b.answer);
        assert!(
            b.sources.iter().any(|s| s.title == "Avalanche Response"),
            "B did not retrieve the avalanche procedure: {:?}",
            sources_of(&b)
        );

        // --- C. A live incident --------------------------------------------
        let c = runtime
            .ask_intelligence("What happened at Mount Abu?", Some(5))
            .unwrap();
        assert!(!c.refused, "C refused: {}", c.answer);
        assert!(
            c.sources
                .iter()
                .any(|s| s.source == PassageSource::LiveIncident),
            "C retrieved no incident: {:?}",
            sources_of(&c)
        );

        // --- D. Both kinds together ----------------------------------------
        let d = runtime
            .ask_intelligence(
                "An avalanche was reported at Mount Abu. What should the field team do?",
                Some(6),
            )
            .unwrap();
        assert!(!d.refused, "D refused: {}", d.answer);
        let kinds: Vec<PassageSource> = d.sources.iter().map(|s| s.source).collect();
        assert!(
            kinds.contains(&PassageSource::OperationalKnowledge)
                && kinds.contains(&PassageSource::LiveIncident),
            "D did not combine both kinds: {:?}",
            sources_of(&d)
        );

        // --- E. Nothing local supports an answer ---------------------------
        let e = runtime
            .ask_intelligence("What is the capital of France?", Some(5))
            .unwrap();
        assert!(
            e.refused,
            "E was answered from pretrained knowledge: {} (sources {:?})",
            e.answer,
            sources_of(&e)
        );
        assert!(e.sources.is_empty());
        // The generator *was* called here, and that is correct: with eleven
        // documents of emergency prose, some passage always clears 0.35 for any
        // English sentence, so retrieval alone cannot tell this question apart.
        // What catches it is comparing the answer with the passages it cited.
        assert!(
            e.answer_support < securemesh_lib::ai::rag::MIN_ANSWER_SUPPORT,
            "E was scored as supported: {:.2}",
            e.answer_support
        );

        // Every genuine answer above must clear the same bar comfortably, or
        // the gate is refusing real answers to catch fabricated ones.
        for (name, answer) in [("A", &a), ("B", &b), ("C", &c), ("D", &d)] {
            assert!(
                answer.answer_support >= securemesh_lib::ai::rag::MIN_ANSWER_SUPPORT,
                "{name} was refused as unsupported: {:.2}",
                answer.answer_support
            );
        }
    }
}
