//! A deterministic generator for synthetic evaluation data.
//!
//! # This data is synthetic and is labelled as such everywhere
//!
//! Nothing here describes a real event, place, or person. It exists to exercise
//! extraction and retrieval, and every record it produces is marked
//! `SYNTHETIC`. It must never be presented as operational data — see the data
//! policy in `README.md`.
//!
//! # Why deterministic
//!
//! An evaluation whose inputs change between runs cannot tell a model
//! regression from a data change. The generator is a small explicit PRNG seeded
//! by the caller, so a dataset version plus a seed reproduces the same corpus
//! byte for byte, on any machine.
//!
//! It is deliberately *not* `rand`: reproducibility here depends on the exact
//! algorithm, and a dependency could change its generator in a minor release
//! and silently invalidate every recorded benchmark.

use crate::domain::IncidentCategory;
use serde::{Deserialize, Serialize};

/// Bumped whenever generation changes in a way that alters the corpus.
///
/// Recorded in every benchmark, so results from different corpora are never
/// compared as though they measured the same thing.
pub const DATASET_VERSION: &str = "synthetic-v2";

/// A reproducible PRNG.
///
/// SplitMix64: tiny, well-distributed, and — most importantly — fully specified
/// here, so its output cannot drift with a dependency update.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        // A zero seed would make the sequence degenerate.
        Self(seed.wrapping_add(0x9E37_79B9_7F4A_7C15))
    }

    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn pick<'a, T>(&mut self, options: &'a [T]) -> &'a T {
        &options[(self.next() % options.len() as u64) as usize]
    }

    fn range(&mut self, low: u32, high: u32) -> u32 {
        if high <= low {
            return low;
        }
        low + (self.next() % (high - low) as u64) as u32
    }
}

/// A synthetic incident with the labels an evaluation compares against.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyntheticIncident {
    pub id: String,
    /// The natural-language report a model is given.
    pub description: String,
    /// Ground truth: what a correct analysis should conclude.
    pub expected_category: IncidentCategory,
    pub expected_severity: String,
    /// Terms a correct extraction should surface. Checked as a recall measure
    /// rather than an exact match, since wording legitimately varies.
    pub expected_keywords: Vec<String>,
    pub zone: String,
    /// Always `true`. Present in the data itself so a record cannot be mistaken
    /// for a real report once it leaves the generator.
    pub synthetic: bool,
}

/// A question with the answer an evaluation expects to find.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvaluationQuestion {
    pub id: String,
    pub question: String,
    /// Terms a grounded answer should contain.
    pub expected_terms: Vec<String>,
    /// True when the corpus genuinely cannot answer, so refusing is the
    /// correct behaviour. Without these, an evaluation rewards a model that
    /// always answers.
    pub expect_refusal: bool,
}

/// A generated corpus.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyntheticDataset {
    pub version: String,
    pub seed: u64,
    pub incidents: Vec<SyntheticIncident>,
    pub questions: Vec<EvaluationQuestion>,
}

/// One scenario family: the phrasings and labels that go together.
struct Scenario {
    category: IncidentCategory,
    severity: &'static str,
    templates: &'static [&'static str],
    assets: &'static [&'static str],
    keywords: &'static [&'static str],
}

/// The scenario families, covering the situations named in the Phase 3 brief.
const SCENARIOS: &[Scenario] = &[
    Scenario {
        category: IncidentCategory::Infrastructure,
        severity: "HIGH",
        templates: &[
            "The {asset} in {zone} has partially collapsed after sustained rainfall. {detail}",
            "Structural damage reported to the {asset} in {zone}. {detail}",
            "Engineers report the {asset} in {zone} is no longer safe to use. {detail}",
        ],
        assets: &[
            "road bridge",
            "footbridge",
            "culvert",
            "retaining wall",
            "overpass",
        ],
        keywords: &["collapse", "structural", "bridge"],
    },
    Scenario {
        category: IncidentCategory::Flooding,
        severity: "HIGH",
        templates: &[
            "Floodwater is rising across {zone} after the {asset} overflowed. {detail}",
            "{zone} is inundated following heavy rain; the {asset} has burst its banks. {detail}",
            "Standing water up to one metre reported throughout {zone} near the {asset}. {detail}",
        ],
        assets: &[
            "river",
            "drainage channel",
            "reservoir",
            "canal",
            "storm drain",
        ],
        keywords: &["flood", "water", "rising"],
    },
    Scenario {
        category: IncidentCategory::Fire,
        severity: "CRITICAL",
        templates: &[
            "A fire has broken out at the {asset} in {zone}. {detail}",
            "Smoke and flames visible from the {asset} in {zone}. {detail}",
            "Fire spreading rapidly through the {asset} in {zone}. {detail}",
        ],
        assets: &[
            "warehouse",
            "market building",
            "fuel depot",
            "residential block",
            "workshop",
        ],
        keywords: &["fire", "smoke", "flames"],
    },
    Scenario {
        category: IncidentCategory::Medical,
        severity: "HIGH",
        templates: &[
            "Multiple casualties requiring treatment at the {asset} in {zone}. {detail}",
            "Medical team requests support at the {asset} in {zone}. {detail}",
            "Injured persons being triaged at the {asset} in {zone}. {detail}",
        ],
        assets: &[
            "clinic",
            "field hospital",
            "community centre",
            "school",
            "relief camp",
        ],
        keywords: &["casualties", "medical", "injured"],
    },
    Scenario {
        category: IncidentCategory::Evacuation,
        severity: "HIGH",
        templates: &[
            "Evacuation under way from {zone} towards the {asset}. {detail}",
            "Residents of {zone} are being moved to the {asset}. {detail}",
            "Evacuation of {zone} ordered; assembly point is the {asset}. {detail}",
        ],
        assets: &[
            "assembly point",
            "higher ground",
            "shelter",
            "school hall",
            "transit centre",
        ],
        keywords: &["evacuation", "residents", "shelter"],
    },
    Scenario {
        category: IncidentCategory::Power,
        severity: "MEDIUM",
        templates: &[
            "Power outage affecting {zone} after damage to the {asset}. {detail}",
            "The {asset} serving {zone} is offline. {detail}",
            "Electricity supply to {zone} lost; the {asset} is damaged. {detail}",
        ],
        assets: &[
            "substation",
            "distribution line",
            "transformer",
            "generator",
            "feeder cable",
        ],
        keywords: &["power", "outage", "electricity"],
    },
    Scenario {
        category: IncidentCategory::Communications,
        severity: "MEDIUM",
        templates: &[
            "Communications lost across {zone}; the {asset} is not responding. {detail}",
            "The {asset} serving {zone} has failed, leaving no coverage. {detail}",
            "No radio or mobile contact with {zone} since the {asset} went down. {detail}",
        ],
        assets: &[
            "relay mast",
            "repeater station",
            "fibre link",
            "base station",
            "antenna array",
        ],
        keywords: &["communications", "coverage", "contact"],
    },
    Scenario {
        category: IncidentCategory::ResourceShortage,
        severity: "MEDIUM",
        templates: &[
            "Supplies of {asset} at the {zone} distribution point are nearly exhausted. {detail}",
            "{zone} reports a critical shortage of {asset}. {detail}",
            "Stocks of {asset} in {zone} will last less than a day. {detail}",
        ],
        assets: &[
            "drinking water",
            "fuel",
            "blankets",
            "medical supplies",
            "dry rations",
        ],
        keywords: &["shortage", "supplies", "stock"],
    },
    Scenario {
        category: IncidentCategory::Earthquake,
        severity: "CRITICAL",
        templates: &[
            "Tremor felt across {zone}; the {asset} shows visible cracking. {detail}",
            "Aftershock damage reported to the {asset} in {zone}. {detail}",
            "Seismic activity in {zone} has destabilised the {asset}. {detail}",
        ],
        assets: &[
            "apartment block",
            "water tower",
            "school building",
            "clinic wall",
            "bridge pier",
        ],
        keywords: &["tremor", "seismic", "cracking"],
    },
    Scenario {
        category: IncidentCategory::SevereWeather,
        severity: "HIGH",
        templates: &[
            "High winds across {zone} have brought down the {asset}. {detail}",
            "Severe storm over {zone}; the {asset} is damaged. {detail}",
            "Hail and gale-force winds in {zone} have destroyed the {asset}. {detail}",
        ],
        assets: &[
            "temporary shelter",
            "power line",
            "roof structure",
            "market awning",
            "signage",
        ],
        keywords: &["wind", "storm", "severe"],
    },
    Scenario {
        category: IncidentCategory::RoadBlockage,
        severity: "MEDIUM",
        templates: &[
            "The {asset} through {zone} is blocked by debris. {detail}",
            "Landslip has closed the {asset} in {zone}. {detail}",
            "A fallen tree is obstructing the {asset} in {zone}. {detail}",
        ],
        assets: &[
            "main highway",
            "access road",
            "mountain pass",
            "service road",
            "link road",
        ],
        keywords: &["blocked", "road", "debris"],
    },
];

const ZONES: &[&str] = &[
    "Zone A",
    "Zone B",
    "Zone C",
    "the northern sector",
    "the eastern district",
    "the southern approach",
    "the western quarter",
    "the central district",
];

const DETAILS: &[&str] = &[
    "Two vehicles are stranded and access from the east is blocked.",
    "No casualties reported so far.",
    "A response team has been requested.",
    "Approximately forty people are affected.",
    "The situation is deteriorating and access is restricted.",
    "Local responders are on scene and the route remains open.",
    "Conditions are stable but monitoring continues.",
];

/// Generates a reproducible corpus.
///
/// The same `seed` and `count` always produce the same records, so a benchmark
/// can name its dataset precisely.
pub fn generate(seed: u64, count: usize) -> SyntheticDataset {
    let mut rng = Rng::new(seed);
    let mut incidents = Vec::with_capacity(count);

    for index in 0..count {
        let scenario = rng.pick(SCENARIOS);
        let template = rng.pick(scenario.templates);
        let asset = rng.pick(scenario.assets);
        let zone = rng.pick(ZONES);
        let detail = rng.pick(DETAILS);

        let description = template
            .replace("{asset}", asset)
            .replace("{zone}", zone)
            .replace("{detail}", detail);

        // A minority of reports are escalated, so severity is not perfectly
        // predictable from category alone — otherwise the task is trivial and
        // the measurement meaningless.
        let severity = if rng.range(0, 100) < 20 {
            escalate(scenario.severity)
        } else {
            scenario.severity.to_string()
        };

        let mut keywords: Vec<String> = scenario.keywords.iter().map(|k| k.to_string()).collect();
        keywords.push(asset.to_string());

        incidents.push(SyntheticIncident {
            id: format!("SYN-{index:04}"),
            description,
            expected_category: scenario.category,
            expected_severity: severity,
            expected_keywords: keywords,
            zone: zone.to_string(),
            synthetic: true,
        });
    }

    let questions = generate_questions(&incidents);

    SyntheticDataset {
        version: DATASET_VERSION.to_string(),
        seed,
        incidents,
        questions,
    }
}

fn escalate(severity: &str) -> String {
    match severity {
        "LOW" => "MEDIUM",
        "MEDIUM" => "HIGH",
        _ => "CRITICAL",
    }
    .to_string()
}

/// Builds evaluation questions from the corpus that was actually generated.
///
/// Derived from the data rather than hard-coded, so a question can never ask
/// about something the corpus does not contain — except the refusal cases,
/// which are unanswerable on purpose.
fn generate_questions(incidents: &[SyntheticIncident]) -> Vec<EvaluationQuestion> {
    let mut questions = Vec::new();

    // One question per category actually present.
    for category in IncidentCategory::ALL {
        if !incidents.iter().any(|i| i.expected_category == category) {
            continue;
        }
        let label = category.as_str().to_lowercase().replace('_', " ");
        questions.push(EvaluationQuestion {
            id: format!("Q-CAT-{category}"),
            question: format!("What {label} incidents have been reported?"),
            expected_terms: vec![label],
            expect_refusal: false,
        });
    }

    // One per zone that appears.
    let mut zones: Vec<&String> = incidents.iter().map(|i| &i.zone).collect();
    zones.sort();
    zones.dedup();
    for (index, zone) in zones.iter().take(8).enumerate() {
        questions.push(EvaluationQuestion {
            id: format!("Q-ZONE-{index}"),
            question: format!("What incidents are reported in {zone}?"),
            expected_terms: vec![(*zone).clone()],
            expect_refusal: false,
        });
    }

    // Questions the corpus cannot answer. A model that answers these is
    // hallucinating, and an evaluation without them rewards exactly that.
    let unanswerable = [
        "What is the current stock price of the national railway operator?",
        "Who won the football match last night?",
        "What is the recommended torque setting for a helicopter rotor bolt?",
        "How many satellites are currently in geostationary orbit?",
        "What is the capital city of Mars?",
    ];

    // Spread through the answerable ones rather than appended.
    //
    // The harness samples a prefix of this list. Appended, a sample of 20 out of
    // 24 reached exactly one refusal case, and a refusal rate over n=1 is not a
    // measurement. Interleaving means any prefix exercises both kinds.
    let stride = (questions.len() / unanswerable.len()).max(1);
    let mut interleaved = Vec::with_capacity(questions.len() + unanswerable.len());
    let mut refusals = unanswerable.iter().enumerate();

    for (index, question) in questions.into_iter().enumerate() {
        if index % stride == 0 {
            if let Some((refusal_index, text)) = refusals.next() {
                interleaved.push(EvaluationQuestion {
                    id: format!("Q-REFUSE-{refusal_index}"),
                    question: (*text).to_string(),
                    expected_terms: vec![],
                    expect_refusal: true,
                });
            }
        }
        interleaved.push(question);
    }

    // Any that did not fit the stride still belong in the set.
    for (refusal_index, text) in refusals {
        interleaved.push(EvaluationQuestion {
            id: format!("Q-REFUSE-{refusal_index}"),
            question: (*text).to_string(),
            expected_terms: vec![],
            expect_refusal: true,
        });
    }

    interleaved
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn generation_is_reproducible_for_a_seed() {
        // The property the whole evaluation depends on.
        let first = generate(42, 200);
        let second = generate(42, 200);

        assert_eq!(first.incidents, second.incidents);
        assert_eq!(first.questions, second.questions);
    }

    #[test]
    fn different_seeds_produce_different_corpora() {
        let a = generate(1, 100);
        let b = generate(2, 100);
        assert_ne!(a.incidents, b.incidents);
    }

    #[test]
    fn the_target_corpus_size_is_achievable() {
        // The brief asks for 500-1000 incidents and 50-100 questions.
        let dataset = generate(7, 1_000);
        assert_eq!(dataset.incidents.len(), 1_000);
        assert!(
            dataset.questions.len() >= 5,
            "questions are derived from the corpus"
        );
    }

    #[test]
    fn every_record_is_marked_synthetic() {
        // The label travels with the data so it cannot be mistaken for real
        // reporting once it leaves the generator.
        let dataset = generate(3, 100);
        assert!(dataset.incidents.iter().all(|i| i.synthetic));
    }

    #[test]
    fn every_scenario_family_appears_in_a_reasonable_corpus() {
        let dataset = generate(11, 600);
        let categories: HashSet<_> = dataset
            .incidents
            .iter()
            .map(|i| i.expected_category)
            .collect();

        for scenario in SCENARIOS {
            assert!(
                categories.contains(&scenario.category),
                "{} should appear in 600 records",
                scenario.category
            );
        }
    }

    #[test]
    fn descriptions_have_no_unfilled_placeholders() {
        let dataset = generate(5, 300);
        for incident in &dataset.incidents {
            assert!(
                !incident.description.contains('{'),
                "{}",
                incident.description
            );
            assert!(
                !incident.description.contains('}'),
                "{}",
                incident.description
            );
            assert!(incident.description.len() > 30);
        }
    }

    #[test]
    fn severity_is_not_perfectly_predictable_from_category() {
        // If it were, classification accuracy would measure nothing.
        let dataset = generate(13, 500);
        let flooding: HashSet<&str> = dataset
            .incidents
            .iter()
            .filter(|i| i.expected_category == IncidentCategory::Flooding)
            .map(|i| i.expected_severity.as_str())
            .collect();

        assert!(flooding.len() > 1, "some reports should be escalated");
    }

    #[test]
    fn identifiers_are_unique() {
        let dataset = generate(17, 500);
        let ids: HashSet<_> = dataset.incidents.iter().map(|i| &i.id).collect();
        assert_eq!(ids.len(), dataset.incidents.len());
    }

    #[test]
    fn the_question_set_includes_unanswerable_ones() {
        // Without these, an evaluation rewards a model that always answers.
        let dataset = generate(19, 200);
        let refusals = dataset
            .questions
            .iter()
            .filter(|q| q.expect_refusal)
            .count();

        assert!(refusals >= 5, "refusal cases must be represented");
        assert!(dataset.questions.iter().any(|q| !q.expect_refusal));
    }

    #[test]
    fn a_prefix_of_the_question_set_exercises_both_kinds() {
        // The harness samples a prefix. With the refusal cases appended, a
        // sample of 20 reached exactly one of them, and a refusal rate over
        // n=1 is not a measurement.
        let dataset = generate(19, 200);
        let half = dataset.questions.len() / 2;
        let prefix = &dataset.questions[..half];

        assert!(
            prefix.iter().filter(|q| q.expect_refusal).count() >= 2,
            "refusal cases must be spread through the set, not appended"
        );
        assert!(prefix.iter().any(|q| !q.expect_refusal));
    }

    #[test]
    fn answerable_questions_only_ask_about_the_generated_corpus() {
        let dataset = generate(23, 300);
        let zones: HashSet<&String> = dataset.incidents.iter().map(|i| &i.zone).collect();

        for question in dataset
            .questions
            .iter()
            .filter(|q| q.id.starts_with("Q-ZONE"))
        {
            let asked = &question.expected_terms[0];
            assert!(zones.contains(asked), "asked about an absent zone: {asked}");
        }
    }

    #[test]
    fn the_generator_does_not_drift_with_a_dependency() {
        // Pinning one value catches an accidental change to the PRNG, which
        // would silently invalidate every recorded benchmark.
        let mut rng = Rng::new(42);
        assert_eq!(rng.next(), 2_949_826_092_126_892_291);
    }

    #[test]
    fn an_empty_corpus_is_handled() {
        let dataset = generate(1, 0);
        assert!(dataset.incidents.is_empty());
        // Only the unanswerable questions remain, which is correct.
        assert!(dataset.questions.iter().all(|q| q.expect_refusal));
    }

    #[test]
    fn the_dataset_version_is_recorded_with_the_corpus() {
        let dataset = generate(1, 10);
        assert_eq!(dataset.version, DATASET_VERSION);
        assert_eq!(dataset.seed, 1);
    }
}
