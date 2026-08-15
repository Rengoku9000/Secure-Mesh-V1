//! Diagnoses the local intelligence execution path.
//!
//! Answers one question: when "Ask SecureMesh" refuses with `generation 0 ms`,
//! is the model broken, or was it never called because retrieval found nothing?
//!
//! Read-only with respect to the node it inspects — it opens the real database
//! to report what is indexed, and does its RAG experiment in a scratch copy so
//! nothing is written to the operator's records.
//!
//! ```text
//! cargo run --example ai_diagnostic -- [path-to-node-data-dir]
//! ```

use securemesh_lib::ai::embedding::EmbeddingEngine;
use securemesh_lib::ai::engine::{GenerationRequest, LocalInferenceEngine, StructuredRequest};
use securemesh_lib::ai::{prompt, IntelligenceService, LlamaConfig, LlamaServerEngine};
use securemesh_lib::domain::RawAnalysis;
use securemesh_lib::storage::Database;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

const AVALANCHE_QUESTION: &str = "what to do in case of avalanche";

fn rule(title: &str) {
    println!("\n{}", "=".repeat(70));
    println!("{title}");
    println!("{}", "=".repeat(70));
}

fn main() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("the project root is the manifest's parent")
        .to_path_buf();

    let node_dir: PathBuf = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            dirs_next_roaming()
                .map(|d| d.join("org.securemesh.node"))
                .unwrap_or_else(|| root.join("node-data"))
        });

    println!("project root : {}", root.display());
    println!("node data dir: {}", node_dir.display());

    // ---------------------------------------------------------------- Test 7
    rule("TEST 7 — RAG INDEX HEALTH (the operator's real node)");
    let node_db_path = node_dir.join("securemesh.sqlite");
    if node_db_path.exists() {
        match Database::open(&node_db_path) {
            Ok(db) => report_index(&db),
            Err(error) => println!("could not open the node database: {}", error.message()),
        }
    } else {
        println!(
            "no database at {} — node never ran here",
            node_db_path.display()
        );
    }

    // ---------------------------------------------------------------- Test 6
    rule("TEST 6 — MODEL HEALTH: what does 'Ready' actually mean?");
    let generation = LlamaConfig::generation(&root, 19_200);
    let embedding = LlamaConfig::embedding(&root, 19_201);

    for (label, config) in [("generation", &generation), ("embedding", &embedding)] {
        match config.availability() {
            Ok(()) => println!("{label:10} files present: runtime + model both on disk"),
            Err(reason) => {
                println!("{label:10} UNAVAILABLE: {}", reason.detail());
                println!("\ncannot continue without a provisioned model.");
                return;
            }
        }
    }

    let generator = Arc::new(LlamaServerEngine::new(generation));
    let embedder = Arc::new(LlamaServerEngine::new(embedding));

    println!("\nhealth BEFORE any inference (this is what the UI badge reads):");
    println!(
        "  generator: {:?}",
        LocalInferenceEngine::health(generator.as_ref())
    );
    println!(
        "  embedder : {:?}",
        EmbeddingEngine::health(embedder.as_ref())
    );

    // ---------------------------------------------------------------- Test 1
    rule("TEST 1 — DIRECT GENERATION (RAG bypassed entirely)");
    let started = Instant::now();
    let direct = generator.generate(&GenerationRequest {
        system: "You are a concise assistant.".to_string(),
        user: "Explain in two sentences what an avalanche is.".to_string(),
        max_tokens: 160,
        temperature: 0.0,
    });
    let elapsed = started.elapsed().as_millis();

    match &direct {
        Ok(text) => {
            println!("generation latency : {elapsed} ms  (includes first-call model load)");
            println!("output chars       : {}", text.len());
            println!("output words       : {}", text.split_whitespace().count());
            println!(
                "\n--- model output ---\n{}\n--------------------",
                text.trim()
            );
        }
        Err(error) => println!("FAILED after {elapsed} ms: {}", error.message()),
    }

    // A second call with the model already resident separates load from decode.
    if direct.is_ok() {
        let started = Instant::now();
        let second = generator.generate(&GenerationRequest {
            system: "You are a concise assistant.".to_string(),
            user: "Name three items to carry in an avalanche kit.".to_string(),
            max_tokens: 120,
            temperature: 0.0,
        });
        let warm = started.elapsed().as_millis();
        match second {
            Ok(text) => {
                let words = text.split_whitespace().count();
                println!("\nwarm generation    : {warm} ms for {words} words");
                if warm > 0 {
                    println!(
                        "approx             : {:.1} words/sec",
                        words as f64 / (warm as f64 / 1000.0)
                    );
                }
            }
            Err(error) => println!("\nwarm call FAILED: {}", error.message()),
        }
    }

    // ---------------------------------------------------------------- Test 2
    rule("TEST 2 — STRUCTURED GENERATION (IncidentAnalysis schema)");
    let started = Instant::now();
    let structured = generator.generate_structured(&StructuredRequest {
        system: prompt::analysis_system_prompt(),
        user: prompt::analysis_user_message(
            "Heavy snowfall triggered an avalanche near a mountain road. \
             Several vehicles are trapped.",
        ),
        schema: prompt::analysis_schema(),
        max_tokens: 512,
    });
    let elapsed = started.elapsed().as_millis();

    match structured {
        Ok(raw) => {
            println!("generation latency : {elapsed} ms");
            println!("raw output         : {}", raw.trim());
            match serde_json::from_str::<RawAnalysis>(&raw) {
                Ok(parsed) => match parsed.validate("diagnostic", "qwen", elapsed as u64) {
                    Ok(analysis) => {
                        println!("\nparsed + validated OK");
                        println!("  category : {}", analysis.category);
                        println!("  severity : {}", analysis.severity);
                        println!("  summary  : {}", analysis.summary);
                    }
                    Err(error) => println!("\nvalidation REJECTED it: {}", error.message()),
                },
                Err(error) => println!("\nJSON parse failed: {error}"),
            }
        }
        Err(error) => println!("FAILED after {elapsed} ms: {}", error.message()),
    }

    // ------------------------------------------------------------ Tests 3 & 4
    rule("TESTS 3 & 4 — RETRIEVAL, in a scratch database");
    let workspace = std::env::temp_dir().join("securemesh-ai-diagnostic");
    let _ = std::fs::remove_dir_all(&workspace);
    std::fs::create_dir_all(&workspace).expect("scratch workspace");

    let database = Arc::new(
        Database::open(workspace.join("diag.sqlite")).expect("scratch database should open"),
    );
    let service = IntelligenceService::new(
        Arc::clone(&database),
        Arc::clone(&generator) as Arc<_>,
        Arc::clone(&embedder) as Arc<_>,
    );

    // --- Test 4 first, against an EMPTY index: the reported symptom ---
    println!("\n--- TEST 4a: avalanche question, EMPTY index (mirrors the report) ---");
    ask_and_report(&service, &database, &embedder, AVALANCHE_QUESTION);

    // --- Now ingest avalanche knowledge and repeat ---
    println!("\n--- ingesting one avalanche document ---");
    let ingested = service.ingest_document(
        "Avalanche Response Procedure",
        "diagnostic",
        "SYNTHETIC",
        "If an avalanche is reported, move all personnel to higher ground away from the \
         slope run-out zone. Do not attempt to cross the debris field on foot. Account for \
         every person and record who is missing. Probe lines and transceiver searches are \
         carried out only by trained rescue teams. Keep the access road closed until a \
         qualified observer confirms the slope is stable.",
    );
    match ingested {
        Ok(Some(id)) => println!("ingested document id={id}"),
        Ok(None) => println!("already present (content hash matched) — nothing ingested"),
        Err(error) => println!("ingest FAILED: {}", error.message()),
    }
    match service.index_pending() {
        Ok(report) => println!(
            "indexed: {} chunks, {} incidents, {} failures",
            report.chunks_embedded, report.incidents_embedded, report.failures
        ),
        Err(error) => println!("indexing FAILED: {}", error.message()),
    }
    println!("index now: {:?}", index_counts(&database));

    println!("\n--- TEST 3: question whose answer IS in the corpus ---");
    ask_and_report(
        &service,
        &database,
        &embedder,
        "What should personnel do if an avalanche is reported?",
    );

    println!("\n--- TEST 4b: the SAME avalanche question, index now populated ---");
    ask_and_report(&service, &database, &embedder, AVALANCHE_QUESTION);

    service.unload();
    let _ = std::fs::remove_dir_all(&workspace);
    println!("\ndiagnostic complete.");
}

/// Prints the index sizes for a database.
fn index_counts(database: &Database) -> (u64, u64, u64) {
    (
        database.count_analyses().unwrap_or(0),
        database.count_knowledge_chunks().unwrap_or(0),
        database.count_embeddings().unwrap_or(0),
    )
}

fn report_index(database: &Database) {
    let (analyses, chunks, vectors) = index_counts(database);
    println!("analyses stored   : {analyses}");
    println!("knowledge chunks  : {chunks}");
    println!("vectors stored    : {vectors}");
    println!(
        "incidents         : {}",
        database.count_incidents().unwrap_or(0)
    );
    if vectors == 0 {
        println!("\n  >> vectors = 0: retrieval can only ever return nothing here.");
    }
}

/// Runs one question and reports every decision point in the pipeline.
fn ask_and_report(
    service: &IntelligenceService,
    database: &Database,
    embedder: &Arc<LlamaServerEngine>,
    question: &str,
) {
    println!("question: {question:?}");

    // Raw similarity scores, threshold removed, so the actual numbers are
    // visible rather than inferred from whether anything was returned.
    match embedder.embed(question) {
        Ok(query) => match database.search_embeddings(&query, 5, 0.0) {
            Ok(all) => {
                println!("  candidates (threshold 0.0): {}", all.len());
                for passage in all.iter().take(5) {
                    println!(
                        "    score {:.4}  {:?}  {}",
                        passage.score, passage.kind, passage.source_title
                    );
                }
                let above = all
                    .iter()
                    .filter(|p| p.score >= securemesh_lib::ai::rag::MIN_RELEVANCE)
                    .count();
                println!(
                    "  above MIN_RELEVANCE ({}): {}",
                    securemesh_lib::ai::rag::MIN_RELEVANCE,
                    above
                );
            }
            Err(error) => println!("  search failed: {}", error.message()),
        },
        Err(error) => println!("  embedding failed: {}", error.message()),
    }

    match service.ask(question, Some(5)) {
        Ok(answer) => {
            println!(
                "  -> retrieval {} ms | generation {} ms | grounded {} | refused {}",
                answer.retrieval_ms, answer.generation_ms, answer.grounded, answer.refused
            );
            println!("  -> sources: {}", answer.sources.len());
            println!("  -> answer: {}", answer.answer.trim());
            println!(
                "  -> GENERATION WAS {}",
                if answer.generation_ms > 0 {
                    "INVOKED"
                } else {
                    "SKIPPED (model never called)"
                }
            );
        }
        Err(error) => println!("  ask failed: {}", error.message()),
    }
}

/// Roaming app-data directory, without adding a dependency for one lookup.
fn dirs_next_roaming() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(PathBuf::from)
}
