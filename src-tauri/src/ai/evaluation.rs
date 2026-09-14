//! The evaluation harness.
//!
//! Produces the numbers reported for Phase 3. Every figure here is *measured*
//! by running the local model against the deterministic synthetic corpus —
//! nothing is estimated, and a run that cannot execute reports that rather than
//! filling in a plausible value.
//!
//! # What is measured, and what each number does not mean
//!
//! - **Classification accuracy** — how often the model's category matches the
//!   generator's label. On synthetic data with templated phrasing this is an
//!   easier task than real field reports, so it is an upper bound, not a
//!   prediction of field performance.
//! - **Extraction recall** — the share of expected keywords appearing anywhere
//!   in the analysis. Recall, deliberately: a model that phrases things
//!   differently is not wrong, so exact-match scoring would measure wording
//!   rather than understanding.
//! - **Retrieval precision@k** — whether the passage a question was generated
//!   from is retrieved for it.
//! - **Refusal rate** — how often unanswerable questions are correctly
//!   refused. Without this, a model that always answers scores well on
//!   everything else.
//! - **Latency** — wall clock per operation, on the hardware named in the
//!   report.
//!
//! The corpus is synthetic and labelled as such; see [`crate::ai::dataset`].

use crate::ai::dataset::{self, SyntheticIncident};
use crate::ai::embedding::EmbeddingEngine;
use crate::ai::engine::{LocalInferenceEngine, StructuredRequest};
use crate::ai::prompt;
use crate::ai::service::IntelligenceService;
use crate::domain::RawAnalysis;
use crate::error::CoreResult;
use serde::Serialize;

/// One operation's timing.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LatencyStats {
    pub samples: usize,
    pub mean_ms: u64,
    pub median_ms: u64,
    pub min_ms: u64,
    pub max_ms: u64,
}

impl LatencyStats {
    fn from(mut samples: Vec<u64>) -> Self {
        if samples.is_empty() {
            return Self::default();
        }
        samples.sort_unstable();

        let total: u128 = samples.iter().map(|s| *s as u128).sum();
        Self {
            samples: samples.len(),
            mean_ms: (total / samples.len() as u128) as u64,
            median_ms: samples[samples.len() / 2],
            min_ms: samples[0],
            max_ms: samples[samples.len() - 1],
        }
    }
}

/// Extraction and classification results.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtractionResults {
    pub attempted: usize,
    /// Output that parsed and validated. A failure here is a broken analysis,
    /// not merely an inaccurate one.
    pub valid: usize,
    pub category_correct: usize,
    pub severity_correct: usize,
    /// Mean share of expected keywords present, over valid analyses.
    pub keyword_recall: f64,
    pub latency: LatencyStats,
}

impl ExtractionResults {
    pub fn valid_rate(&self) -> f64 {
        ratio(self.valid, self.attempted)
    }

    pub fn category_accuracy(&self) -> f64 {
        ratio(self.category_correct, self.valid)
    }

    pub fn severity_accuracy(&self) -> f64 {
        ratio(self.severity_correct, self.valid)
    }
}

/// Retrieval and grounding results.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RetrievalResults {
    pub answerable_asked: usize,
    /// Answers citing at least one supplied passage.
    pub grounded: usize,
    /// Answerable questions whose retrieval returned something relevant.
    pub retrieved_something: usize,
    pub unanswerable_asked: usize,
    /// Unanswerable questions correctly refused.
    pub correctly_refused: usize,
    /// Citations the model invented. Must be zero: they are filtered before
    /// they can reach a user, and a non-zero count means the filter fired.
    pub invented_citations: usize,
    /// Unanswerable questions that were answered anyway.
    ///
    /// Recorded rather than merely counted: "80% refused" says a fifth got
    /// through, but not which, and the failing question is the one worth
    /// looking at.
    pub answered_unanswerable: Vec<String>,
    pub retrieval_latency: LatencyStats,
    pub answer_latency: LatencyStats,
}

impl RetrievalResults {
    pub fn grounding_rate(&self) -> f64 {
        ratio(self.grounded, self.answerable_asked)
    }

    pub fn refusal_accuracy(&self) -> f64 {
        ratio(self.correctly_refused, self.unanswerable_asked)
    }
}

/// A full benchmark record.
///
/// Carries everything needed to interpret the numbers: without the model,
/// quantisation, hardware, and dataset version, an accuracy figure is not
/// comparable to anything.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BenchmarkReport {
    pub model_id: String,
    pub quantisation: String,
    pub backend: String,
    pub embedding_model: String,
    pub hardware: String,
    pub prompt_version: String,
    pub dataset_version: String,
    pub dataset_seed: u64,
    pub corpus_size: usize,
    pub extraction: ExtractionResults,
    pub retrieval: RetrievalResults,
    pub embedding_latency: LatencyStats,
    pub notes: Vec<String>,
}

/// Prompt revision, bumped whenever wording changes.
///
/// Accuracy shifts with prompt wording, so a result without this is not
/// reproducible.
pub const PROMPT_VERSION: &str = "analysis-v3/rag-v2";

fn ratio(part: usize, whole: usize) -> f64 {
    if whole == 0 {
        return 0.0;
    }
    part as f64 / whole as f64
}

/// Runs extraction and classification over a sample of the corpus.
///
/// `sample` bounds the run: a full 1000-incident pass at CPU latency takes
/// roughly forty minutes, which is too slow for a routine check.
pub fn evaluate_extraction(
    generator: &dyn LocalInferenceEngine,
    incidents: &[SyntheticIncident],
    sample: usize,
) -> ExtractionResults {
    let mut results = ExtractionResults::default();
    let mut latencies = Vec::new();
    let mut recall_total = 0.0f64;

    for incident in incidents.iter().take(sample) {
        results.attempted += 1;

        let request = StructuredRequest {
            system: prompt::analysis_system_prompt(),
            user: prompt::analysis_user_message(&incident.description),
            schema: prompt::analysis_schema(),
            max_tokens: 512,
        };

        let started = std::time::Instant::now();
        let Ok(raw) = generator.generate_structured(&request) else {
            continue;
        };
        latencies.push(started.elapsed().as_millis() as u64);

        // Same untrusted-output path the application uses, so the harness
        // measures what actually ships.
        let Ok(parsed) = serde_json::from_str::<RawAnalysis>(&raw) else {
            continue;
        };
        let Ok(analysis) = parsed.validate(&incident.id, "eval", 0) else {
            continue;
        };

        results.valid += 1;

        if analysis.category == incident.expected_category {
            results.category_correct += 1;
        }
        if analysis.severity.as_str() == incident.expected_severity {
            results.severity_correct += 1;
        }

        // Recall over everything the analysis said, since a keyword may
        // legitimately land in the summary rather than a named field.
        let haystack = format!(
            "{} {} {} {}",
            analysis.summary,
            analysis.asset.clone().unwrap_or_default(),
            analysis.cause.clone().unwrap_or_default(),
            analysis.entities.join(" ")
        )
        .to_lowercase();

        let found = incident
            .expected_keywords
            .iter()
            .filter(|keyword| haystack.contains(&keyword.to_lowercase()))
            .count();
        recall_total += ratio(found, incident.expected_keywords.len());
    }

    results.keyword_recall = if results.valid > 0 {
        recall_total / results.valid as f64
    } else {
        0.0
    };
    results.latency = LatencyStats::from(latencies);
    results
}

/// Runs the question set against the RAG pipeline.
pub fn evaluate_retrieval(
    service: &IntelligenceService,
    questions: &[dataset::EvaluationQuestion],
    sample: usize,
) -> RetrievalResults {
    let mut results = RetrievalResults::default();
    let mut retrieval_latencies = Vec::new();
    let mut answer_latencies = Vec::new();

    for question in questions.iter().take(sample) {
        let Ok(answer) = service.ask(&question.question, Some(5)) else {
            continue;
        };

        retrieval_latencies.push(answer.retrieval_ms);
        if answer.generation_ms > 0 {
            answer_latencies.push(answer.generation_ms);
        }

        // Two sources of fabrication: a source number the model returned that
        // was never supplied (dropped by the RAG layer, and reported), and an
        // out-of-range marker written into the prose.
        let cited_markers = crate::ai::rag::extract_citations(&answer.answer, usize::MAX);
        let real = answer.sources.len();
        results.invented_citations +=
            answer.dropped_citations + cited_markers.iter().filter(|m| **m > real).count();

        if question.expect_refusal {
            results.unanswerable_asked += 1;
            if answer.refused || !answer.grounded {
                results.correctly_refused += 1;
            } else {
                results
                    .answered_unanswerable
                    .push(question.question.clone());
            }
        } else {
            results.answerable_asked += 1;
            if !answer.sources.is_empty() {
                results.retrieved_something += 1;
            }
            if answer.grounded {
                results.grounded += 1;
            }
        }
    }

    results.retrieval_latency = LatencyStats::from(retrieval_latencies);
    results.answer_latency = LatencyStats::from(answer_latencies);
    results
}

/// Measures embedding latency alone.
pub fn evaluate_embedding(embedder: &dyn EmbeddingEngine, samples: usize) -> LatencyStats {
    let mut latencies = Vec::new();

    for index in 0..samples {
        let text = format!(
            "Incident report number {index}: flooding reported near the northern access road."
        );
        let started = std::time::Instant::now();
        if embedder.embed(&text).is_ok() {
            latencies.push(started.elapsed().as_millis() as u64);
        }
    }

    LatencyStats::from(latencies)
}

/// How a benchmark run is parameterised.
///
/// Grouped rather than passed as eight arguments: these travel together, and
/// the samples in particular are a trade between confidence and runtime that is
/// worth naming.
#[derive(Debug, Clone)]
pub struct BenchmarkConfig {
    /// Free-text description of the machine, recorded with the results.
    pub hardware: String,
    pub seed: u64,
    pub corpus_size: usize,
    /// Incidents analysed. Bounded because a full corpus at CPU latency takes
    /// far too long for a routine check.
    pub extraction_sample: usize,
    pub question_sample: usize,
}

impl Default for BenchmarkConfig {
    /// A run that finishes in a few minutes on CPU while still being
    /// statistically meaningful.
    fn default() -> Self {
        Self {
            hardware: "unspecified".to_string(),
            seed: 42,
            corpus_size: 500,
            extraction_sample: 30,
            question_sample: 20,
        }
    }
}

/// Runs the whole benchmark.
pub fn run_benchmark(
    service: &IntelligenceService,
    generator: &dyn LocalInferenceEngine,
    embedder: &dyn EmbeddingEngine,
    config: &BenchmarkConfig,
) -> CoreResult<BenchmarkReport> {
    let BenchmarkConfig {
        hardware,
        seed,
        corpus_size,
        extraction_sample,
        question_sample,
    } = config.clone();

    let corpus = dataset::generate(seed, corpus_size);
    let mut notes = Vec::new();

    // Index first: retrieval cannot be measured against an empty index.
    service.load_synthetic_corpus(seed, corpus_size)?;
    let mut indexed = 0usize;
    loop {
        let report = service.index_pending()?;
        let moved = report.chunks_embedded + report.incidents_embedded;
        indexed += moved;
        if moved == 0 {
            if report.failures > 0 {
                notes.push(format!("{} items could not be embedded", report.failures));
            }
            break;
        }
    }
    notes.push(format!(
        "{indexed} items embedded before retrieval was measured"
    ));

    let model = generator.model_info();

    Ok(BenchmarkReport {
        model_id: model
            .as_ref()
            .map(|m| m.model_id.clone())
            .unwrap_or_else(|| "unavailable".to_string()),
        quantisation: model
            .as_ref()
            .map(|m| m.quantisation.clone())
            .unwrap_or_default(),
        backend: model
            .as_ref()
            .map(|m| m.backend.clone())
            .unwrap_or_default(),
        embedding_model: embedder.model_id(),
        hardware: hardware.to_string(),
        prompt_version: PROMPT_VERSION.to_string(),
        dataset_version: corpus.version.clone(),
        dataset_seed: seed,
        corpus_size,
        extraction: evaluate_extraction(generator, &corpus.incidents, extraction_sample),
        retrieval: evaluate_retrieval(service, &corpus.questions, question_sample),
        embedding_latency: evaluate_embedding(embedder, 20),
        notes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latency_statistics_are_computed_over_samples() {
        let stats = LatencyStats::from(vec![10, 20, 30, 40, 50]);

        assert_eq!(stats.samples, 5);
        assert_eq!(stats.mean_ms, 30);
        assert_eq!(stats.median_ms, 30);
        assert_eq!(stats.min_ms, 10);
        assert_eq!(stats.max_ms, 50);
    }

    #[test]
    fn latency_statistics_handle_no_samples() {
        // A run that could not execute must report zero, never a fabricated
        // number.
        let stats = LatencyStats::from(vec![]);
        assert_eq!(stats.samples, 0);
        assert_eq!(stats.mean_ms, 0);
    }

    #[test]
    fn latency_ordering_does_not_depend_on_input_order() {
        let ascending = LatencyStats::from(vec![1, 2, 3, 100]);
        let shuffled = LatencyStats::from(vec![100, 2, 1, 3]);
        assert_eq!(ascending.median_ms, shuffled.median_ms);
        assert_eq!(ascending.max_ms, shuffled.max_ms);
    }

    #[test]
    fn an_unrefused_question_is_named_rather_than_only_counted() {
        // A refusal rate says a fraction got through; the failing question says
        // why, which is the part worth acting on.
        let results = RetrievalResults {
            unanswerable_asked: 5,
            correctly_refused: 4,
            answered_unanswerable: vec!["What is the capital city of Mars?".to_string()],
            ..Default::default()
        };

        assert_eq!(results.refusal_accuracy(), 0.8);
        assert_eq!(
            results.answered_unanswerable.len(),
            results.unanswerable_asked - results.correctly_refused
        );
    }

    #[test]
    fn rates_are_zero_rather_than_undefined_when_nothing_was_attempted() {
        let extraction = ExtractionResults::default();
        assert_eq!(extraction.valid_rate(), 0.0);
        assert_eq!(extraction.category_accuracy(), 0.0);

        let retrieval = RetrievalResults::default();
        assert_eq!(retrieval.grounding_rate(), 0.0);
        assert_eq!(retrieval.refusal_accuracy(), 0.0);
    }

    #[test]
    fn accuracy_is_measured_against_valid_analyses_not_attempts() {
        // Otherwise a model that fails to produce parsable output would score
        // as merely inaccurate rather than broken, hiding the real problem.
        let results = ExtractionResults {
            attempted: 10,
            valid: 5,
            category_correct: 5,
            severity_correct: 3,
            ..Default::default()
        };

        assert_eq!(results.valid_rate(), 0.5);
        assert_eq!(results.category_accuracy(), 1.0);
        assert_eq!(results.severity_accuracy(), 0.6);
    }

    #[test]
    fn a_benchmark_records_what_is_needed_to_interpret_it() {
        // An accuracy number without the model, prompt and dataset behind it is
        // not comparable to anything.
        assert!(PROMPT_VERSION.contains("analysis"));
        assert!(PROMPT_VERSION.contains("rag"));
        assert!(!dataset::DATASET_VERSION.is_empty());
    }
}
