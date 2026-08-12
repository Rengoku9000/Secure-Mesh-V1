//! Runs the Phase 3 evaluation against the real local model.
//!
//! ```text
//! cargo run --release --example run_benchmark -- [seed] [corpus] [extraction] [questions]
//! ```
//!
//! An example target, so it is not part of the shipped application. It needs a
//! provisioned model; without one it says so and exits rather than reporting
//! zeros as though they were results.
//!
//! Results are written to `docs/ai/benchmark-latest.json` alongside the summary
//! printed here, so a figure quoted in documentation can be traced to a run.

use securemesh_lib::ai::evaluation::{run_benchmark, BenchmarkConfig};
use securemesh_lib::ai::{IntelligenceService, LlamaConfig, LlamaServerEngine};
use securemesh_lib::storage::Database;
use std::path::Path;
use std::sync::Arc;

fn main() {
    let mut args = std::env::args().skip(1);
    let seed: u64 = args.next().and_then(|a| a.parse().ok()).unwrap_or(42);
    let corpus_size: usize = args.next().and_then(|a| a.parse().ok()).unwrap_or(500);
    let extraction_sample: usize = args.next().and_then(|a| a.parse().ok()).unwrap_or(30);
    let question_sample: usize = args.next().and_then(|a| a.parse().ok()).unwrap_or(20);

    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("the project root is the manifest's parent")
        .to_path_buf();

    let generation = LlamaConfig::generation(&root, 19_100);
    let embedding = LlamaConfig::embedding(&root, 19_101);

    // Refuse to produce numbers without a model, rather than reporting zeros
    // that could be mistaken for measurements.
    for config in [&generation, &embedding] {
        if let Err(reason) = config.availability() {
            eprintln!("cannot benchmark: {}", reason.detail());
            std::process::exit(1);
        }
    }

    // A scratch database, so a benchmark never touches a real node's records.
    let workspace = std::env::temp_dir().join(format!("securemesh-bench-{seed}"));
    let _ = std::fs::remove_dir_all(&workspace);
    std::fs::create_dir_all(&workspace).expect("could not create the benchmark workspace");

    let database =
        Arc::new(Database::open(workspace.join("bench.sqlite")).expect("could not open database"));

    let generator = Arc::new(LlamaServerEngine::new(generation));
    let embedder = Arc::new(LlamaServerEngine::new(embedding));
    let service = IntelligenceService::new(
        Arc::clone(&database),
        Arc::clone(&generator) as Arc<_>,
        Arc::clone(&embedder) as Arc<_>,
    );

    let hardware = "AMD Ryzen AI 7 350 (8C/16T), 23 GB RAM, CPU-only llama.cpp b10375";
    println!("Running benchmark — this loads models and may take several minutes.\n");

    let report = run_benchmark(
        &service,
        generator.as_ref(),
        embedder.as_ref(),
        &BenchmarkConfig {
            hardware: hardware.to_string(),
            seed,
            corpus_size,
            extraction_sample,
            question_sample,
        },
    )
    .expect("benchmark failed");

    println!("=== SecureMesh Phase 3 benchmark ===");
    println!("model            : {} ({})", report.model_id, report.quantisation);
    println!("backend          : {}", report.backend);
    println!("embedding model  : {}", report.embedding_model);
    println!("hardware         : {}", report.hardware);
    println!("prompt version   : {}", report.prompt_version);
    println!("dataset          : {} seed {}", report.dataset_version, report.dataset_seed);
    println!("corpus size      : {}", report.corpus_size);

    let extraction = &report.extraction;
    println!("\n--- extraction ---");
    println!("attempted        : {}", extraction.attempted);
    println!("valid output     : {} ({:.1}%)", extraction.valid, extraction.valid_rate() * 100.0);
    println!("category accuracy: {:.1}%", extraction.category_accuracy() * 100.0);
    println!("severity accuracy: {:.1}%", extraction.severity_accuracy() * 100.0);
    println!("keyword recall   : {:.1}%", extraction.keyword_recall * 100.0);
    println!(
        "latency ms       : mean {} median {} min {} max {}",
        extraction.latency.mean_ms,
        extraction.latency.median_ms,
        extraction.latency.min_ms,
        extraction.latency.max_ms
    );

    let retrieval = &report.retrieval;
    println!("\n--- retrieval and grounding ---");
    println!("answerable asked : {}", retrieval.answerable_asked);
    println!("retrieved context: {}", retrieval.retrieved_something);
    println!("grounded answers : {} ({:.1}%)", retrieval.grounded, retrieval.grounding_rate() * 100.0);
    println!("unanswerable     : {}", retrieval.unanswerable_asked);
    println!(
        "correctly refused: {} ({:.1}%)",
        retrieval.correctly_refused,
        retrieval.refusal_accuracy() * 100.0
    );
    println!("invented citations: {}", retrieval.invented_citations);
    for question in &retrieval.answered_unanswerable {
        println!("  ANSWERED ANYWAY: {question}");
    }
    println!(
        "retrieval ms     : mean {} median {}",
        retrieval.retrieval_latency.mean_ms, retrieval.retrieval_latency.median_ms
    );
    println!(
        "answer ms        : mean {} median {}",
        retrieval.answer_latency.mean_ms, retrieval.answer_latency.median_ms
    );

    println!("\n--- embedding ---");
    println!(
        "latency ms       : mean {} median {} min {} max {}",
        report.embedding_latency.mean_ms,
        report.embedding_latency.median_ms,
        report.embedding_latency.min_ms,
        report.embedding_latency.max_ms
    );

    for note in &report.notes {
        println!("\nnote: {note}");
    }

    let output = root.join("docs/ai/benchmark-latest.json");
    if let Some(parent) = output.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match serde_json::to_string_pretty(&report) {
        Ok(json) => {
            if std::fs::write(&output, json).is_ok() {
                println!("\nwritten to {}", output.display());
            }
        }
        Err(error) => eprintln!("could not serialise the report: {error}"),
    }

    // Free the model processes promptly rather than at exit.
    service.unload();
    let _ = std::fs::remove_dir_all(&workspace);
}
