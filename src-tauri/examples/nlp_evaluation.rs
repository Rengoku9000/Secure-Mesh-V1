//! Measures the laptop-side language layer: rule extraction, the semantic
//! category fallback, and the duplicate/related thresholds.
//!
//! ```text
//! cargo run --example nlp_evaluation
//! ```
//!
//! Part A needs nothing but the binary. Parts B–C use the local BGE embedding
//! model when it is provisioned and say so when it is not. Nothing is written
//! to any node's database.
//!
//! # A caution about Part A
//!
//! The synthetic corpus is template-generated, and the rule vocabulary was
//! written by someone who had read those templates. Part A therefore measures
//! an upper bound. The held-out set in Part B was written separately, in
//! different words, and is the more honest figure.

use securemesh_lib::ai::dataset;
use securemesh_lib::ai::embedding::{cosine_similarity, EmbeddingEngine};
use securemesh_lib::ai::insight::{self, prototype_text};
use securemesh_lib::ai::nlp::extract;
use securemesh_lib::ai::{LlamaConfig, LlamaServerEngine, LocalInferenceEngine};
use securemesh_lib::domain::IncidentCategory as C;
use std::path::Path;
use std::time::Instant;

/// Hand-written reports, deliberately phrased unlike the synthetic templates.
const HELD_OUT: &[(&str, C)] = &[
    ("The building is burning", C::Fire),
    ("Flames spotted on the roof of the school", C::Fire),
    ("Structure fire reported at the textile mill", C::Fire),
    ("Major fire outbreak in the slum cluster", C::Fire),
    ("Thick black smoke pouring out of the godown", C::Fire),
    ("Shop gutted, still smouldering", C::Fire),
    ("An elderly man has collapsed and is not breathing", C::Medical),
    ("Three children with high fever and vomiting at the relief camp", C::Medical),
    ("Woman in labour needs urgent help, no vehicle available", C::Medical),
    ("2 persons require medical help near the bus stand", C::Medical),
    ("Water entering houses in the low-lying colony", C::Flooding),
    ("The river has breached the embankment near the village", C::Flooding),
    ("Streets are knee-deep in water after the downpour", C::Flooding),
    ("Flash flood swept through the camp at night", C::Flooding),
    ("Cracks have appeared in the flyover pillar", C::Infrastructure),
    ("Part of the school boundary wall came down", C::Infrastructure),
    ("The old footbridge partially collapsed", C::Infrastructure),
    ("No electricity in the whole ward since morning", C::Power),
    ("Transformer blew up, the area is in darkness", C::Power),
    ("Power cut across the eastern district", C::Power),
    ("Mobile towers are down, no signal anywhere", C::Communications),
    ("We have lost radio contact with the forward team", C::Communications),
    ("Internet and phone lines dead in the valley", C::Communications),
    ("Boulders on the ghat road, vehicles cannot pass", C::RoadBlockage),
    ("A fallen tree is blocking the highway", C::RoadBlockage),
    ("Mudslide has cut off the village road", C::RoadBlockage),
    ("Strong shaking felt for twenty seconds, people ran outside", C::Earthquake),
    ("Aftershock rattled the town this morning", C::Earthquake),
    ("Cyclone winds tearing roofs off homes", C::SevereWeather),
    ("Hailstorm damaged crops and vehicles", C::SevereWeather),
    ("Lightning struck the temple tower", C::SevereWeather),
    ("Families are being moved to the school shelter", C::Evacuation),
    ("Villagers relocated to higher ground overnight", C::Evacuation),
    ("The camp has run out of drinking water", C::ResourceShortage),
    ("Only one day of rations left for 200 people", C::ResourceShortage),
    ("Diesel for the generators is almost finished", C::ResourceShortage),
];

/// (a, b) pairs describing the same event in different words.
const DUPLICATES: &[(&str, &str)] = &[
    ("Heavy smoke reported near Block B. Around 5 people may still be inside.",
     "Fire at Block B, about five residents believed trapped inside."),
    ("Landslip has closed the mountain pass near Zone C.",
     "Mountain pass at Zone C blocked by a landslide."),
    ("Two people injured in a bus collision on NH-48.",
     "Bus crash on NH-48, two injured."),
    ("Power outage across the eastern district after the substation failed.",
     "Eastern district has no electricity; substation down."),
    ("River overflowed and water is entering homes in Ward 12.",
     "Ward 12 houses flooded after the river burst its banks."),
    ("Mobile network down across the valley.",
     "No phone signal anywhere in the valley."),
    ("The relief camp at Sector 9 has run out of drinking water.",
     "Sector 9 relief camp: no drinking water left."),
];

/// Same kind of incident, different events.
const RELATED: &[(&str, &str)] = &[
    ("Fire at Block B, about five residents believed trapped inside.",
     "Small kitchen fire at the hostel in Sector 4, already extinguished."),
    ("Landslip has closed the mountain pass near Zone C.",
     "Fallen tree blocking the service road in Zone A."),
    ("Bus crash on NH-48, two injured.",
     "Motorbike skidded near the market, rider hurt."),
    ("Power outage across the eastern district after the substation failed.",
     "Transformer sparking near the school, power fluctuating."),
    ("Ward 12 houses flooded after the river burst its banks.",
     "Water logging in the railway underpass after rain."),
    ("Mobile network down across the valley.",
     "Radio repeater on the ridge is not responding."),
];

/// Unrelated reports.
const UNRELATED: &[(&str, &str)] = &[
    ("Fire at Block B, about five residents believed trapped inside.",
     "The relief camp at Sector 9 has run out of drinking water."),
    ("Bus crash on NH-48, two injured.",
     "Mobile network down across the valley."),
    ("Ward 12 houses flooded after the river burst its banks.",
     "Aftershock cracked the school wall."),
    ("Power outage across the eastern district after the substation failed.",
     "Families are being moved to the school shelter."),
    ("Landslip has closed the mountain pass near Zone C.",
     "An elderly man has collapsed and is not breathing."),
    ("Heavy smoke reported near Block B.",
     "Hailstorm damaged crops and vehicles."),
];

fn stats(values: &[f32]) -> String {
    let min = values.iter().cloned().fold(f32::INFINITY, f32::min);
    let max = values.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
    let mean = values.iter().sum::<f32>() / values.len().max(1) as f32;
    format!("min {min:.3}  mean {mean:.3}  max {max:.3}  (n={})", values.len())
}

fn main() {
    // --- Part A: rules on the synthetic corpora ---------------------------
    println!("=== A. Rule layer on synthetic corpora (upper bound; see header) ===");
    for (seed, count) in [(42u64, 500usize), (7, 300)] {
        let corpus = dataset::generate(seed, count);
        let mut category = 0usize;
        let mut severity = 0usize;
        let started = Instant::now();
        for incident in &corpus.incidents {
            let e = extract(&incident.description);
            if e.category == incident.expected_category {
                category += 1;
            }
            if e.severity.level.as_str() == incident.expected_severity {
                severity += 1;
            }
        }
        let per = started.elapsed().as_micros() as f64 / count as f64;
        println!(
            "seed {seed:>2} n={count}: category {:.1}%  severity {:.1}%  {per:.0} µs/report",
            category as f64 * 100.0 / count as f64,
            severity as f64 * 100.0 / count as f64,
        );
    }

    // --- Part B: held-out phrasing ------------------------------------------
    println!("\n=== B. Held-out hand-written reports (n={}) ===", HELD_OUT.len());
    let mut rules_correct = 0usize;
    let mut rules_silent = 0usize;
    for (text, expected) in HELD_OUT {
        let e = extract(text);
        if e.category == *expected {
            rules_correct += 1;
        } else if e.category == C::Other {
            rules_silent += 1;
        }
    }
    println!(
        "rules only        : {}/{} correct ({:.1}%), {} with no cue (OTHER)",
        rules_correct,
        HELD_OUT.len(),
        rules_correct as f64 * 100.0 / HELD_OUT.len() as f64,
        rules_silent
    );

    // --- Parts B/C with the embedding model ---------------------------------
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf();
    let config = LlamaConfig::embedding(&root, 19_201);
    if let Err(reason) = config.availability() {
        println!("\nembedding model not provisioned ({}); semantic parts skipped", reason.detail());
        return;
    }
    let embedder = LlamaServerEngine::new(config);

    let prototypes: Vec<_> = C::ALL
        .iter()
        .filter(|c| **c != C::Other)
        .map(|c| (*c, embedder.embed(&prototype_text(*c)).expect("embed prototype")))
        .collect();

    let mut combined = 0usize;
    let mut semantic_only = 0usize;
    let mut misses = Vec::new();
    for (text, expected) in HELD_OUT {
        let e = extract(text);
        let vector = embedder.embed(text).expect("embed");
        let semantic = insight::semantic_category(&vector, &prototypes);
        if semantic.is_some_and(|s| s.category == *expected) {
            semantic_only += 1;
        }
        let (chosen, method) = insight::choose_category(&e, semantic);
        if chosen == *expected {
            combined += 1;
        } else {
            misses.push(format!("{text:?} → {chosen} via {method:?} (expected {expected})"));
        }
    }
    println!(
        "semantic only     : {}/{} ({:.1}%)",
        semantic_only,
        HELD_OUT.len(),
        semantic_only as f64 * 100.0 / HELD_OUT.len() as f64
    );
    println!(
        "rules + fallback  : {}/{} ({:.1}%)",
        combined,
        HELD_OUT.len(),
        combined as f64 * 100.0 / HELD_OUT.len() as f64
    );
    for miss in &misses {
        println!("  miss: {miss}");
    }

    // Semantic fallback on the synthetic corpus, where rules are strongest.
    let corpus = dataset::generate(42, 120);
    let mut hits = 0usize;
    let mut answered = 0usize;
    for incident in &corpus.incidents {
        let vector = embedder.embed(&incident.description).expect("embed");
        if let Some(s) = insight::semantic_category(&vector, &prototypes) {
            answered += 1;
            if s.category == incident.expected_category {
                hits += 1;
            }
        }
    }
    println!(
        "semantic on synthetic seed 42 n=120: answered {answered}, correct {hits} ({:.1}% of all)",
        hits as f64 * 100.0 / 120.0
    );

    // --- Part C: duplicate / related calibration ----------------------------
    println!("\n=== C. Similarity bands (BGE-small cosine) ===");
    let score = |pairs: &[(&str, &str)]| -> Vec<f32> {
        pairs
            .iter()
            .map(|(a, b)| {
                let va = embedder.embed(a).expect("embed");
                let vb = embedder.embed(b).expect("embed");
                cosine_similarity(&va.vector, &vb.vector)
            })
            .collect()
    };
    let duplicates = score(DUPLICATES);
    let related = score(RELATED);
    let unrelated = score(UNRELATED);
    println!("same event restated : {}", stats(&duplicates));
    println!("same kind, different: {}", stats(&related));
    println!("unrelated           : {}", stats(&unrelated));
    println!(
        "thresholds in use   : duplicate {} / possible {} / related {}",
        insight::DUPLICATE_MIN,
        insight::POSSIBLE_DUPLICATE_MIN,
        insight::RELATED_MIN
    );
    let band = |s: f32| {
        if s >= insight::DUPLICATE_MIN {
            "DUPLICATE"
        } else if s >= insight::POSSIBLE_DUPLICATE_MIN {
            "POSSIBLE_DUPLICATE"
        } else if s >= insight::RELATED_MIN {
            "RELATED"
        } else {
            "none"
        }
    };
    for (label, values) in [("dup", &duplicates), ("rel", &related), ("unrel", &unrelated)] {
        let bands: Vec<&str> = values.iter().map(|s| band(*s)).collect();
        println!("  {label:<5} → {}", bands.join(", "));
    }

    embedder.unload();
}
