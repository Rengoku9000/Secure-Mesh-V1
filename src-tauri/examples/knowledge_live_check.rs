//! Runs the operational-knowledge demonstration against the real local models.
//!
//! Exists because the claim being made is about a pipeline, and a stub embedder
//! cannot check it. Whether "What should I do in heavy rain?" actually retrieves
//! the heavy rain procedure is a property of BGE and of the documents; only the
//! provisioned models can answer it.
//!
//! Everything runs against a scratch node in the temp directory, so an
//! operator's running node is untouched.
//!
//! ```text
//! cargo run --release --example knowledge_live_check
//! ```

use securemesh_lib::ai::{IntelligenceService, LlamaConfig, LlamaServerEngine};
use securemesh_lib::domain::{LocationSource, NewIncident};
use securemesh_lib::storage::intelligence::PassageSource;
use securemesh_lib::NodeRuntime;
use std::path::Path;
use std::sync::Arc;

fn rule(title: &str) {
    println!("\n{}", "=".repeat(72));
    println!("{title}");
    println!("{}", "=".repeat(72));
}

fn label(source: PassageSource) -> &'static str {
    match source {
        PassageSource::OperationalKnowledge => "Operational Knowledge",
        PassageSource::LiveIncident => "Live Incident       ",
        PassageSource::ImportedDocument => "Imported Document   ",
    }
}

fn ask(runtime: &NodeRuntime, question: &str, expectation: &str) -> bool {
    println!("\nQ: {question}");
    println!("   expected: {expectation}");

    let answer = match runtime.ask_intelligence(question, Some(6)) {
        Ok(answer) => answer,
        Err(error) => {
            println!("   FAILED: {}", error.message());
            return false;
        }
    };

    println!(
        "   verdict : {}",
        if answer.refused {
            "NO RELEVANT LOCAL KNOWLEDGE"
        } else if answer.grounded {
            "ANSWERED FROM LOCAL KNOWLEDGE"
        } else {
            "MODEL INTERPRETATION - NOT CITED"
        }
    );
    println!(
        "   timing  : retrieval {} ms · generation {} ms",
        answer.retrieval_ms, answer.generation_ms
    );
    println!(
        "   support : {:.2}  (threshold {:.2})",
        answer.answer_support,
        securemesh_lib::ai::rag::MIN_ANSWER_SUPPORT
    );

    if !answer.refused {
        println!(
            "   answer  : {}",
            answer.answer.replace('\n', "\n             ")
        );
    }

    if answer.sources.is_empty() {
        println!("   sources : (none)");
    } else {
        println!("   sources :");
        for source in &answer.sources {
            println!(
                "     [{}] {}  {}  score {:.3}{}",
                source.marker,
                label(source.source),
                source.title,
                source.score,
                if source.cited { "  (cited)" } else { "" }
            );
        }
    }

    !answer.refused
}

fn main() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("project root")
        .to_path_buf();

    let embedding = LlamaConfig::embedding(&root, 19_421);
    let generation = LlamaConfig::generation(&root, 19_420);
    for config in [&embedding, &generation] {
        if let Err(reason) = config.availability() {
            println!("models are not provisioned: {}", reason.detail());
            println!("nothing was checked.");
            return;
        }
    }

    let dir = std::env::temp_dir().join("securemesh-knowledge-live-check");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch directory");
    println!("scratch node : {}", dir.display());

    let mut runtime = NodeRuntime::initialize(&dir).expect("initialise node");
    runtime.attach_intelligence(IntelligenceService::new(
        runtime.database_handle(),
        Arc::new(LlamaServerEngine::new(generation)),
        Arc::new(LlamaServerEngine::new(embedding)),
    ));

    // ---------------------------------------------------------------- install
    rule("1 - INSTALL THE OPERATIONAL KNOWLEDGE PACK");
    let started = std::time::Instant::now();
    let report = runtime
        .install_operational_knowledge()
        .expect("install the pack");
    println!(
        "installed {} document(s), {} already present, {} chunks in {} ms",
        report.documents_installed,
        report.documents_already_present,
        report.chunks_created,
        started.elapsed().as_millis()
    );

    println!("\nre-running the install to prove it is idempotent…");
    let again = runtime
        .install_operational_knowledge()
        .expect("reinstall the pack");
    println!(
        "installed {}, already present {}, chunks added {}",
        again.documents_installed, again.documents_already_present, again.chunks_created
    );
    assert_eq!(again.documents_installed, 0, "the pack duplicated itself");
    assert_eq!(again.chunks_created, 0, "chunks were duplicated");

    // ------------------------------------------------------------------ index
    rule("2 - EMBED (local BGE)");
    let started = std::time::Instant::now();
    // The indexer runs in batches; loop until nothing is left to embed.
    loop {
        let pass = runtime.index_intelligence().expect("index");
        if pass.chunks_embedded == 0 && pass.incidents_embedded == 0 {
            break;
        }
        println!(
            "  embedded {} chunk(s), {} incident(s), {} failure(s)",
            pass.chunks_embedded, pass.incidents_embedded, pass.failures
        );
    }
    println!("embedding finished in {} ms", started.elapsed().as_millis());

    let summary = runtime.knowledge_summary().expect("summary");
    println!("\nLOCAL KNOWLEDGE");
    println!(
        "  operational documents : {}",
        summary.operational_documents
    );
    println!("  imported documents    : {}", summary.imported_documents);
    println!(
        "  live incidents        : {} indexed of {} held",
        summary.live_incidents_indexed, summary.live_incidents_total
    );
    println!("  chunks                : {}", summary.chunks);
    println!("  vectors               : {}", summary.vectors);

    // -------------------------------------------------- questions A, B and E
    rule("3 - OPERATIONAL QUESTIONS, NO INCIDENTS YET");
    let a = ask(
        &runtime,
        "What should I do in heavy rain?",
        "grounded in Heavy Rain Response",
    );
    let b = ask(
        &runtime,
        "What should I do during an avalanche?",
        "grounded in Avalanche Response",
    );
    let e = ask(
        &runtime,
        "What is the capital of France?",
        "refusal - nothing local supports this",
    );

    // ------------------------------------------------------------- incidents
    rule("4 - RECORD INCIDENTS (the demo scenario)");
    for (description, latitude, longitude) in [
        (
            "Avalanche reported at Mount Abu. The approach road is buried and two vehicles are cut off beyond the slide.",
            Some(24.5925),
            Some(72.7156),
        ),
        (
            "Heavy rain causing road blockage near the northern checkpoint.",
            Some(13.133599),
            Some(77.565330),
        ),
    ] {
        let incident = runtime
            .create_incident(NewIncident {
                description: description.to_string(),
                severity: "HIGH".to_string(),
                latitude,
                longitude,
                accuracy_meters: Some(6.0),
                location_source: Some(LocationSource::Gnss),
                location_captured_at: Some(chrono::Utc::now()),
            })
            .expect("create incident");
        println!(
            "  {}  {:?}  {}",
            &incident.id[..8],
            incident.location_source,
            description
        );
    }
    loop {
        let pass = runtime.index_intelligence().expect("index");
        if pass.chunks_embedded == 0 && pass.incidents_embedded == 0 {
            break;
        }
    }

    // -------------------------------------------------- questions C and D
    rule("5 - INCIDENT AND COMBINED QUESTIONS");
    let c = ask(
        &runtime,
        "What happened at Mount Abu?",
        "grounded in the live incident",
    );
    let d = ask(
        &runtime,
        "An avalanche was reported at Mount Abu. What should the field team do?",
        "grounded in BOTH the avalanche procedure and the live incident",
    );
    let f = ask(
        &runtime,
        "What should the team do about the heavy rain road blockage at the checkpoint?",
        "grounded in heavy rain + road blockage guidance and the live incident",
    );

    rule("RESULT");
    let checks = [
        ("A  heavy rain -> guidance", a),
        ("B  avalanche -> guidance", b),
        ("C  Mount Abu -> live incident", c),
        ("D  combined avalanche + incident", d),
        ("E  capital of France -> refusal", e),
        ("F  demo scenario, combined", f),
    ];
    for (name, passed) in checks {
        // E inverts: a refusal is the correct outcome there.
        let ok = if name.starts_with('E') {
            !passed
        } else {
            passed
        };
        println!("  {} {}", if ok { "PASS" } else { "FAIL" }, name);
    }
}
