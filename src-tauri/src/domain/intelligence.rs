//! Derived intelligence: what a local model inferred about an incident.
//!
//! # The authoritative record is never the model
//!
//! Incidents and their signed events remain the source of truth. Everything
//! here is *derived* — a local interpretation, attributable to a named model at
//! a known time, and discardable without losing anything. Deleting every row of
//! derived intelligence must leave the operational record intact.
//!
//! That is why analyses are **not** replicated as mesh events: an inference is
//! a local opinion, not a fact about the world, and two nodes running different
//! models will legitimately disagree.
//!
//! # Model output is untrusted input
//!
//! A language model is treated exactly like a peer on the network: it produces
//! bytes that *claim* to be structured data. The pipeline is deliberate:
//!
//! ```text
//!   model output ──▶ parse ──▶ validate ──▶ normalise ──▶ store as derived
//!                      │          │
//!                      └──────────┴──▶ rejected, with a reason recorded
//! ```
//!
//! [`RawAnalysis`] is the untrusted shape and cannot be stored. Only
//! [`IncidentAnalysis`], which exists solely as the output of
//! [`RawAnalysis::validate`], can. This mirrors `NewIncident` → `Incident`
//! exactly, for the same reason: making the invalid state unrepresentable is
//! more reliable than remembering to check.

use crate::error::{CoreError, CoreResult};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// Longest accepted free-text field from a model, in characters.
///
/// A model that runs away producing text must not be able to write an unbounded
/// row, so every generated string is bounded before it reaches storage.
pub const MAX_SUMMARY_CHARS: usize = 1_000;
pub const MAX_FIELD_CHARS: usize = 200;
/// Most entities accepted from a single analysis.
pub const MAX_ENTITIES: usize = 32;

/// The operational category a model assigned to an incident.
///
/// A closed set. An open string field would let a model invent categories,
/// which makes the field useless for filtering and impossible to evaluate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum IncidentCategory {
    Infrastructure,
    Flooding,
    Fire,
    Medical,
    Evacuation,
    Power,
    Communications,
    ResourceShortage,
    Earthquake,
    SevereWeather,
    RoadBlockage,
    /// The model could not place it, or placed it somewhere unrecognised.
    Other,
}

impl IncidentCategory {
    pub const ALL: [IncidentCategory; 12] = [
        IncidentCategory::Infrastructure,
        IncidentCategory::Flooding,
        IncidentCategory::Fire,
        IncidentCategory::Medical,
        IncidentCategory::Evacuation,
        IncidentCategory::Power,
        IncidentCategory::Communications,
        IncidentCategory::ResourceShortage,
        IncidentCategory::Earthquake,
        IncidentCategory::SevereWeather,
        IncidentCategory::RoadBlockage,
        IncidentCategory::Other,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            IncidentCategory::Infrastructure => "INFRASTRUCTURE",
            IncidentCategory::Flooding => "FLOODING",
            IncidentCategory::Fire => "FIRE",
            IncidentCategory::Medical => "MEDICAL",
            IncidentCategory::Evacuation => "EVACUATION",
            IncidentCategory::Power => "POWER",
            IncidentCategory::Communications => "COMMUNICATIONS",
            IncidentCategory::ResourceShortage => "RESOURCE_SHORTAGE",
            IncidentCategory::Earthquake => "EARTHQUAKE",
            IncidentCategory::SevereWeather => "SEVERE_WEATHER",
            IncidentCategory::RoadBlockage => "ROAD_BLOCKAGE",
            IncidentCategory::Other => "OTHER",
        }
    }

    /// What the category names, in one line.
    ///
    /// The identifiers are not self-explanatory — `INFRASTRUCTURE` and
    /// `ROAD_BLOCKAGE` both cover a blocked route, and nothing in the name says
    /// which. These lines are given to the model alongside the schema for the
    /// same reason they would be given to a new operator.
    ///
    /// Kept next to the variants so a category added to [`Self::ALL`] cannot
    /// reach the model undescribed.
    pub fn description(self) -> &'static str {
        match self {
            IncidentCategory::Infrastructure => {
                "damage to a built structure — bridge, wall, building, culvert"
            }
            IncidentCategory::Flooding => {
                "water where it should not be — rising, standing, or burst"
            }
            IncidentCategory::Fire => "active fire, smoke, or flames",
            IncidentCategory::Medical => "casualties, injuries, or a request for medical support",
            IncidentCategory::Evacuation => "people being moved, sheltered, or assembled",
            IncidentCategory::Power => "loss of electricity supply or damage to its equipment",
            IncidentCategory::Communications => "loss of radio, mobile, or network contact",
            IncidentCategory::ResourceShortage => {
                "supplies running out — water, fuel, rations, medical stock"
            }
            IncidentCategory::Earthquake => "tremor, aftershock, or seismic damage",
            IncidentCategory::SevereWeather => "storm, high wind, or hail as the reported hazard",
            IncidentCategory::RoadBlockage => {
                "a route obstructed by debris, landslip, or a fallen tree"
            }
            IncidentCategory::Other => "none of the above fits",
        }
    }

    /// The values offered to the model in its output schema.
    pub fn schema_values() -> Vec<String> {
        Self::ALL.iter().map(|c| c.as_str().to_string()).collect()
    }

    /// The category list as given to the model, one per line.
    pub fn described_list() -> String {
        Self::ALL
            .iter()
            .map(|category| format!("- {}: {}", category.as_str(), category.description()))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

impl fmt::Display for IncidentCategory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for IncidentCategory {
    type Err = CoreError;

    fn from_str(value: &str) -> CoreResult<Self> {
        // Tolerant of case and separator: a model asked for SCREAMING_SNAKE_CASE
        // will occasionally answer "road blockage". Coercing that to the right
        // variant is more useful than discarding an otherwise good analysis,
        // and it is still a closed set — anything unrecognised becomes `Other`
        // rather than a new category.
        let normalised = value.trim().to_ascii_uppercase().replace([' ', '-'], "_");

        match normalised.as_str() {
            "INFRASTRUCTURE" => Ok(IncidentCategory::Infrastructure),
            "FLOODING" | "FLOOD" => Ok(IncidentCategory::Flooding),
            "FIRE" => Ok(IncidentCategory::Fire),
            "MEDICAL" => Ok(IncidentCategory::Medical),
            "EVACUATION" => Ok(IncidentCategory::Evacuation),
            "POWER" | "POWER_OUTAGE" => Ok(IncidentCategory::Power),
            "COMMUNICATIONS" | "COMMUNICATION" => Ok(IncidentCategory::Communications),
            "RESOURCE_SHORTAGE" | "RESOURCE" => Ok(IncidentCategory::ResourceShortage),
            "EARTHQUAKE" => Ok(IncidentCategory::Earthquake),
            "SEVERE_WEATHER" | "WEATHER" => Ok(IncidentCategory::SevereWeather),
            "ROAD_BLOCKAGE" | "ROAD" => Ok(IncidentCategory::RoadBlockage),
            _ => Ok(IncidentCategory::Other),
        }
    }
}

/// How reachable the model judged the affected area to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AccessStatus {
    Open,
    Restricted,
    Blocked,
    Unknown,
}

impl AccessStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            AccessStatus::Open => "OPEN",
            AccessStatus::Restricted => "RESTRICTED",
            AccessStatus::Blocked => "BLOCKED",
            AccessStatus::Unknown => "UNKNOWN",
        }
    }

    pub fn schema_values() -> Vec<String> {
        ["OPEN", "RESTRICTED", "BLOCKED", "UNKNOWN"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    }
}

impl fmt::Display for AccessStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for AccessStatus {
    type Err = CoreError;

    fn from_str(value: &str) -> CoreResult<Self> {
        match value.trim().to_ascii_uppercase().as_str() {
            "OPEN" => Ok(AccessStatus::Open),
            "RESTRICTED" | "PARTIAL" => Ok(AccessStatus::Restricted),
            "BLOCKED" | "CLOSED" => Ok(AccessStatus::Blocked),
            // Unrecognised means unknown, never an invented status.
            _ => Ok(AccessStatus::Unknown),
        }
    }
}

/// **Untrusted.** The shape a model is asked to produce.
///
/// Every field is optional and every type is permissive, because this describes
/// what a model *might* emit, not what SecureMesh accepts. It has no
/// constructor and no path to storage other than [`Self::validate`].
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawAnalysis {
    pub category: Option<String>,
    pub severity: Option<String>,
    pub summary: Option<String>,
    pub asset: Option<String>,
    pub cause: Option<String>,
    pub access_status: Option<String>,
    #[serde(default)]
    pub entities: Vec<String>,
    #[serde(default)]
    pub affected_resources: Vec<String>,
    pub location_hint: Option<String>,
    /// **Accepted on the wire, never propagated.** The field is kept so that
    /// output or stored JSON carrying a confidence still deserialises rather
    /// than being refused by `deny_unknown_fields`, but [`Self::validate`]
    /// discards it. The model is no longer asked for it either — it is absent
    /// from `ai::prompt::analysis_schema`.
    pub confidence: Option<f64>,
}

impl RawAnalysis {
    /// Turns untrusted model output into a storable analysis, or rejects it.
    ///
    /// Rules, in the order they are applied:
    ///
    /// - a summary is **required** — an analysis with nothing to say is not
    ///   worth storing, and its absence usually means the model failed;
    /// - free text is trimmed and length-bounded;
    /// - closed sets fall back to `Other`/`Unknown` rather than failing, so one
    ///   odd enum value does not discard an otherwise usable analysis;
    /// - severity must parse, since it drives operational display and a wrong
    ///   guess there is worse than no answer;
    /// - confidence is **discarded**. It used to be clamped to `0.0..=1.0`,
    ///   which made a model saying `2` and a model saying `95` both arrive as
    ///   `1.0` and display as "100%" — the least confident answers presenting
    ///   as the most. The training targets contain no `confidence` field, so
    ///   there is no scale behind the number to recover. An honest absence is
    ///   worth more to an operator than a confident-looking invention.
    pub fn validate(
        self,
        incident_id: &str,
        model_id: &str,
        latency_ms: u64,
    ) -> CoreResult<IncidentAnalysis> {
        let summary = bounded_text(self.summary.as_deref(), MAX_SUMMARY_CHARS)
            .ok_or_else(|| CoreError::validation("model produced no usable summary"))?;

        if incident_id.trim().is_empty() {
            return Err(CoreError::validation("analysis must reference an incident"));
        }
        if model_id.trim().is_empty() {
            return Err(CoreError::validation("analysis must name the model"));
        }

        let severity = self
            .severity
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| CoreError::validation("model produced no severity"))?
            .parse::<crate::domain::Severity>()?;

        let category = self
            .category
            .as_deref()
            .map(|c| c.parse::<IncidentCategory>())
            .transpose()?
            .unwrap_or(IncidentCategory::Other);

        let access_status = self
            .access_status
            .as_deref()
            .map(|a| a.parse::<AccessStatus>())
            .transpose()?
            .unwrap_or(AccessStatus::Unknown);

        Ok(IncidentAnalysis {
            incident_id: incident_id.to_string(),
            category,
            severity,
            summary,
            asset: bounded_text(self.asset.as_deref(), MAX_FIELD_CHARS),
            cause: bounded_text(self.cause.as_deref(), MAX_FIELD_CHARS),
            access_status,
            entities: bounded_list(self.entities),
            affected_resources: bounded_list(self.affected_resources),
            location_hint: bounded_text(self.location_hint.as_deref(), MAX_FIELD_CHARS),
            // Deliberately discarded. See the note on `RawAnalysis::confidence`:
            // the model has no calibrated confidence to give, and clamping what
            // it does give turned "2" and "95" alike into a displayed 100%.
            confidence: None,
            model_id: model_id.to_string(),
            latency_ms,
            generated_at: crate::domain::now(),
        })
    }
}

/// Trims, drops if empty, and truncates on a character boundary.
fn bounded_text(value: Option<&str>, limit: usize) -> Option<String> {
    let trimmed = value?.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.chars().take(limit).collect())
}

/// Bounds a list of model-supplied strings in both length and count.
fn bounded_list(values: Vec<String>) -> Vec<String> {
    values
        .into_iter()
        .filter_map(|v| bounded_text(Some(&v), MAX_FIELD_CHARS))
        .take(MAX_ENTITIES)
        .collect()
}

/// **Validated.** Derived intelligence about one incident.
///
/// Only obtainable from [`RawAnalysis::validate`], so possession of this type
/// is itself evidence that the model's output passed every check.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IncidentAnalysis {
    pub incident_id: String,
    pub category: IncidentCategory,
    /// The severity the *model* assigned. Deliberately separate from the
    /// incident's own severity, which the operator set — the two disagreeing is
    /// useful information, so the model never overwrites the human.
    pub severity: crate::domain::Severity,
    pub summary: String,
    pub asset: Option<String>,
    pub cause: Option<String>,
    pub access_status: AccessStatus,
    pub entities: Vec<String>,
    pub affected_resources: Vec<String>,
    pub location_hint: Option<String>,
    /// **Always `None` for analyses produced now.** [`RawAnalysis::validate`]
    /// discards whatever the model states; rows written before that change may
    /// still hold a clamped value, which is why the field and its stored column
    /// remain. Never presented to an operator as a number.
    pub confidence: Option<f64>,
    /// Which model produced this, so an analysis can be re-run or discarded
    /// when the model changes.
    pub model_id: String,
    /// How long the inference took, for the benchmark record.
    pub latency_ms: u64,
    pub generated_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Severity;

    fn raw() -> RawAnalysis {
        RawAnalysis {
            category: Some("INFRASTRUCTURE".to_string()),
            severity: Some("HIGH".to_string()),
            summary: Some("Bridge partially collapsed after rainfall.".to_string()),
            asset: Some("bridge".to_string()),
            cause: Some("heavy rainfall".to_string()),
            access_status: Some("BLOCKED".to_string()),
            entities: vec!["2 vehicles".to_string()],
            affected_resources: vec!["eastern road".to_string()],
            location_hint: Some("northern zone".to_string()),
            confidence: Some(0.8),
        }
    }

    #[test]
    fn a_well_formed_analysis_validates() {
        let analysis = raw().validate("inc-1", "qwen2.5-1.5b", 2600).unwrap();

        assert_eq!(analysis.category, IncidentCategory::Infrastructure);
        assert_eq!(analysis.severity, Severity::High);
        assert_eq!(analysis.access_status, AccessStatus::Blocked);
        assert_eq!(analysis.incident_id, "inc-1");
        assert_eq!(analysis.model_id, "qwen2.5-1.5b");
        assert_eq!(analysis.latency_ms, 2600);
    }

    // --- Required fields ---------------------------------------------------

    #[test]
    fn an_analysis_with_no_summary_is_rejected() {
        for empty in [None, Some(String::new()), Some("   ".to_string())] {
            let mut candidate = raw();
            candidate.summary = empty;
            let err = candidate.validate("inc-1", "m", 0).unwrap_err();
            assert_eq!(err.code(), "VALIDATION_ERROR");
        }
    }

    #[test]
    fn an_analysis_with_no_severity_is_rejected() {
        let mut candidate = raw();
        candidate.severity = None;
        assert!(candidate.validate("inc-1", "m", 0).is_err());
    }

    #[test]
    fn an_analysis_must_reference_an_incident_and_a_model() {
        assert!(raw().validate("  ", "m", 0).is_err());
        assert!(raw().validate("inc-1", "  ", 0).is_err());
    }

    #[test]
    fn an_invented_severity_is_rejected_rather_than_guessed() {
        // Severity drives operational display, so a wrong guess is worse than
        // refusing the analysis.
        let mut candidate = raw();
        candidate.severity = Some("APOCALYPTIC".to_string());
        assert!(candidate.validate("inc-1", "m", 0).is_err());
    }

    // --- Closed sets degrade rather than fail ------------------------------

    #[test]
    fn an_invented_category_becomes_other() {
        let mut candidate = raw();
        candidate.category = Some("ALIEN_INVASION".to_string());

        let analysis = candidate.validate("inc-1", "m", 0).unwrap();
        assert_eq!(analysis.category, IncidentCategory::Other);
    }

    #[test]
    fn category_parsing_tolerates_model_formatting() {
        assert_eq!(
            "road blockage".parse::<IncidentCategory>().unwrap(),
            IncidentCategory::RoadBlockage
        );
        assert_eq!(
            "Power-Outage".parse::<IncidentCategory>().unwrap(),
            IncidentCategory::Power
        );
        assert_eq!(
            "flood".parse::<IncidentCategory>().unwrap(),
            IncidentCategory::Flooding
        );
    }

    #[test]
    fn an_invented_access_status_becomes_unknown() {
        let mut candidate = raw();
        candidate.access_status = Some("PROBABLY_FINE".to_string());

        let analysis = candidate.validate("inc-1", "m", 0).unwrap();
        assert_eq!(analysis.access_status, AccessStatus::Unknown);
    }

    #[test]
    fn a_missing_category_defaults_to_other_not_to_a_guess() {
        let mut candidate = raw();
        candidate.category = None;

        let analysis = candidate.validate("inc-1", "m", 0).unwrap();
        assert_eq!(analysis.category, IncidentCategory::Other);
    }

    // --- Bounds ------------------------------------------------------------

    #[test]
    fn a_runaway_summary_is_truncated_not_stored_whole() {
        let mut candidate = raw();
        candidate.summary = Some("x".repeat(MAX_SUMMARY_CHARS * 10));

        let analysis = candidate.validate("inc-1", "m", 0).unwrap();
        assert_eq!(analysis.summary.chars().count(), MAX_SUMMARY_CHARS);
    }

    #[test]
    fn an_unbounded_entity_list_is_capped() {
        let mut candidate = raw();
        candidate.entities = (0..500).map(|n| format!("entity {n}")).collect();

        let analysis = candidate.validate("inc-1", "m", 0).unwrap();
        assert_eq!(analysis.entities.len(), MAX_ENTITIES);
    }

    #[test]
    fn empty_entities_are_dropped_rather_than_stored_blank() {
        let mut candidate = raw();
        candidate.entities = vec!["  ".to_string(), "real".to_string(), String::new()];

        let analysis = candidate.validate("inc-1", "m", 0).unwrap();
        assert_eq!(analysis.entities, vec!["real".to_string()]);
    }

    #[test]
    fn truncation_respects_character_boundaries() {
        // A byte-based truncation would panic or corrupt multi-byte text.
        let mut candidate = raw();
        candidate.summary = Some("बाढ़".repeat(2000));

        let analysis = candidate.validate("inc-1", "m", 0).unwrap();
        assert_eq!(analysis.summary.chars().count(), MAX_SUMMARY_CHARS);
    }

    // --- Confidence --------------------------------------------------------

    #[test]
    fn no_stated_confidence_survives_validation_whatever_its_scale() {
        // The defect this replaces: confidence was clamped to 0.0..=1.0, so a
        // model saying 1, 2 or 3 and a model saying 90, 95 or 100 all produced
        // exactly 1.0, which the UI rendered as "100%". The least confident
        // outputs displayed as maximum confidence.
        //
        // Measured on fresh reports, the candidate and stock models emitted
        // every one of these values, and the training targets contain no
        // `confidence` field at all (0 of 1052 train, 0 of 214 val), so there
        // is no convention behind any of them.
        for given in [
            0.0, 0.5, 0.75, 0.9, 0.95, 1.0, // the 0..1 scale it was assumed to use
            1.0, 2.0, 3.0, 50.0, 80.0, 90.0, 95.0, 100.0, // what it actually emits
            -1.0, -2.0, 1e9, // out of range in both directions
        ] {
            let mut candidate = raw();
            candidate.confidence = Some(given);
            let analysis = candidate.validate("inc-1", "m", 0).unwrap();
            assert_eq!(
                analysis.confidence, None,
                "confidence {given} must not survive validation"
            );
        }
    }

    #[test]
    fn a_missing_confidence_is_still_a_valid_analysis() {
        let mut candidate = raw();
        candidate.confidence = None;
        let analysis = candidate.validate("inc-1", "m", 0).unwrap();
        assert_eq!(analysis.confidence, None);
    }

    #[test]
    fn a_non_finite_confidence_is_discarded_rather_than_stored_or_panicking() {
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let mut candidate = raw();
            candidate.confidence = Some(bad);
            let analysis = candidate.validate("inc-1", "m", 0).unwrap();
            assert_eq!(analysis.confidence, None);
        }
    }

    #[test]
    fn a_malformed_confidence_is_refused_at_parse_rather_than_coerced() {
        // A non-numeric confidence must not be silently read as 0 or dropped:
        // it means the output is not the shape SecureMesh asked for.
        for bad in [
            r#"{"summary":"ok","severity":"HIGH","confidence":"high"}"#,
            r#"{"summary":"ok","severity":"HIGH","confidence":"0.95"}"#,
            r#"{"summary":"ok","severity":"HIGH","confidence":[0.9]}"#,
            r#"{"summary":"ok","severity":"HIGH","confidence":{"value":0.9}}"#,
        ] {
            assert!(
                serde_json::from_str::<RawAnalysis>(bad).is_err(),
                "should refuse: {bad}"
            );
        }
    }

    #[test]
    fn the_model_is_never_asked_for_a_confidence_it_cannot_calibrate() {
        // The schema and validation have to agree: asking for a field that is
        // then always discarded would spend tokens to produce nothing, and
        // would leave the unbounded property in place for a future caller to
        // start trusting again.
        let schema = crate::ai::prompt::analysis_schema();
        assert!(
            schema["properties"]["confidence"].is_null(),
            "confidence must not be offered to the model"
        );
        assert_eq!(schema["additionalProperties"], false);
    }

    // --- Deserialisation of hostile model output ---------------------------

    #[test]
    fn model_output_with_unexpected_fields_is_refused() {
        // `deny_unknown_fields` stops a model smuggling extra keys — including
        // ones that might later be mistaken for a field SecureMesh trusts.
        let json = r#"{"summary":"ok","severity":"HIGH","node_id":"attacker"}"#;
        assert!(serde_json::from_str::<RawAnalysis>(json).is_err());
    }

    #[test]
    fn malformed_model_output_is_an_error_not_a_panic() {
        for bad in [
            "",
            "not json",
            "{",
            "[]",
            "null",
            r#"{"summary": 42}"#,
            r#"{"entities": "not a list"}"#,
            r#"{"confidence": "high"}"#,
        ] {
            assert!(
                serde_json::from_str::<RawAnalysis>(bad).is_err(),
                "should reject: {bad}"
            );
        }
    }

    #[test]
    fn a_minimal_valid_response_is_accepted() {
        // Only the two required fields; a small model often gives no more.
        let json = r#"{"summary":"Road blocked.","severity":"MEDIUM"}"#;
        let parsed: RawAnalysis = serde_json::from_str(json).unwrap();

        let analysis = parsed.validate("inc-1", "m", 10).unwrap();
        assert_eq!(analysis.severity, Severity::Medium);
        assert_eq!(analysis.category, IncidentCategory::Other);
        assert!(analysis.entities.is_empty());
        assert_eq!(analysis.confidence, None);
    }

    #[test]
    fn every_category_round_trips_through_its_wire_form() {
        for category in IncidentCategory::ALL {
            assert_eq!(
                category.as_str().parse::<IncidentCategory>().unwrap(),
                category
            );
        }
        assert_eq!(IncidentCategory::schema_values().len(), 12);
    }
}
