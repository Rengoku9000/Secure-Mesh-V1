//! Inspects a **live** node's RAG index without disturbing it.
//!
//! The node keeps its database in WAL mode, so a consistent snapshot needs the
//! `-wal` and `-shm` files copied alongside the main file. Everything below runs
//! against that copy: the operator's running node is never written to, and the
//! control ingest in step 9 lands in the copy only.
//!
//! ```text
//! cargo run --example rag_live_diagnostic -- <node-data-dir> [question]
//! ```

use securemesh_lib::ai::embedding::EmbeddingEngine;
use securemesh_lib::ai::{IntelligenceService, LlamaConfig, LlamaServerEngine};
use securemesh_lib::storage::Database;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const DB: &str = "securemesh.sqlite";

fn rule(title: &str) {
    println!("\n{}", "=".repeat(70));
    println!("{title}");
    println!("{}", "=".repeat(70));
}

/// Copies the database and its WAL sidecars, so the snapshot is consistent.
fn snapshot(source_dir: &Path, into: &Path) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(into)?;
    for suffix in ["", "-wal", "-shm"] {
        let name = format!("{DB}{suffix}");
        let from = source_dir.join(&name);
        if from.exists() {
            std::fs::copy(&from, into.join(&name))?;
        }
    }
    Ok(into.join(DB))
}

fn main() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("project root")
        .to_path_buf();

    let mut args = std::env::args().skip(1);
    let node_dir = PathBuf::from(args.next().expect("usage: <node-data-dir> [question]"));
    let question = args
        .next()
        .unwrap_or_else(|| "what happened at mount abu".to_string());

    println!("live node dir : {}", node_dir.display());

    let scratch = std::env::temp_dir().join("securemesh-rag-live-diag");
    let _ = std::fs::remove_dir_all(&scratch);
    let copied = snapshot(&node_dir, &scratch).expect("snapshot the live database");
    println!("snapshot      : {}", copied.display());

    let database = Arc::new(Database::open(&copied).expect("open the snapshot"));

    // ------------------------------------------------------------- 1. Counts
    rule("1 — LIVE INDEX STATE (from the snapshot)");
    println!(
        "incidents        : {}",
        database.count_incidents().unwrap_or(0)
    );
    println!(
        "analyses         : {}",
        database.count_analyses().unwrap_or(0)
    );
    println!(
        "knowledge chunks : {}",
        database.count_knowledge_chunks().unwrap_or(0)
    );
    println!(
        "vectors          : {}",
        database.count_embeddings().unwrap_or(0)
    );
    println!(
        "documents        : {}",
        database.list_documents().map(|d| d.len()).unwrap_or(0)
    );

    println!("\nincidents present:");
    match database.list_incidents(None) {
        Ok(incidents) => {
            for incident in &incidents {
                println!(
                    "  id={} severity={} sync={:?}\n     description: {:?}",
                    incident.id,
                    incident.severity.as_str(),
                    incident.sync_status,
                    incident.description
                );
            }
            if incidents.is_empty() {
                println!("  (none)");
            }
        }
        Err(error) => println!("  failed: {}", error.message()),
    }

    // ------------------------------------------ 5/6. Indexing pipeline status
    rule("5 & 6 — INCIDENT -> INDEX PIPELINE, and what text gets embedded");
    let embedding_config = LlamaConfig::embedding(&root, 19_301);
    let generation_config = LlamaConfig::generation(&root, 19_300);
    for config in [&embedding_config, &generation_config] {
        if let Err(reason) = config.availability() {
            println!("model unavailable: {}", reason.detail());
            return;
        }
    }

    let embedder = Arc::new(LlamaServerEngine::new(embedding_config));
    let generator = Arc::new(LlamaServerEngine::new(generation_config));
    let model_id = embedder.model_id();
    println!("embedding model id: {model_id}");

    // Anything listed here has NO vector for this model — the precise test for
    // "did this incident enter the index?".
    match database.incidents_awaiting_embedding(&model_id, 100) {
        Ok(pending) => {
            println!("\nincidents AWAITING embedding: {}", pending.len());
            for (id, description) in &pending {
                println!("  id={id}");
                println!("     text that WOULD be embedded: {description:?}");
            }
            if pending.is_empty() {
                println!("  (none — every incident already has a vector)");
            }
        }
        Err(error) => println!("query failed: {}", error.message()),
    }

    // ------------------------------------------------------------ 8. Geometry
    rule("8 — EMBEDDING MODEL / DIMENSIONS");
    match embedder.embed("dimension probe") {
        Ok(query) => {
            println!("query model id   : {}", query.model_id);
            println!("query dimensions : {}", query.dimensions());
        }
        Err(error) => {
            println!("embedding FAILED: {}", error.message());
            return;
        }
    }

    let service = IntelligenceService::new(
        Arc::clone(&database),
        Arc::clone(&generator) as Arc<_>,
        Arc::clone(&embedder) as Arc<_>,
    );

    // ------------------------------------------------- 2/3/7. The real queries
    rule("2, 3 & 7 — RETRIEVAL CANDIDATES FOR EACH QUERY");
    let queries = [
        question.as_str(),
        "What happened in the incident?",
        "avalanche occurred at mount abu",
        "Where did the avalanche occur?",
    ];
    for q in queries {
        probe(&service, &database, &embedder, q);
    }

    // ------------------------------------------------------- 9. Control test
    rule("9 — CONTROL: ingest known text INTO THE SNAPSHOT, then re-ask");
    match service.ingest_document(
        "Mount Abu Avalanche (diagnostic control)",
        "diagnostic",
        "SYNTHETIC",
        "An avalanche occurred at Mount Abu. The incident was classified as high severity.",
    ) {
        Ok(Some(id)) => println!("ingested control document id={id}"),
        Ok(None) => println!("control document already present"),
        Err(error) => println!("ingest failed: {}", error.message()),
    }
    match service.index_pending() {
        Ok(report) => println!(
            "index_pending: {} chunks, {} incidents, {} failures",
            report.chunks_embedded, report.incidents_embedded, report.failures
        ),
        Err(error) => println!("index_pending failed: {}", error.message()),
    }
    println!("vectors now: {}", database.count_embeddings().unwrap_or(0));

    println!();
    probe(
        &service,
        &database,
        &embedder,
        "What happened at Mount Abu?",
    );
    probe(&service, &database, &embedder, question.as_str());

    service.unload();
    let _ = std::fs::remove_dir_all(&scratch);
    println!("\nsnapshot discarded; the live node was never written to.");
}

/// Reports every intermediate retrieval decision for one question.
fn probe(
    service: &IntelligenceService,
    database: &Database,
    embedder: &Arc<LlamaServerEngine>,
    question: &str,
) {
    println!("\n--- query: {question:?}");
    let Ok(query) = embedder.embed(question) else {
        println!("  query embedding FAILED");
        return;
    };
    println!("  query embedded OK ({} dims)", query.dimensions());

    // Threshold removed, so the real scores are visible rather than inferred.
    match database.search_embeddings(&query, 10, 0.0) {
        Ok(all) => {
            println!("  candidates (threshold 0.0): {}", all.len());
            for passage in &all {
                let verdict = if passage.score >= securemesh_lib::ai::rag::MIN_RELEVANCE {
                    "PASS"
                } else {
                    "below threshold"
                };
                println!(
                    "    {verdict:16} score {:.4}  {:?}  subject={}  title={:?}",
                    passage.score, passage.kind, passage.subject_id, passage.source_title
                );
                let excerpt: String = passage.content.chars().take(90).collect();
                println!("        text: {excerpt:?}");
            }
            let top = all.first().map(|p| p.score).unwrap_or(0.0);
            println!(
                "  top_score {:.4} vs threshold {} -> {}",
                top,
                securemesh_lib::ai::rag::MIN_RELEVANCE,
                if top >= securemesh_lib::ai::rag::MIN_RELEVANCE {
                    "passes"
                } else {
                    "REJECTED by threshold"
                }
            );
        }
        Err(error) => println!("  search failed: {}", error.message()),
    }

    match service.ask(question, Some(5)) {
        Ok(answer) => {
            println!(
                "  => retrieval {} ms | generation {} ms | grounded {} | refused {} | sources {}",
                answer.retrieval_ms,
                answer.generation_ms,
                answer.grounded,
                answer.refused,
                answer.sources.len()
            );
            println!(
                "  => generation {}",
                if answer.generation_ms > 0 {
                    "INVOKED"
                } else {
                    "SKIPPED — model never called"
                }
            );
            let shown: String = answer.answer.chars().take(220).collect();
            println!("  => answer: {shown}");
        }
        Err(error) => println!("  ask failed: {}", error.message()),
    }
}
