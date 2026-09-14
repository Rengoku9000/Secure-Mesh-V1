//! Incident insight: rule extraction, semantic matching, and related reports.
//!
//! Everything here is **pure** — it is handed incidents, extractions and
//! vectors, and returns a value. It reads no database and calls no model, so
//! it behaves identically on a node with no AI, and the service layer decides
//! what inputs are available (vectors exist only once the indexer has run).
//!
//! # Derived, local, and disposable
//!
//! Nothing produced here is stored or replicated. An insight is recomputed
//! when asked for, from records the node already holds; the incident, its
//! signed event and the operator's severity are never touched. Two nodes may
//! compute different insights about the same incident — that is expected, and
//! it is why none of this goes on the wire.

use crate::ai::embedding::{cosine_similarity, Embedding};
use crate::ai::nlp::{Hazard, TextExtraction};
use crate::domain::{Incident, IncidentAnalysis, IncidentCategory, Severity};
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::collections::HashSet;

/// Cosine similarity (BGE-small) at or above which two reports read as the
/// same event restated. See `docs/ai/EVALUATION.md` for how these were
/// measured.
pub const DUPLICATE_MIN: f32 = 0.93;
pub const POSSIBLE_DUPLICATE_MIN: f32 = 0.86;
pub const RELATED_MIN: f32 = 0.76;

/// The same three bands when no vector is available, over the lexical score.
pub const LEXICAL_DUPLICATE_MIN: f32 = 0.75;
pub const LEXICAL_POSSIBLE_DUPLICATE_MIN: f32 = 0.55;
pub const LEXICAL_RELATED_MIN: f32 = 0.30;

/// Semantic category fallback: minimum similarity to a category prototype,
/// and the lead it needs over the runner-up to be trusted.
pub const SEMANTIC_CATEGORY_MIN: f32 = 0.55;
pub const SEMANTIC_CATEGORY_MARGIN: f32 = 0.02;

/// Below this rule confidence the semantic category, when there is one, is
/// preferred.
pub const LEXICAL_CONFIDENCE_MIN: f32 = 0.25;

pub const MAX_RELATED: usize = 5;
/// Incidents considered by a situation brief, newest first.
pub const BRIEF_LIMIT: usize = 200;
const EXCERPT_CHARS: usize = 160;

/// How a pair of incidents relate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Relation {
    Duplicate,
    PossibleDuplicate,
    Related,
}

/// Which evidence produced a judgement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Method {
    /// Local embedding vectors.
    Semantic,
    /// Word and hazard overlap — used when no vector exists yet.
    Lexical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CategoryMethod {
    /// From hazard cues in the text.
    Lexical,
    /// From similarity to category descriptions, when no cue fired.
    Semantic,
    /// Nothing recognisable either way.
    None,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RelatedIncident {
    pub incident_id: String,
    pub excerpt: String,
    pub operator_severity: Severity,
    pub created_at: DateTime<Utc>,
    pub similarity: f32,
    pub relation: Relation,
    pub method: Method,
    pub same_category: bool,
    pub shared_hazards: Vec<Hazard>,
    /// Straight-line distance when both incidents carry coordinates.
    pub distance_km: Option<f64>,
    pub hours_apart: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SemanticCategory {
    pub category: IncidentCategory,
    pub similarity: f32,
}

/// Everything derived about one incident, for the incident view.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IncidentInsight {
    pub incident_id: String,
    pub category: IncidentCategory,
    pub category_method: CategoryMethod,
    pub semantic_category: Option<SemanticCategory>,
    pub extraction: TextExtraction,
    /// Whether a stored model analysis agrees on the category. `None` when
    /// the incident has not been analysed.
    pub model_agrees: Option<bool>,
    pub related: Vec<RelatedIncident>,
    /// True when the derived severity differs from the operator's. Shown, never
    /// acted on.
    pub severity_differs_from_operator: bool,
    /// Whether vectors were available; lexical matching is the fallback.
    pub semantic_available: bool,
    pub elapsed_ms: u64,
}

/// Text for a category's prototype vector, used by the semantic fallback.
pub fn prototype_text(category: IncidentCategory) -> String {
    format!(
        "Emergency field report: {} ({}).",
        category.description(),
        category.as_str().to_lowercase().replace('_', " ")
    )
}

/// The closest category prototype, if it is close enough and clearly ahead.
pub fn semantic_category(
    vector: &Embedding,
    prototypes: &[(IncidentCategory, Embedding)],
) -> Option<SemanticCategory> {
    let mut scored: Vec<(IncidentCategory, f32)> = prototypes
        .iter()
        .filter(|(category, _)| *category != IncidentCategory::Other)
        .map(|(category, proto)| (*category, cosine_similarity(&vector.vector, &proto.vector)))
        .collect();
    scored.sort_by(|a, b| b.1.total_cmp(&a.1));

    let (category, top) = *scored.first()?;
    let second = scored.get(1).map_or(0.0, |s| s.1);
    (top >= SEMANTIC_CATEGORY_MIN && top - second >= SEMANTIC_CATEGORY_MARGIN).then_some(
        SemanticCategory {
            category,
            similarity: (top * 1000.0).round() / 1000.0,
        },
    )
}

/// Picks the category: rules when they are confident, the semantic fallback
/// when they are not, and `OTHER` when neither knows.
pub fn choose_category(
    extraction: &TextExtraction,
    semantic: Option<SemanticCategory>,
) -> (IncidentCategory, CategoryMethod) {
    let lexical = extraction.category != IncidentCategory::Other;
    if lexical && extraction.category_confidence >= LEXICAL_CONFIDENCE_MIN {
        return (extraction.category, CategoryMethod::Lexical);
    }
    if let Some(semantic) = semantic {
        return (semantic.category, CategoryMethod::Semantic);
    }
    if lexical {
        return (extraction.category, CategoryMethod::Lexical);
    }
    (IncidentCategory::Other, CategoryMethod::None)
}

// ---------------------------------------------------------------------------
// Lexical similarity
// ---------------------------------------------------------------------------

const STEM_STOPWORDS: &[&str] = &[
    "with", "from", "that", "this", "have", "been", "were", "there", "their", "reported",
    "report", "near", "after", "into", "about", "still", "some", "they", "them", "also",
    "which", "while", "being", "since", "will", "would", "could", "should", "across",
];

fn stem(word: &str) -> String {
    for suffix in ["ing", "ed", "es", "s"] {
        if let Some(root) = word.strip_suffix(suffix) {
            if root.chars().count() >= 4 {
                return root.to_string();
            }
        }
    }
    word.to_string()
}

fn content_stems(text: &str) -> HashSet<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 4 && !STEM_STOPWORDS.contains(w))
        .map(stem)
        .collect()
}

fn jaccard<T: std::hash::Hash + Eq>(a: &HashSet<T>, b: &HashSet<T>) -> f32 {
    if a.is_empty() && b.is_empty() {
        return 0.0;
    }
    a.intersection(b).count() as f32 / a.union(b).count() as f32
}

/// Word and hazard overlap between two reports, in `0..=1`.
pub fn lexical_similarity(
    a_text: &str,
    a: &TextExtraction,
    b_text: &str,
    b: &TextExtraction,
) -> f32 {
    let words = jaccard(&content_stems(a_text), &content_stems(b_text));
    let a_hazards: HashSet<Hazard> = a.active_hazards().collect();
    let b_hazards: HashSet<Hazard> = b.active_hazards().collect();
    if a_hazards.is_empty() && b_hazards.is_empty() {
        return words;
    }
    0.7 * words + 0.3 * jaccard(&a_hazards, &b_hazards)
}

fn relation_for(score: f32, method: Method) -> Option<Relation> {
    let (duplicate, possible, related) = match method {
        Method::Semantic => (DUPLICATE_MIN, POSSIBLE_DUPLICATE_MIN, RELATED_MIN),
        Method::Lexical => (
            LEXICAL_DUPLICATE_MIN,
            LEXICAL_POSSIBLE_DUPLICATE_MIN,
            LEXICAL_RELATED_MIN,
        ),
    };
    if score >= duplicate {
        Some(Relation::Duplicate)
    } else if score >= possible {
        Some(Relation::PossibleDuplicate)
    } else if score >= related {
        Some(Relation::Related)
    } else {
        None
    }
}

/// Great-circle distance, in kilometres.
pub fn haversine_km(a: (f64, f64), b: (f64, f64)) -> f64 {
    let (lat1, lon1) = (a.0.to_radians(), a.1.to_radians());
    let (lat2, lon2) = (b.0.to_radians(), b.1.to_radians());
    let h = ((lat2 - lat1) / 2.0).sin().powi(2)
        + lat1.cos() * lat2.cos() * ((lon2 - lon1) / 2.0).sin().powi(2);
    2.0 * 6_371.0 * h.sqrt().asin()
}

pub fn excerpt(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= EXCERPT_CHARS {
        return trimmed.to_string();
    }
    let cut: String = trimmed.chars().take(EXCERPT_CHARS).collect();
    format!("{}…", cut.trim_end())
}

fn round3(value: f32) -> f32 {
    (value * 1000.0).round() / 1000.0
}

/// A candidate for comparison: the incident, what the rules found, and its
/// vector if the indexer has produced one.
pub struct Candidate<'a> {
    pub incident: &'a Incident,
    pub extraction: &'a TextExtraction,
    pub vector: Option<&'a Embedding>,
}

/// Similarity between two candidates, by the best evidence both have.
fn similarity(a: &Candidate<'_>, b: &Candidate<'_>) -> (f32, Method) {
    match (a.vector, b.vector) {
        (Some(x), Some(y)) if x.model_id == y.model_id => {
            (cosine_similarity(&x.vector, &y.vector), Method::Semantic)
        }
        _ => (
            lexical_similarity(
                &a.incident.description,
                a.extraction,
                &b.incident.description,
                b.extraction,
            ),
            Method::Lexical,
        ),
    }
}

/// The incidents most related to `target`, strongest relation first.
pub fn related_incidents(target: &Candidate<'_>, others: &[Candidate<'_>]) -> Vec<RelatedIncident> {
    let target_hazards: HashSet<Hazard> = target.extraction.active_hazards().collect();
    let target_position = target.incident.latitude.zip(target.incident.longitude);

    let mut related: Vec<RelatedIncident> = others
        .iter()
        .filter(|other| other.incident.id != target.incident.id)
        .filter_map(|other| {
            let (score, method) = similarity(target, other);
            let relation = relation_for(score, method)?;
            let other_hazards: HashSet<Hazard> = other.extraction.active_hazards().collect();
            let mut shared: Vec<Hazard> = target_hazards.intersection(&other_hazards).copied().collect();
            shared.sort_by_key(|h| h.label());

            Some(RelatedIncident {
                incident_id: other.incident.id.clone(),
                excerpt: excerpt(&other.incident.description),
                operator_severity: other.incident.severity,
                created_at: other.incident.created_at,
                similarity: round3(score),
                relation,
                method,
                same_category: other.extraction.category == target.extraction.category
                    && target.extraction.category != IncidentCategory::Other,
                shared_hazards: shared,
                distance_km: target_position
                    .zip(other.incident.latitude.zip(other.incident.longitude))
                    .map(|(a, b)| (haversine_km(a, b) * 100.0).round() / 100.0),
                hours_apart: ((target.incident.created_at - other.incident.created_at)
                    .num_minutes()
                    .abs() as f64
                    / 60.0
                    * 10.0)
                    .round()
                    / 10.0,
            })
        })
        .collect();

    related.sort_by(|a, b| {
        a.relation
            .cmp(&b.relation)
            .then(b.similarity.total_cmp(&a.similarity))
    });
    related.truncate(MAX_RELATED);
    related
}

/// Assembles an insight from what the service gathered.
///
/// Corroboration raises the derived severity: several independent reports of
/// the same kind of event nearby in time are evidence that it is real and
/// widespread. Duplicates are *not* counted — a restated report is one report.
pub fn build_insight(
    target: &Candidate<'_>,
    others: &[Candidate<'_>],
    semantic: Option<SemanticCategory>,
    analysis: Option<&IncidentAnalysis>,
    semantic_available: bool,
    elapsed_ms: u64,
) -> IncidentInsight {
    let (category, category_method) = choose_category(target.extraction, semantic);
    let related = related_incidents(target, others);

    let mut extraction = target.extraction.clone();
    let corroborating = related
        .iter()
        .filter(|r| r.relation == Relation::Related && r.hours_apart <= 24.0)
        .count();
    if corroborating >= 2 {
        extraction.severity = extraction.severity.with_factor(
            format!("{corroborating} related reports within a day"),
            1,
        );
    }

    IncidentInsight {
        incident_id: target.incident.id.clone(),
        category,
        category_method,
        semantic_category: semantic,
        model_agrees: analysis.map(|a| a.category == category),
        severity_differs_from_operator: extraction.severity.level != target.incident.severity,
        extraction,
        related,
        semantic_available,
        elapsed_ms,
    }
}

// ---------------------------------------------------------------------------
// Situation brief
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CategoryCount {
    pub category: IncidentCategory,
    pub count: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SeverityCount {
    pub level: Severity,
    pub count: usize,
}

/// People figures summed across reports.
///
/// Summed, so two reports of the same five trapped people read as ten; the
/// duplicate groups beside these figures are how an operator spots that.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PeopleTotals {
    pub deceased: u32,
    pub trapped: u32,
    pub missing: u32,
    pub injured: u32,
    pub at_risk: u32,
    pub reports_with_people: usize,
    pub approximate: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BriefPriority {
    pub incident_id: String,
    pub excerpt: String,
    pub category: IncidentCategory,
    pub derived_severity: Severity,
    pub score: i32,
    pub reason: String,
    pub operator_severity: Severity,
    pub created_at: DateTime<Utc>,
}

/// A situation digest across the incidents this node holds.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SituationBrief {
    pub incidents_considered: usize,
    pub categories: Vec<CategoryCount>,
    pub derived_severity: Vec<SeverityCount>,
    pub people: PeopleTotals,
    pub blocked_routes: Vec<String>,
    pub priorities: Vec<BriefPriority>,
    /// Groups of incident IDs that read as the same event.
    pub duplicate_groups: Vec<Vec<String>>,
    pub semantic_available: bool,
    /// A model-written digest of the figures above, when one was asked for and
    /// passed the support check.
    pub summary: Option<String>,
    pub summary_model: Option<String>,
    pub summary_support: Option<f32>,
    /// Why there is no summary, when there is none.
    pub summary_note: Option<String>,
    pub elapsed_ms: u64,
}

const MAX_PRIORITIES: usize = 5;
const MAX_ROUTES: usize = 12;

/// Deterministic digest: counts, totals, priorities and duplicate groups.
pub fn build_brief(candidates: &[Candidate<'_>], elapsed_ms: u64) -> SituationBrief {
    let mut categories: Vec<CategoryCount> = Vec::new();
    let mut severities: Vec<SeverityCount> = Severity::ALL
        .iter()
        .rev()
        .map(|level| SeverityCount { level: *level, count: 0 })
        .collect();
    let mut people = PeopleTotals::default();
    let mut routes: Vec<String> = Vec::new();

    for candidate in candidates {
        let e = candidate.extraction;
        match categories.iter_mut().find(|c| c.category == e.category) {
            Some(entry) => entry.count += 1,
            None => categories.push(CategoryCount { category: e.category, count: 1 }),
        }
        if let Some(entry) = severities.iter_mut().find(|s| s.level == e.severity.level) {
            entry.count += 1;
        }

        let s = &e.people_summary;
        let mut any = false;
        for (value, total) in [
            (s.deceased, &mut people.deceased),
            (s.trapped, &mut people.trapped),
            (s.missing, &mut people.missing),
            (s.injured, &mut people.injured),
            (s.at_risk, &mut people.at_risk),
        ] {
            if let Some(n) = value {
                *total = total.saturating_add(n);
                any = true;
            }
        }
        if any {
            people.reports_with_people += 1;
            people.approximate |= s.approximate;
        }

        for route in e.routes.iter().filter(|r| r.blocked) {
            let key = route.text.to_lowercase();
            if routes.len() < MAX_ROUTES && !routes.iter().any(|r| r.to_lowercase() == key) {
                routes.push(route.text.clone());
            }
        }
    }
    categories.sort_by(|a, b| b.count.cmp(&a.count));

    let mut ranked: Vec<&Candidate<'_>> = candidates.iter().collect();
    ranked.sort_by(|a, b| {
        b.extraction
            .severity
            .score
            .cmp(&a.extraction.severity.score)
            .then(b.incident.severity.cmp(&a.incident.severity))
            .then(b.incident.created_at.cmp(&a.incident.created_at))
    });
    let priorities = ranked
        .iter()
        .take(MAX_PRIORITIES)
        .map(|c| BriefPriority {
            incident_id: c.incident.id.clone(),
            excerpt: excerpt(&c.incident.description),
            category: c.extraction.category,
            derived_severity: c.extraction.severity.level,
            score: c.extraction.severity.score,
            reason: c.extraction.severity.reason.clone(),
            operator_severity: c.incident.severity,
            created_at: c.incident.created_at,
        })
        .collect();

    SituationBrief {
        incidents_considered: candidates.len(),
        categories,
        derived_severity: severities,
        people,
        blocked_routes: routes,
        priorities,
        duplicate_groups: duplicate_groups(candidates),
        semantic_available: candidates.iter().any(|c| c.vector.is_some()),
        summary: None,
        summary_model: None,
        summary_support: None,
        summary_note: None,
        elapsed_ms,
    }
}

/// Connected groups of incidents whose pairwise relation is a duplicate.
fn duplicate_groups(candidates: &[Candidate<'_>]) -> Vec<Vec<String>> {
    let n = candidates.len();
    let mut parent: Vec<usize> = (0..n).collect();
    fn root(parent: &mut [usize], mut i: usize) -> usize {
        while parent[i] != i {
            parent[i] = parent[parent[i]];
            i = parent[i];
        }
        i
    }

    for i in 0..n {
        for j in (i + 1)..n {
            let (score, method) = similarity(&candidates[i], &candidates[j]);
            if relation_for(score, method) == Some(Relation::Duplicate) {
                let (a, b) = (root(&mut parent, i), root(&mut parent, j));
                if a != b {
                    parent[b] = a;
                }
            }
        }
    }

    let mut groups: Vec<(usize, Vec<String>)> = Vec::new();
    for i in 0..n {
        let r = root(&mut parent, i);
        let id = candidates[i].incident.id.clone();
        match groups.iter_mut().find(|(g, _)| *g == r) {
            Some((_, members)) => members.push(id),
            None => groups.push((r, vec![id])),
        }
    }
    groups
        .into_iter()
        .map(|(_, members)| members)
        .filter(|members| members.len() > 1)
        .collect()
}

/// The brief as compact numbered facts, for a model to summarise.
///
/// Bounded, and built only from the brief — the model sees these figures and
/// the priority excerpts, not the database.
pub fn brief_context(brief: &SituationBrief) -> String {
    let mut lines = Vec::new();
    lines.push(format!("Incidents held: {}", brief.incidents_considered));
    let categories: Vec<String> = brief
        .categories
        .iter()
        .map(|c| format!("{} {}", c.count, c.category.as_str().to_lowercase().replace('_', " ")))
        .collect();
    if !categories.is_empty() {
        lines.push(format!("By type: {}", categories.join(", ")));
    }
    let p = &brief.people;
    if p.reports_with_people > 0 {
        lines.push(format!(
            "People across reports: {} dead, {} trapped, {} missing, {} injured{}",
            p.deceased,
            p.trapped,
            p.missing,
            p.injured,
            if p.approximate { " (some figures approximate)" } else { "" }
        ));
    }
    if !brief.blocked_routes.is_empty() {
        lines.push(format!("Blocked routes: {}", brief.blocked_routes.join(", ")));
    }
    if !brief.duplicate_groups.is_empty() {
        lines.push(format!(
            "Reports that appear to describe the same event: {} group(s)",
            brief.duplicate_groups.len()
        ));
    }
    for (index, priority) in brief.priorities.iter().enumerate() {
        lines.push(format!(
            "Priority {}: [{}] {} — {}",
            index + 1,
            priority.derived_severity.as_str(),
            priority.excerpt,
            priority.reason
        ));
    }
    let joined = lines.join("\n");
    joined.chars().take(3_000).collect()
}

/// Share of a summary's content words found in the context it was given.
///
/// The same containment check grounded answers use: a summary that brings in
/// words the context never contained is drawing on the model, not the node's
/// records.
pub fn support_score(summary: &str, context: &str) -> f32 {
    let context = context.to_lowercase();
    let words: Vec<String> = summary
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 4)
        .map(str::to_string)
        .collect();
    if words.is_empty() {
        return 0.0;
    }
    let supported = words
        .iter()
        .filter(|w| {
            context.contains(w.as_str())
                || w.strip_suffix('s').is_some_and(|s| s.chars().count() >= 4 && context.contains(s))
                || (w.chars().count() >= 6 && context.contains(&stem(w)))
        })
        .count();
    supported as f32 / words.len() as f32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::nlp::extract;
    use crate::domain::NewIncident;

    const NODE: &str = "a7f32c9e00000000000000000000000000000000000000000000000000000000";

    fn incident(description: &str, severity: &str, position: Option<(f64, f64)>) -> Incident {
        NewIncident {
            description: description.to_string(),
            severity: severity.to_string(),
            latitude: position.map(|p| p.0),
            longitude: position.map(|p| p.1),
            accuracy_meters: None,
            location_source: None,
            location_captured_at: None,
        }
        .validate(NODE)
        .unwrap()
    }

    fn vector(values: &[f32]) -> Embedding {
        Embedding::new(values.to_vec(), "test-embed").unwrap()
    }

    #[test]
    fn restated_reports_are_duplicates_by_vector() {
        let a = incident("Fire at the market", "HIGH", Some((12.97, 77.59)));
        let b = incident("Market is on fire", "HIGH", Some((12.971, 77.591)));
        let c = incident("Road blocked by a tree", "LOW", None);
        let (ea, eb, ec) = (extract(&a.description), extract(&b.description), extract(&c.description));
        let (va, vb, vc) = (vector(&[1.0, 0.0]), vector(&[0.99, 0.05]), vector(&[0.0, 1.0]));

        let target = Candidate { incident: &a, extraction: &ea, vector: Some(&va) };
        let others = [
            Candidate { incident: &b, extraction: &eb, vector: Some(&vb) },
            Candidate { incident: &c, extraction: &ec, vector: Some(&vc) },
        ];
        let related = related_incidents(&target, &others);

        assert_eq!(related.len(), 1);
        assert_eq!(related[0].incident_id, b.id);
        assert_eq!(related[0].relation, Relation::Duplicate);
        assert_eq!(related[0].method, Method::Semantic);
        assert!(related[0].same_category);
        assert!(related[0].distance_km.unwrap() < 1.0);
        assert!(related[0].shared_hazards.contains(&Hazard::Fire));
    }

    #[test]
    fn without_vectors_matching_falls_back_to_words_and_hazards() {
        let a = incident("Two people injured in a bus collision on the highway", "HIGH", None);
        let b = incident("Bus collision on highway, two injured", "HIGH", None);
        let c = incident("Power outage across the district", "LOW", None);
        let (ea, eb, ec) = (extract(&a.description), extract(&b.description), extract(&c.description));

        let target = Candidate { incident: &a, extraction: &ea, vector: None };
        let others = [
            Candidate { incident: &b, extraction: &eb, vector: None },
            Candidate { incident: &c, extraction: &ec, vector: None },
        ];
        let related = related_incidents(&target, &others);

        assert_eq!(related.len(), 1, "{related:?}");
        assert_eq!(related[0].incident_id, b.id);
        assert_eq!(related[0].method, Method::Lexical);
    }

    #[test]
    fn an_incident_is_never_related_to_itself() {
        let a = incident("Fire", "HIGH", None);
        let ea = extract(&a.description);
        let target = Candidate { incident: &a, extraction: &ea, vector: None };
        let same = [Candidate { incident: &a, extraction: &ea, vector: None }];
        assert!(related_incidents(&target, &same).is_empty());
    }

    #[test]
    fn vectors_from_different_models_are_not_compared() {
        let a = incident("Fire at the depot", "HIGH", None);
        let b = incident("Flooding in the north", "HIGH", None);
        let (ea, eb) = (extract(&a.description), extract(&b.description));
        let va = Embedding::new(vec![1.0, 0.0], "model-a").unwrap();
        let vb = Embedding::new(vec![1.0, 0.0], "model-b").unwrap();
        let target = Candidate { incident: &a, extraction: &ea, vector: Some(&va) };
        let others = [Candidate { incident: &b, extraction: &eb, vector: Some(&vb) }];
        // Identical vectors, but incomparable: lexical, and unrelated.
        assert!(related_incidents(&target, &others).is_empty());
    }

    #[test]
    fn the_semantic_fallback_needs_a_clear_winner() {
        let prototypes = vec![
            (IncidentCategory::Fire, vector(&[1.0, 0.0, 0.0])),
            (IncidentCategory::Flooding, vector(&[0.0, 1.0, 0.0])),
            (IncidentCategory::Other, vector(&[1.0, 0.0, 0.0])),
        ];
        let near_fire = semantic_category(&vector(&[0.9, 0.1, 0.0]), &prototypes).unwrap();
        assert_eq!(near_fire.category, IncidentCategory::Fire);

        // Equidistant: no clear winner, so no guess.
        assert!(semantic_category(&vector(&[1.0, 1.0, 0.0]), &prototypes).is_none());
        // Far from everything.
        assert!(semantic_category(&vector(&[0.0, 0.0, 1.0]), &prototypes).is_none());
    }

    #[test]
    fn rules_win_when_confident_and_the_fallback_fills_gaps() {
        let confident = extract("Major fire outbreak at the warehouse");
        let semantic = Some(SemanticCategory { category: IncidentCategory::Flooding, similarity: 0.7 });
        assert_eq!(choose_category(&confident, semantic), (IncidentCategory::Fire, CategoryMethod::Lexical));

        let blank = extract("The dam gave out overnight and the valley is under water");
        let chosen = choose_category(&blank, semantic);
        assert_ne!(chosen.1, CategoryMethod::None);

        let nothing = extract("Something happened");
        assert_eq!(choose_category(&nothing, None), (IncidentCategory::Other, CategoryMethod::None));
    }

    #[test]
    fn corroborating_reports_raise_derived_severity_but_duplicates_do_not() {
        let a = incident("Flooding in Zone A", "MEDIUM", None);
        let ea = extract(&a.description);
        let base = ea.severity.score;

        let b = incident("Water rising in Zone A", "MEDIUM", None);
        let c = incident("Zone A streets under water", "MEDIUM", None);
        let (eb, ec) = (extract(&b.description), extract(&c.description));
        let (va, vb, vc) = (vector(&[1.0, 0.0]), vector(&[0.8, 0.6]), vector(&[0.8, -0.6]));

        let target = Candidate { incident: &a, extraction: &ea, vector: Some(&va) };
        let others = [
            Candidate { incident: &b, extraction: &eb, vector: Some(&vb) },
            Candidate { incident: &c, extraction: &ec, vector: Some(&vc) },
        ];
        let insight = build_insight(&target, &others, None, None, true, 0);
        assert_eq!(insight.related.len(), 2);
        assert_eq!(insight.extraction.severity.score, base + 1);

        // The operator's record is not what changed.
        assert_eq!(a.severity, Severity::Medium);
    }

    #[test]
    fn the_brief_counts_prioritises_and_groups() {
        let a = incident("Around 5 people trapped after the building collapsed, road blocked", "HIGH", None);
        let b = incident("Five people trapped in the collapsed building", "HIGH", None);
        let c = incident("Minor power outage, stable", "LOW", None);
        let (ea, eb, ec) = (extract(&a.description), extract(&b.description), extract(&c.description));
        let (va, vb, vc) = (vector(&[1.0, 0.0]), vector(&[0.999, 0.01]), vector(&[0.0, 1.0]));
        let candidates = [
            Candidate { incident: &a, extraction: &ea, vector: Some(&va) },
            Candidate { incident: &b, extraction: &eb, vector: Some(&vb) },
            Candidate { incident: &c, extraction: &ec, vector: Some(&vc) },
        ];

        let brief = build_brief(&candidates, 0);
        assert_eq!(brief.incidents_considered, 3);
        assert_eq!(brief.people.trapped, 10, "summed, with duplicates flagged separately");
        assert_eq!(brief.duplicate_groups.len(), 1);
        assert_eq!(brief.duplicate_groups[0].len(), 2);
        assert_ne!(brief.priorities[0].incident_id, c.id, "the outage is not the top priority");
        assert!(!brief.blocked_routes.is_empty() || ea.any_route_blocked());

        let context = brief_context(&brief);
        assert!(context.contains("trapped"));
        assert!(context.chars().count() <= 3_000);
    }

    #[test]
    fn support_catches_a_summary_that_invents() {
        let context = "Incidents held: 3\nPeople across reports: 0 dead, 10 trapped\nBlocked routes: eastern road";
        assert!(support_score("Ten people trapped; eastern road blocked.", context) > 0.5);
        assert!(support_score("Paris is the capital of France.", context) < 0.2);
    }

    #[test]
    fn haversine_is_sane() {
        let bengaluru = (12.9716, 77.5946);
        let mysuru = (12.2958, 76.6394);
        let d = haversine_km(bengaluru, mysuru);
        assert!((120.0..135.0).contains(&d), "{d}");
        assert!(haversine_km(bengaluru, bengaluru) < 1e-9);
    }
}
