//! The intelligence service: what the rest of SecureMesh talks to.
//!
//! # AI is a layer, never a dependency
//!
//! Every method here degrades. A node with no model provisioned, or whose
//! runtime failed, keeps creating incidents, keeps replicating them, and keeps
//! serving its dashboard — it simply reports intelligence as unavailable.
//!
//! That is enforced by shape rather than discipline: this service is held as an
//! `Option` by the runtime, and analysis is never on the path of incident
//! creation or synchronisation. There is no code path in which a model failure
//! can block a record being written or sent.
//!
//! # What this service can reach
//!
//! A database handle and two engines. It has no identity, no keystore, no trust
//! store mutation, no sync engine, and no filesystem access beyond the model
//! files the engines opened. The AI trust boundary is this struct's field list.

use crate::ai::dataset;
use crate::ai::embedding::{Embedding, EmbeddingEngine};
use crate::ai::engine::{EngineHealth, LocalInferenceEngine, StructuredRequest};
use crate::ai::gate::{GateSnapshot, InferenceGate};
use crate::ai::insight::{self, Candidate, IncidentInsight, SituationBrief};
use crate::ai::knowledge_pack;
use crate::ai::nlp::{self, TextExtraction};
use crate::ai::prompt;
use crate::ai::rag::{self, GroundedAnswer};
use crate::ai::Unavailable;
use crate::domain::{Incident, IncidentCategory, RawAnalysis};
use crate::error::{CoreError, CoreResult};
use crate::storage::intelligence::{EmbeddingKind, KnowledgeDocument};
use crate::storage::Database;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// Tokens allowed for one incident analysis.
const ANALYSIS_TOKENS: u32 = 512;
/// Tokens allowed for a situation summary: three sentences.
const SUMMARY_TOKENS: u32 = 220;
/// Longest stored summary, in characters.
const MAX_SUMMARY_CHARS: usize = 800;
/// Share of a summary's content words that must appear in the facts it was
/// given. Below this it is drawing on the model, not the node's records.
pub const MIN_SUMMARY_SUPPORT: f32 = 0.5;
/// Incidents an insight compares against, newest first.
const INSIGHT_CANDIDATES: u32 = 1_000;
/// Chunk size, in characters, for ingested documents.
const CHUNK_CHARS: usize = 900;
/// Sentences repeated between adjacent chunks.
const CHUNK_OVERLAP: usize = 1;
/// Items embedded per indexing pass, so a large corpus does not block for
/// minutes in one call.
const INDEX_BATCH: u32 = 64;

/// What the dashboard shows about local intelligence.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IntelligenceStatus {
    /// `READY`, `LOADING`, or `UNAVAILABLE`.
    pub state: String,
    pub detail: String,
    pub model_name: Option<String>,
    pub model_id: Option<String>,
    pub quantisation: Option<String>,
    /// Always `"LOCAL"`. Present so the UI states it rather than implying it.
    pub inference: String,
    /// Always `"NONE"`.
    pub network_dependency: String,
    pub embedding_model: Option<String>,
    pub analyses_stored: u64,
    pub documents_indexed: u64,
    pub chunks_indexed: u64,
    pub vectors_stored: u64,
}

/// What local knowledge this node holds.
///
/// Reported as counts of distinct things rather than one total, because the
/// distinction is the point: standing guidance and live incident reports are
/// different kinds of knowledge and an operator needs to see both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeBaseSummary {
    /// Documents from the provisioned operational knowledge pack.
    pub operational_documents: u64,
    /// Any other imported document, including the synthetic evaluation corpus.
    pub imported_documents: u64,
    /// Incidents that hold a vector, and are therefore actually searchable.
    pub live_incidents_indexed: u64,
    /// Incidents this node holds, indexed or not. A gap between the two means
    /// indexing is still catching up, which is normal and not an error.
    pub live_incidents_total: u64,
    pub chunks: u64,
    pub vectors: u64,
    /// How many documents the pack compiled into this binary contains.
    pub pack_documents_available: usize,
    /// True when every pack document is present in the index.
    pub pack_installed: bool,
}

/// Outcome of an indexing pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct IndexReport {
    pub chunks_embedded: usize,
    pub incidents_embedded: usize,
    pub failures: usize,
}

/// Local intelligence over a node's own records.
pub struct IntelligenceService {
    database: Arc<Database>,
    generator: Arc<dyn LocalInferenceEngine>,
    embedder: Arc<dyn EmbeddingEngine>,
    /// One generation at a time, a short bounded queue, then refusal.
    gate: InferenceGate,
    /// Category prototype vectors for the semantic fallback, embedded once.
    prototypes: Mutex<Option<Vec<(IncidentCategory, Embedding)>>>,
}

impl IntelligenceService {
    pub fn new(
        database: Arc<Database>,
        generator: Arc<dyn LocalInferenceEngine>,
        embedder: Arc<dyn EmbeddingEngine>,
    ) -> Self {
        Self {
            database,
            generator,
            embedder,
            gate: InferenceGate::default(),
            prototypes: Mutex::new(None),
        }
    }

    /// Whether the generation model is busy, and how many requests wait.
    pub fn gate_snapshot(&self) -> GateSnapshot {
        self.gate.snapshot()
    }

    /// Current state, for the dashboard.
    ///
    /// Never fails: a status call that errored would leave the UI unable to
    /// report that the subsystem is broken, which is precisely when it matters.
    pub fn status(&self) -> IntelligenceStatus {
        let health = self.generator.health();

        let (state, detail, model_name, model_id, quantisation) = match &health {
            EngineHealth::Ready(info) => (
                "READY",
                "Local model available. Inference runs on this device.".to_string(),
                Some(info.display_name.clone()),
                Some(info.model_id.clone()),
                Some(info.quantisation.clone()),
            ),
            EngineHealth::Loading => (
                "LOADING",
                "The local model is loading.".to_string(),
                None,
                None,
                None,
            ),
            EngineHealth::Unavailable(reason) => ("UNAVAILABLE", reason.detail(), None, None, None),
        };

        let embedding_model = match self.embedder.health() {
            EngineHealth::Ready(info) => Some(info.display_name),
            _ => None,
        };

        IntelligenceStatus {
            state: state.to_string(),
            detail,
            model_name,
            model_id,
            quantisation,
            inference: "LOCAL".to_string(),
            network_dependency: "NONE".to_string(),
            embedding_model,
            // Counts degrade to zero rather than failing the status call.
            analyses_stored: self.database.count_analyses().unwrap_or(0),
            documents_indexed: self
                .database
                .list_documents()
                .map(|d| d.len() as u64)
                .unwrap_or(0),
            chunks_indexed: self.database.count_knowledge_chunks().unwrap_or(0),
            vectors_stored: self.database.count_embeddings().unwrap_or(0),
        }
    }

    /// Whether the generation model can serve a request.
    pub fn is_ready(&self) -> bool {
        self.generator.health().is_ready()
    }

    /// Analyses one incident and stores the result.
    ///
    /// The incident text is fenced as data; the output is schema-constrained,
    /// then parsed, then validated, and only then stored. A model that produces
    /// nonsense yields an error, never a corrupt record.
    pub fn analyse_incident(
        &self,
        incident_id: &str,
    ) -> CoreResult<crate::ai::consistency::AnalysisOutcome> {
        let incident = self.database.get_incident(incident_id)?;

        let model_id = self
            .generator
            .model_info()
            .map(|info| info.model_id)
            .ok_or_else(|| CoreError::from(Unavailable::Disabled))?;

        // The rule layer's findings go to the model as labelled, fallible
        // context: the model is asked to judge, not to rediscover counts.
        //
        // The same extraction is reused after the answer comes back, so the
        // rules run once per analysis rather than twice — and the evidence the
        // model was shown is exactly the evidence its answer is checked against.
        let extraction = nlp::extract(&incident.description);
        let facts = nlp::facts_line(&extraction);
        let request = StructuredRequest {
            system: prompt::analysis_system_prompt(),
            user: prompt::analysis_user_message_with_facts(&incident.description, &facts),
            schema: prompt::analysis_schema(),
            max_tokens: ANALYSIS_TOKENS,
        };

        let _permit = self.gate.acquire()?;
        let started = std::time::Instant::now();
        let raw_output = self.generator.generate_structured(&request)?;
        let latency_ms = started.elapsed().as_millis() as u64;

        // Untrusted from here to `validate`.
        let parsed: RawAnalysis = serde_json::from_str(&raw_output).map_err(|_| {
            CoreError::validation("the local model produced output that is not valid analysis JSON")
        })?;

        let analysis = parsed.validate(incident_id, &model_id, latency_ms)?;
        self.database.store_analysis(&analysis)?;

        // After the model output has been parsed and validated, and before the
        // result is treated as operator-facing intelligence. This never alters
        // `analysis`: it records where the model and the report's own stated
        // facts disagree, so an operator can see both. A model talked into
        // "severity LOW" by the report it is reading still produces that
        // answer — but it no longer arrives unaccompanied.
        let consistency =
            crate::ai::consistency::check_against(&analysis, &extraction, &incident.description);

        Ok(crate::ai::consistency::AnalysisOutcome {
            analysis,
            consistency,
        })
    }

    /// The stored analysis for an incident, with the rules' current verdict.
    ///
    /// The consistency report is recomputed rather than stored. It is a pure
    /// function of the analysis and the report text, so recomputing it returns
    /// exactly what was produced when the model ran — and if the rule layer is
    /// improved later, an operator sees today's evidence rather than a verdict
    /// frozen at the moment of inference. It also means no storage migration:
    /// `IncidentAnalysis` and its table are unchanged.
    pub fn analysis_for(
        &self,
        incident_id: &str,
    ) -> CoreResult<Option<crate::ai::consistency::AnalysisOutcome>> {
        let Some(analysis) = self.database.get_analysis(incident_id)? else {
            return Ok(None);
        };
        let incident = self.database.get_incident(incident_id)?;
        let consistency = crate::ai::consistency::check(&analysis, &incident.description);

        Ok(Some(crate::ai::consistency::AnalysisOutcome {
            analysis,
            consistency,
        }))
    }

    /// Ingests a document: normalise, chunk, store.
    ///
    /// Embedding happens separately in [`Self::index_pending`], so importing a
    /// large corpus does not block on inference.
    pub fn ingest_document(
        &self,
        title: &str,
        source: &str,
        source_type: &str,
        text: &str,
    ) -> CoreResult<Option<String>> {
        if title.trim().is_empty() {
            return Err(CoreError::validation("a document needs a title"));
        }

        let chunks = rag::chunk_text(text, CHUNK_CHARS, CHUNK_OVERLAP);
        if chunks.is_empty() {
            return Err(CoreError::validation("the document has no usable text"));
        }

        self.database.import_document(
            title,
            source,
            source_type,
            &rag::content_hash(text),
            &chunks,
        )
    }

    /// Embeds whatever is not yet indexed.
    ///
    /// Incremental and resumable: a pass that is interrupted loses only the
    /// items it had not finished, and the next pass picks up from there.
    pub fn index_pending(&self) -> CoreResult<IndexReport> {
        let model_id = self.embedder.model_id();
        let mut report = IndexReport::default();

        for chunk in self
            .database
            .chunks_awaiting_embedding(&model_id, INDEX_BATCH)?
        {
            match self.embedder.embed(&chunk.content) {
                Ok(embedding) => {
                    self.database.store_embedding(
                        EmbeddingKind::KnowledgeChunk,
                        &chunk.id,
                        &embedding,
                    )?;
                    report.chunks_embedded += 1;
                }
                // One unembeddable item must not abandon the whole pass.
                Err(_) => report.failures += 1,
            }
        }

        for (incident_id, description) in self
            .database
            .incidents_awaiting_embedding(&model_id, INDEX_BATCH)?
        {
            match self.embedder.embed(&description) {
                Ok(embedding) => {
                    self.database.store_embedding(
                        EmbeddingKind::Incident,
                        &incident_id,
                        &embedding,
                    )?;
                    report.incidents_embedded += 1;
                }
                Err(_) => report.failures += 1,
            }
        }

        Ok(report)
    }

    /// Answers a question from this node's records only.
    pub fn ask(&self, question: &str, top_k: Option<usize>) -> CoreResult<GroundedAnswer> {
        if question.trim().is_empty() {
            return Err(CoreError::validation("a question cannot be empty"));
        }
        let _permit = self.gate.acquire()?;
        rag::answer_question(
            &self.database,
            self.embedder.as_ref(),
            self.generator.as_ref(),
            question,
            top_k.unwrap_or(rag::DEFAULT_TOP_K),
        )
    }

    pub fn documents(&self) -> CoreResult<Vec<KnowledgeDocument>> {
        self.database.list_documents()
    }

    /// What this node holds locally, for the Knowledge Base panel.
    ///
    /// Reads only. Nothing here provisions, downloads or indexes — an operator
    /// looking at the panel must not be causing work by looking.
    pub fn knowledge_summary(&self) -> CoreResult<KnowledgeBaseSummary> {
        let documents = self.database.list_documents()?;
        let operational_documents = documents
            .iter()
            .filter(|document| document.source_type == knowledge_pack::SOURCE_TYPE)
            .count() as u64;

        Ok(KnowledgeBaseSummary {
            operational_documents,
            imported_documents: documents.len() as u64 - operational_documents,
            live_incidents_indexed: self
                .database
                .count_embeddings_of_kind(EmbeddingKind::Incident)?,
            live_incidents_total: self.database.count_incidents()?,
            chunks: self.database.count_knowledge_chunks()?,
            vectors: self.database.count_embeddings()?,
            pack_documents_available: knowledge_pack::DOCUMENTS.len(),
            pack_installed: operational_documents as usize >= knowledge_pack::DOCUMENTS.len(),
        })
    }

    /// Incidents that hold no vector for the embedding model in use.
    ///
    /// Derived from the absence of a stored vector rather than from a status
    /// column, so it cannot disagree with what retrieval can actually find: an
    /// incident is searchable exactly when a vector exists for it.
    pub fn unindexed_incident_ids(&self, limit: u32) -> CoreResult<Vec<String>> {
        Ok(self
            .database
            .incidents_awaiting_embedding(&self.embedder.model_id(), limit)?
            .into_iter()
            .map(|(incident_id, _description)| incident_id)
            .collect())
    }

    /// Every incident this node holds, newest first.
    pub fn incident_ids(&self, limit: u32) -> CoreResult<Vec<String>> {
        Ok(self
            .database
            .list_incidents(Some(limit))?
            .into_iter()
            .map(|incident| incident.id)
            .collect())
    }

    /// Installs the operational knowledge pack compiled into this binary.
    ///
    /// Explicit, never automatic: nothing is provisioned at startup, so an
    /// operator can always answer the question "where did this text come
    /// from?" with "I installed it, from the binary".
    ///
    /// **Idempotent.** Each document is keyed by a SHA-256 of its normalised
    /// text, so a second install adds no documents, no chunks and no vectors.
    /// Re-running it is how an operator confirms the pack is present, which
    /// means it must be safe to run.
    ///
    /// Embedding is not done here. Vectors are produced by
    /// [`Self::index_pending`], which is the same path incidents take.
    pub fn install_operational_knowledge(&self) -> CoreResult<knowledge_pack::InstallReport> {
        let chunks_before = self.database.count_knowledge_chunks()?;
        let mut report = knowledge_pack::InstallReport::default();

        for document in knowledge_pack::DOCUMENTS {
            let stored = self.ingest_document(
                document.title,
                document.source,
                knowledge_pack::SOURCE_TYPE,
                document.text,
            )?;
            match stored {
                Some(_) => report.documents_installed += 1,
                None => report.documents_already_present += 1,
            }
        }

        report.chunks_created = self
            .database
            .count_knowledge_chunks()?
            .saturating_sub(chunks_before);
        Ok(report)
    }

    /// How much of the pack is already present.
    ///
    /// Read separately from installing it, so the UI can show the state without
    /// writing anything.
    pub fn operational_document_count(&self) -> CoreResult<usize> {
        Ok(self
            .database
            .list_documents()?
            .iter()
            .filter(|document| document.source_type == knowledge_pack::SOURCE_TYPE)
            .count())
    }

    /// Loads the synthetic evaluation corpus as knowledge documents.
    ///
    /// Development and demonstration only. Every record is labelled synthetic
    /// in the data itself and in its source type, so a corpus can be audited
    /// for anything that should not be there.
    pub fn load_synthetic_corpus(&self, seed: u64, count: usize) -> CoreResult<usize> {
        let generated = dataset::generate(seed, count);
        let mut imported = 0usize;

        for incident in &generated.incidents {
            let text = format!(
                "Incident {} ({}). {} Zone: {}.",
                incident.id, incident.expected_category, incident.description, incident.zone
            );
            let stored = self.database.import_document(
                &format!("Synthetic incident {}", incident.id),
                &format!("{} seed {}", generated.version, generated.seed),
                "SYNTHETIC",
                &rag::content_hash(&text),
                &[text],
            )?;
            if stored.is_some() {
                imported += 1;
            }
        }

        Ok(imported)
    }

    // --- Insight and situation brief ---------------------------------------

    /// Category prototype vectors, embedded on first use and cached.
    ///
    /// `None` when the embedder cannot serve; a failure is not cached, so the
    /// next call retries once the model is back.
    fn prototypes(&self) -> Option<Vec<(IncidentCategory, Embedding)>> {
        let mut cached = self
            .prototypes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(prototypes) = cached.as_ref() {
            return Some(prototypes.clone());
        }

        let mut built = Vec::new();
        for category in IncidentCategory::ALL {
            if category == IncidentCategory::Other {
                continue;
            }
            built.push((
                category,
                self.embedder
                    .embed(&insight::prototype_text(category))
                    .ok()?,
            ));
        }
        *cached = Some(built.clone());
        Some(built)
    }

    /// Incidents, their rule extractions, and whatever vectors exist.
    fn candidates(
        &self,
        limit: u32,
    ) -> CoreResult<(
        Vec<Incident>,
        Vec<TextExtraction>,
        HashMap<String, Embedding>,
    )> {
        let incidents = self.database.list_incidents(Some(limit))?;
        let extractions = incidents
            .iter()
            .map(|i| nlp::extract(&i.description))
            .collect();
        // Vectors are an enhancement: if they cannot be read, matching falls
        // back to the lexical score rather than failing.
        let vectors = self
            .database
            .incident_embeddings(&self.embedder.model_id())
            .unwrap_or_default()
            .into_iter()
            .collect();
        Ok((incidents, extractions, vectors))
    }

    /// Everything derived about one incident: rule extraction, category,
    /// severity with reasons, and related reports.
    ///
    /// Never calls the generation model, so it is fast and never waits on the
    /// gate. Computed on demand and not stored.
    pub fn insight(&self, incident_id: &str) -> CoreResult<IncidentInsight> {
        let started = Instant::now();
        let target = self.database.get_incident(incident_id)?;
        let (incidents, extractions, vectors) = self.candidates(INSIGHT_CANDIDATES)?;

        let target_extraction = nlp::extract(&target.description);
        let target_vector = vectors.get(&target.id);
        let semantic = target_vector
            .and_then(|vector| Some((vector, self.prototypes()?)))
            .and_then(|(vector, prototypes)| insight::semantic_category(vector, &prototypes));

        let others: Vec<Candidate<'_>> = incidents
            .iter()
            .zip(extractions.iter())
            .map(|(incident, extraction)| Candidate {
                incident,
                extraction,
                vector: vectors.get(&incident.id),
            })
            .collect();
        let target_candidate = Candidate {
            incident: &target,
            extraction: &target_extraction,
            vector: target_vector,
        };

        let analysis = self.database.get_analysis(incident_id).unwrap_or(None);
        Ok(insight::build_insight(
            &target_candidate,
            &others,
            semantic,
            analysis.as_ref(),
            target_vector.is_some(),
            started.elapsed().as_millis() as u64,
        ))
    }

    /// A digest across the incidents this node holds.
    ///
    /// The figures are deterministic. With `summarise`, the model is also
    /// asked for a short prose summary of *those figures only*; a summary that
    /// is not supported by them is withheld and the reason stated.
    pub fn situation_brief(&self, summarise: bool) -> CoreResult<SituationBrief> {
        let started = Instant::now();
        let (incidents, extractions, vectors) = self.candidates(insight::BRIEF_LIMIT as u32)?;
        let candidates: Vec<Candidate<'_>> = incidents
            .iter()
            .zip(extractions.iter())
            .map(|(incident, extraction)| Candidate {
                incident,
                extraction,
                vector: vectors.get(&incident.id),
            })
            .collect();

        let mut brief = insight::build_brief(&candidates, 0);

        if summarise {
            if brief.incidents_considered == 0 {
                brief.summary_note = Some("There are no incidents to summarise.".to_string());
            } else {
                match self.summarise(&brief) {
                    Ok((summary, support, model_id)) => {
                        brief.summary_support = Some(support);
                        brief.summary_model = Some(model_id);
                        if support >= MIN_SUMMARY_SUPPORT {
                            brief.summary = Some(summary);
                        } else {
                            brief.summary_note = Some(
                                "The model's summary used material that is not in this node's \
                                 records, so it was withheld."
                                    .to_string(),
                            );
                        }
                    }
                    Err(error) => brief.summary_note = Some(error.message().to_string()),
                }
            }
        }

        brief.elapsed_ms = started.elapsed().as_millis() as u64;
        Ok(brief)
    }

    /// Asks the model to summarise a brief. Untrusted output, checked by the
    /// caller for support.
    fn summarise(&self, brief: &SituationBrief) -> CoreResult<(String, f32, String)> {
        let model_id = self
            .generator
            .model_info()
            .map(|info| info.model_id)
            .ok_or_else(|| CoreError::from(Unavailable::Disabled))?;

        let context = insight::brief_context(brief);
        let request = StructuredRequest {
            system: prompt::BRIEF_SYSTEM_PROMPT.to_string(),
            user: prompt::brief_user_message(&context),
            schema: prompt::brief_schema(),
            max_tokens: SUMMARY_TOKENS,
        };

        let _permit = self.gate.acquire()?;
        let raw = self.generator.generate_structured(&request)?;

        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct RawSummary {
            summary: String,
        }
        let parsed: RawSummary = serde_json::from_str(&raw).map_err(|_| {
            CoreError::validation("the local model produced output that is not a valid summary")
        })?;

        let summary: String = parsed
            .summary
            .trim()
            .chars()
            .take(MAX_SUMMARY_CHARS)
            .collect();
        if summary.is_empty() {
            return Err(CoreError::validation(
                "the local model produced an empty summary",
            ));
        }
        let support = insight::support_score(&summary, &context);
        Ok((summary, (support * 100.0).round() / 100.0, model_id))
    }

    /// Releases model memory.
    pub fn unload(&self) {
        self.generator.unload();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::embedding::Embedding;
    use crate::ai::engine::{GenerationRequest, ModelInfo};
    use crate::domain::NewIncident;
    use crate::identity::keystore::FileKeyStore;
    use crate::identity::NodeIdentity;
    use std::sync::Mutex;
    use tempfile::TempDir;

    /// A stub engine, so service behaviour can be tested without a model.
    ///
    /// Deterministic by construction: the tests below are about how the service
    /// handles model output, and a real model would make them slow and
    /// unrepeatable.
    struct StubEngine {
        health: EngineHealth,
        /// What an analysis request returns, or an error.
        structured: Mutex<Result<String, String>>,
        /// What a question request returns. Kept separate because analysis and
        /// RAG use different schemas, and a stub that ignored the difference
        /// would not exercise the path the service actually takes.
        answer: Mutex<String>,
        text: Mutex<String>,
    }

    impl StubEngine {
        fn ready(structured: &str) -> Self {
            Self {
                health: EngineHealth::Ready(ModelInfo {
                    model_id: "stub-model".to_string(),
                    display_name: "Stub".to_string(),
                    quantisation: "none".to_string(),
                    context_tokens: 2048,
                    backend: "local-cpu".to_string(),
                }),
                structured: Mutex::new(Ok(structured.to_string())),
                answer: Mutex::new(
                    r#"{"answer":"Evacuate low ground.","sources":[1],"sufficient":true}"#
                        .to_string(),
                ),
                text: Mutex::new("answer citing [S1]".to_string()),
            }
        }

        fn unavailable() -> Self {
            Self {
                health: EngineHealth::Unavailable(Unavailable::ModelMissing("x.gguf".to_string())),
                structured: Mutex::new(Err("no model".to_string())),
                answer: Mutex::new(String::new()),
                text: Mutex::new(String::new()),
            }
        }

        /// Replaces what a question request returns, so a test can hand the
        /// service hostile or malformed answer output.
        fn answering(self, answer: &str) -> Self {
            *self.answer.lock().unwrap() = answer.to_string();
            self
        }
    }

    impl LocalInferenceEngine for StubEngine {
        fn health(&self) -> EngineHealth {
            self.health.clone()
        }

        fn generate(&self, _request: &GenerationRequest) -> CoreResult<String> {
            if !self.health.is_ready() {
                return Err(CoreError::internal("model unavailable"));
            }
            Ok(self.text.lock().unwrap().clone())
        }

        fn generate_structured(&self, request: &StructuredRequest) -> CoreResult<String> {
            // Dispatch on the schema, the way a real constrained decoder does.
            if request.schema["properties"].get("sufficient").is_some() {
                if !self.health.is_ready() {
                    return Err(CoreError::internal("model unavailable"));
                }
                return Ok(self.answer.lock().unwrap().clone());
            }

            self.structured
                .lock()
                .unwrap()
                .clone()
                .map_err(CoreError::internal)
        }

        fn unload(&self) {}
    }

    /// A stub embedder producing a vector from a simple hash of the text, so
    /// identical text embeds identically and different text does not.
    struct StubEmbedder {
        available: bool,
    }

    impl EmbeddingEngine for StubEmbedder {
        fn health(&self) -> EngineHealth {
            if self.available {
                EngineHealth::Ready(ModelInfo {
                    model_id: "stub-embed".to_string(),
                    display_name: "Stub Embedder".to_string(),
                    quantisation: "none".to_string(),
                    context_tokens: 512,
                    backend: "local-cpu".to_string(),
                })
            } else {
                EngineHealth::Unavailable(Unavailable::Disabled)
            }
        }

        fn embed(&self, text: &str) -> CoreResult<Embedding> {
            if !self.available {
                return Err(CoreError::internal("embedder unavailable"));
            }
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

    struct Fixture {
        _dir: TempDir,
        service: IntelligenceService,
        database: Arc<Database>,
        node_id: String,
    }

    fn fixture(generator: StubEngine, embedder_available: bool) -> Fixture {
        let dir = TempDir::new().unwrap();
        let identity =
            NodeIdentity::load_or_create(&FileKeyStore::new(dir.path().join("id.json"))).unwrap();
        let database = Arc::new(Database::open(dir.path().join("node.sqlite")).unwrap());
        database
            .register_local_node(
                identity.node_id(),
                identity.node_name(),
                &identity.public_key_hex(),
                identity.created_at(),
            )
            .unwrap();

        let service = IntelligenceService::new(
            Arc::clone(&database),
            Arc::new(generator),
            Arc::new(StubEmbedder {
                available: embedder_available,
            }),
        );

        Fixture {
            _dir: dir,
            service,
            database,
            node_id: identity.node_id().to_string(),
        }
    }

    fn incident(f: &Fixture, description: &str) -> String {
        let validated = NewIncident {
            description: description.to_string(),
            severity: "HIGH".to_string(),
            latitude: None,
            longitude: None,
            accuracy_meters: None,
            location_source: None,
            location_captured_at: None,
        }
        .validate(&f.node_id)
        .unwrap();
        f.database.insert_incident(&validated).unwrap();
        validated.id
    }

    const GOOD_OUTPUT: &str = r#"{
        "category": "FLOODING",
        "severity": "HIGH",
        "summary": "Flooding reported in the northern zone.",
        "access_status": "BLOCKED",
        "entities": ["2 vehicles"]
    }"#;

    // --- Availability ------------------------------------------------------

    #[test]
    fn status_reports_unavailable_without_failing() {
        let f = fixture(StubEngine::unavailable(), false);
        let status = f.service.status();

        assert_eq!(status.state, "UNAVAILABLE");
        assert!(status.detail.contains("PROVISIONING"));
        assert!(!f.service.is_ready());
        // The honest constants stay true whatever the state.
        assert_eq!(status.inference, "LOCAL");
        assert_eq!(status.network_dependency, "NONE");
    }

    #[test]
    fn status_reports_the_loaded_model_when_ready() {
        let f = fixture(StubEngine::ready(GOOD_OUTPUT), true);
        let status = f.service.status();

        assert_eq!(status.state, "READY");
        assert_eq!(status.model_id.as_deref(), Some("stub-model"));
        assert_eq!(status.embedding_model.as_deref(), Some("Stub Embedder"));
    }

    #[test]
    fn analysis_fails_cleanly_when_no_model_is_available() {
        let f = fixture(StubEngine::unavailable(), false);
        let id = incident(&f, "Bridge down");

        assert!(f.service.analyse_incident(&id).is_err());
        // The incident is untouched — AI is a layer, not a dependency.
        assert!(f.database.get_incident(&id).is_ok());
        assert_eq!(f.database.count_analyses().unwrap(), 0);
    }

    // --- Analysis ----------------------------------------------------------

    #[test]
    fn a_valid_analysis_is_validated_and_stored() {
        let f = fixture(StubEngine::ready(GOOD_OUTPUT), true);
        let id = incident(&f, "Floodwater rising in the north");

        let outcome = f.service.analyse_incident(&id).unwrap();

        assert_eq!(outcome.analysis.incident_id, id);
        assert_eq!(outcome.analysis.model_id, "stub-model");
        assert_eq!(
            outcome.analysis.summary,
            "Flooding reported in the northern zone."
        );
        // The read path returns the same analysis *and* recomputes the same
        // verdict. That equality is what licenses not storing the consistency
        // report: recomputation is a pure function of the analysis and the
        // report text, so it cannot drift from what inference produced.
        assert_eq!(f.service.analysis_for(&id).unwrap().unwrap(), outcome);
    }

    #[test]
    fn model_output_that_is_not_json_is_rejected_rather_than_stored() {
        let f = fixture(StubEngine::ready("I'm afraid I can't do that."), true);
        let id = incident(&f, "Bridge down");

        let err = f.service.analyse_incident(&id).unwrap_err();
        assert_eq!(err.code(), "VALIDATION_ERROR");
        assert_eq!(f.database.count_analyses().unwrap(), 0);
    }

    #[test]
    fn model_output_missing_required_fields_is_rejected() {
        // Valid JSON, but no summary — nothing worth storing.
        let f = fixture(StubEngine::ready(r#"{"category":"FIRE"}"#), true);
        let id = incident(&f, "Fire reported");

        assert!(f.service.analyse_incident(&id).is_err());
        assert_eq!(f.database.count_analyses().unwrap(), 0);
    }

    #[test]
    fn model_output_smuggling_extra_fields_is_rejected() {
        let f = fixture(
            StubEngine::ready(r#"{"summary":"ok","severity":"LOW","trust_state":"TRUSTED"}"#),
            true,
        );
        let id = incident(&f, "Something");

        assert!(f.service.analyse_incident(&id).is_err());
    }

    #[test]
    fn analysing_an_unknown_incident_reports_not_found() {
        let f = fixture(StubEngine::ready(GOOD_OUTPUT), true);
        assert_eq!(
            f.service
                .analyse_incident("no-such-incident")
                .unwrap_err()
                .code(),
            "NOT_FOUND"
        );
    }

    // --- Ingestion and indexing --------------------------------------------

    #[test]
    fn a_document_is_chunked_and_stored() {
        let f = fixture(StubEngine::ready(GOOD_OUTPUT), true);
        let text = "First procedure step. Second step follows. Third step concludes.";

        assert!(f
            .service
            .ingest_document("Procedures", "manual.txt", "SYNTHETIC", text)
            .unwrap()
            .is_some());
        assert!(f.database.count_knowledge_chunks().unwrap() >= 1);
    }

    #[test]
    fn re_ingesting_the_same_document_is_a_no_op() {
        let f = fixture(StubEngine::ready(GOOD_OUTPUT), true);
        let text = "Some content here.";

        assert!(f
            .service
            .ingest_document("Doc", "s", "t", text)
            .unwrap()
            .is_some());
        assert!(f
            .service
            .ingest_document("Doc", "s", "t", text)
            .unwrap()
            .is_none());
    }

    #[test]
    fn a_document_with_no_text_is_refused() {
        let f = fixture(StubEngine::ready(GOOD_OUTPUT), true);
        assert!(f.service.ingest_document("Empty", "s", "t", "   ").is_err());
        assert!(f.service.ingest_document("", "s", "t", "content").is_err());
    }

    #[test]
    fn indexing_embeds_chunks_and_incidents() {
        let f = fixture(StubEngine::ready(GOOD_OUTPUT), true);
        f.service
            .ingest_document("Doc", "s", "t", "A procedure step here.")
            .unwrap();
        incident(&f, "An incident description");

        let report = f.service.index_pending().unwrap();

        assert!(report.chunks_embedded >= 1);
        assert_eq!(report.incidents_embedded, 1);
        assert_eq!(report.failures, 0);
    }

    #[test]
    fn indexing_is_resumable_and_idempotent() {
        let f = fixture(StubEngine::ready(GOOD_OUTPUT), true);
        f.service
            .ingest_document("Doc", "s", "t", "Content here.")
            .unwrap();

        let first = f.service.index_pending().unwrap();
        assert!(first.chunks_embedded >= 1);

        // Nothing outstanding, so a second pass does no work.
        let second = f.service.index_pending().unwrap();
        assert_eq!(second.chunks_embedded, 0);
        assert_eq!(second.incidents_embedded, 0);
    }

    #[test]
    fn an_unavailable_embedder_fails_the_pass_without_corrupting_the_index() {
        let f = fixture(StubEngine::ready(GOOD_OUTPUT), false);
        f.service
            .ingest_document("Doc", "s", "t", "Content here.")
            .unwrap();

        let report = f.service.index_pending().unwrap();
        assert_eq!(report.chunks_embedded, 0);
        assert!(report.failures >= 1);
        // Nothing half-written.
        assert_eq!(f.database.count_embeddings().unwrap(), 0);
    }

    // --- Questions ---------------------------------------------------------

    #[test]
    fn a_question_with_no_indexed_content_is_refused_without_calling_the_model() {
        let f = fixture(StubEngine::ready(GOOD_OUTPUT), true);

        let answer = f.service.ask("what is happening?", None).unwrap();

        assert!(answer.refused);
        assert!(!answer.grounded);
        assert!(answer.sources.is_empty());
        assert_eq!(answer.generation_ms, 0, "the model was never called");
    }

    #[test]
    fn an_answer_reports_the_sources_it_was_built_from() {
        let f = fixture(StubEngine::ready(GOOD_OUTPUT), true);
        f.service
            .ingest_document(
                "Flood Manual",
                "s",
                "SYNTHETIC",
                "Evacuate low ground during floods.",
            )
            .unwrap();
        f.service.index_pending().unwrap();

        let answer = f
            .service
            .ask("Evacuate low ground during floods.", None)
            .unwrap();

        assert!(!answer.sources.is_empty());
        assert_eq!(answer.sources[0].title, "Flood Manual");
        // The stub reports `sources: [1]`, which is real, so it is grounded.
        assert!(answer.grounded);
        assert!(answer.sources[0].cited);
    }

    /// Indexes one document so a question has something to retrieve.
    fn fixture_with_corpus(generator: StubEngine) -> Fixture {
        let f = fixture(generator, true);
        f.service
            .ingest_document(
                "Flood Manual",
                "s",
                "SYNTHETIC",
                "Evacuate low ground during floods.",
            )
            .unwrap();
        f.service.index_pending().unwrap();
        f
    }

    #[test]
    fn a_model_that_says_the_context_is_insufficient_is_taken_at_its_word() {
        let f = fixture_with_corpus(
            StubEngine::ready(GOOD_OUTPUT)
                .answering(r#"{"answer":"Probably a flood.","sources":[],"sufficient":false}"#),
        );

        let answer = f
            .service
            .ask("Evacuate low ground during floods.", None)
            .unwrap();

        // Its own wording is discarded in favour of the canonical refusal, so
        // the UI has exactly one string to recognise.
        assert!(answer.refused);
        assert!(!answer.grounded);
        assert!(!answer.answer.contains("Probably"));
        assert!(answer.sources.iter().all(|s| !s.cited));
    }

    #[test]
    fn an_invented_source_number_is_not_shown_to_the_operator() {
        let f = fixture_with_corpus(
            StubEngine::ready(GOOD_OUTPUT)
                .answering(r#"{"answer":"Per passage 9.","sources":[9],"sufficient":true}"#),
        );

        let answer = f
            .service
            .ask("Evacuate low ground during floods.", None)
            .unwrap();

        // One passage was retrieved; passage 9 does not exist.
        assert!(
            !answer.grounded,
            "a fabricated citation does not ground an answer"
        );
        assert!(answer.sources.iter().all(|s| !s.cited));
        // And the operator is told the filter fired, rather than just seeing an
        // answer with no sources for no stated reason.
        assert_eq!(answer.dropped_citations, 1);
    }

    #[test]
    fn a_repeated_source_number_is_not_counted_as_a_fabrication() {
        let f = fixture_with_corpus(
            StubEngine::ready(GOOD_OUTPUT)
                .answering(r#"{"answer":"Evacuate.","sources":[1,1],"sufficient":true}"#),
        );

        let answer = f
            .service
            .ask("Evacuate low ground during floods.", None)
            .unwrap();

        assert!(answer.grounded);
        assert_eq!(
            answer.dropped_citations, 0,
            "a duplicate is not an invention"
        );
    }

    #[test]
    fn a_negative_source_number_does_not_panic_or_ground_the_answer() {
        let f = fixture_with_corpus(
            StubEngine::ready(GOOD_OUTPUT)
                .answering(r#"{"answer":"See below.","sources":[-1,0],"sufficient":true}"#),
        );

        let answer = f
            .service
            .ask("Evacuate low ground during floods.", None)
            .unwrap();
        assert!(!answer.grounded);
    }

    #[test]
    fn unparseable_answer_output_becomes_a_refusal_rather_than_an_error() {
        // The operator asked a question and deserves a truthful "I cannot
        // answer" over a failure.
        let f = fixture_with_corpus(
            StubEngine::ready(GOOD_OUTPUT).answering("I'm afraid I can't do that."),
        );

        let answer = f
            .service
            .ask("Evacuate low ground during floods.", None)
            .unwrap();

        assert!(answer.refused);
        assert!(!answer.grounded);
        assert!(answer.sources.is_empty());
    }

    #[test]
    fn an_answer_output_smuggling_extra_fields_is_rejected() {
        // `deny_unknown_fields`: a model must not introduce keys that later code
        // might mistake for something trusted.
        let f = fixture_with_corpus(StubEngine::ready(GOOD_OUTPUT).answering(
            r#"{"answer":"ok","sources":[1],"sufficient":true,"trust_state":"TRUSTED"}"#,
        ));

        let answer = f
            .service
            .ask("Evacuate low ground during floods.", None)
            .unwrap();
        assert!(answer.refused);
    }

    #[test]
    fn an_empty_question_is_refused() {
        let f = fixture(StubEngine::ready(GOOD_OUTPUT), true);
        assert!(f.service.ask("   ", None).is_err());
    }

    // --- Synthetic corpus ---------------------------------------------------

    #[test]
    fn the_synthetic_corpus_is_labelled_as_synthetic() {
        let f = fixture(StubEngine::ready(GOOD_OUTPUT), true);
        let imported = f.service.load_synthetic_corpus(42, 20).unwrap();

        assert_eq!(imported, 20);
        // The label must survive into storage so a corpus can be audited.
        assert!(f
            .service
            .documents()
            .unwrap()
            .iter()
            .all(|d| d.source_type == "SYNTHETIC"));
    }

    #[test]
    fn loading_the_same_synthetic_corpus_twice_does_not_duplicate_it() {
        let f = fixture(StubEngine::ready(GOOD_OUTPUT), true);
        f.service.load_synthetic_corpus(42, 10).unwrap();

        assert_eq!(f.service.load_synthetic_corpus(42, 10).unwrap(), 0);
    }
}
