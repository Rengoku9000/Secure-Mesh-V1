//! Incidents: the primary operational record a SecureMesh node creates,
//! stores, and (from Phase 2) synchronises with peers.

use crate::error::{CoreError, CoreResult};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;
use uuid::Uuid;

/// Longest accepted incident description, in characters.
///
/// Bounded so that a single record cannot exhaust storage or make the sync
/// payloads of Phase 2 unbounded.
pub const MAX_DESCRIPTION_CHARS: usize = 2_000;

/// Operational urgency of an incident.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Severity {
    Low,
    Medium,
    High,
    Critical,
}

impl Severity {
    pub const ALL: [Severity; 4] = [
        Severity::Low,
        Severity::Medium,
        Severity::High,
        Severity::Critical,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Low => "LOW",
            Severity::Medium => "MEDIUM",
            Severity::High => "HIGH",
            Severity::Critical => "CRITICAL",
        }
    }
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Severity {
    type Err = CoreError;

    fn from_str(value: &str) -> CoreResult<Self> {
        match value.trim().to_ascii_uppercase().as_str() {
            "LOW" => Ok(Severity::Low),
            "MEDIUM" => Ok(Severity::Medium),
            "HIGH" => Ok(Severity::High),
            "CRITICAL" => Ok(Severity::Critical),
            _ => Err(CoreError::validation(
                "severity must be one of LOW, MEDIUM, HIGH, CRITICAL",
            )),
        }
    }
}

/// Whether a record has been propagated to peers.
///
/// Phase 1 only ever produces `Pending`, because there is no networking yet.
/// The remaining variants exist so the Phase 2 sync engine does not require a
/// schema migration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum SyncStatus {
    /// Created locally, not yet offered to any peer.
    Pending,
    /// Handed to the sync engine, delivery not yet confirmed.
    Syncing,
    /// Confirmed present on at least one peer.
    Synced,
    /// Delivery failed; the sync engine will retry.
    Failed,
}

impl SyncStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            SyncStatus::Pending => "PENDING",
            SyncStatus::Syncing => "SYNCING",
            SyncStatus::Synced => "SYNCED",
            SyncStatus::Failed => "FAILED",
        }
    }
}

impl fmt::Display for SyncStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for SyncStatus {
    type Err = CoreError;

    fn from_str(value: &str) -> CoreResult<Self> {
        match value.trim().to_ascii_uppercase().as_str() {
            "PENDING" => Ok(SyncStatus::Pending),
            "SYNCING" => Ok(SyncStatus::Syncing),
            "SYNCED" => Ok(SyncStatus::Synced),
            "FAILED" => Ok(SyncStatus::Failed),
            _ => Err(CoreError::storage(format!(
                "database holds an unrecognised sync status: {value}"
            ))),
        }
    }
}

/// Where a position came from.
///
/// Provenance is recorded because it is the difference between a coordinate a
/// responder can act on and one they cannot. The three variants are the
/// distinctions that change a decision; finer detail about which constellation
/// or which access point produced a fix does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum LocationSource {
    /// A satellite fix — GPS, GLONASS, Galileo or BeiDou. The only source that
    /// works with no network of any kind.
    #[serde(alias = "SATELLITE")]
    Gnss,
    /// Derived by the operating system from nearby Wi-Fi or cellular
    /// infrastructure. Usually accurate to tens or hundreds of metres, and on
    /// most platforms it requires the OS to reach a lookup service, so it is
    /// not an offline capability.
    Wireless,
    /// Provenance was not recorded: a coordinate typed by hand, a reading whose
    /// source the platform would not name, or an incident written by a build
    /// that predates this field.
    ///
    /// An IP-address-derived position also lands here. The platform layer can
    /// tell that case apart and the capture panel shows it, but the record does
    /// not keep the distinction: an IP lookup is a city-sized guess, and the
    /// honest thing for a stored coordinate to say is that its provenance is
    /// not something to rely on. The accuracy figure carries the magnitude —
    /// such a reading arrives as UNKNOWN with a radius in the tens of
    /// kilometres, which is exactly what it is.
    #[default]
    #[serde(alias = "IP_ADDRESS")]
    Unknown,
}

impl LocationSource {
    pub const ALL: [LocationSource; 3] = [
        LocationSource::Gnss,
        LocationSource::Wireless,
        LocationSource::Unknown,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            LocationSource::Gnss => "GNSS",
            LocationSource::Wireless => "WIRELESS",
            LocationSource::Unknown => "UNKNOWN",
        }
    }

    /// Whether a position from this source could have been obtained with no
    /// network reachable.
    ///
    /// Only satellite positioning qualifies. This is the honesty check that
    /// stops SecureMesh describing a Wi-Fi-derived coordinate as evidence of
    /// offline capability.
    pub fn works_offline(self) -> bool {
        matches!(self, LocationSource::Gnss)
    }
}

impl fmt::Display for LocationSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for LocationSource {
    type Err = CoreError;

    fn from_str(value: &str) -> CoreResult<Self> {
        match value.trim().to_ascii_uppercase().as_str() {
            // The aliases accept the platform layer's vocabulary, so the
            // mapping between the two lives here and nowhere else. Both the UI
            // and a peer's payload are parsed by this one function.
            "GNSS" | "SATELLITE" => Ok(LocationSource::Gnss),
            "WIRELESS" => Ok(LocationSource::Wireless),
            "UNKNOWN" | "IP_ADDRESS" => Ok(LocationSource::Unknown),
            _ => Err(CoreError::validation(
                "location source must be one of GNSS, WIRELESS, UNKNOWN",
            )),
        }
    }
}

/// Largest accepted accuracy radius, in metres.
///
/// Half the Earth's circumference: past this the figure is not a measurement of
/// anything. The bound rejects nonsense, not poor fixes — a genuinely bad
/// reading should arrive intact and be shown as bad.
pub const MAX_ACCURACY_METERS: f64 = 20_000_000.0;

/// A geographic position, in WGS 84 decimal degrees, with the provenance needed
/// to judge it.
///
/// This is the only place coordinate rules live. Both the local command path
/// and events arriving from peers construct one through [`Location::new`], so a
/// remote node cannot write a position a local operator would have been refused.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Location {
    pub latitude: f64,
    pub longitude: f64,
    /// Radius of the reported confidence circle, in metres.
    ///
    /// `None` means the reading carried no accuracy figure — not that it was
    /// perfect. Nothing infers a value when the platform declines to give one.
    pub accuracy_meters: Option<f64>,
    pub source: LocationSource,
    /// When the position was measured, which is not when the incident was
    /// filed. A stale fix attached to a fresh incident is a real failure mode,
    /// and the two timestamps are what let a reader notice it.
    ///
    /// `None` for hand-entered coordinates and for incidents written before
    /// this field existed.
    pub captured_at: Option<DateTime<Utc>>,
}

impl Location {
    /// Validates a position and its provenance.
    ///
    /// Rules enforced here:
    /// - coordinates are finite and within valid WGS 84 ranges;
    /// - accuracy, when present, is finite and between 0 and
    ///   [`MAX_ACCURACY_METERS`].
    ///
    /// A negative or non-finite accuracy is rejected rather than dropped: it
    /// means the sender is confused about its own reading, and silently
    /// discarding the figure would present the coordinate as better attested
    /// than it is.
    pub fn new(
        latitude: f64,
        longitude: f64,
        accuracy_meters: Option<f64>,
        source: LocationSource,
        captured_at: Option<DateTime<Utc>>,
    ) -> CoreResult<Self> {
        if !latitude.is_finite() || !longitude.is_finite() {
            return Err(CoreError::validation("coordinates must be finite numbers"));
        }
        if !(-90.0..=90.0).contains(&latitude) {
            return Err(CoreError::validation(
                "latitude must be between -90 and 90 degrees",
            ));
        }
        if !(-180.0..=180.0).contains(&longitude) {
            return Err(CoreError::validation(
                "longitude must be between -180 and 180 degrees",
            ));
        }

        if let Some(accuracy) = accuracy_meters {
            if !accuracy.is_finite() {
                return Err(CoreError::validation(
                    "location accuracy must be a finite number",
                ));
            }
            if accuracy < 0.0 {
                return Err(CoreError::validation(
                    "location accuracy must not be negative",
                ));
            }
            if accuracy > MAX_ACCURACY_METERS {
                return Err(CoreError::validation(
                    "location accuracy is implausibly large",
                ));
            }
        }

        Ok(Location {
            latitude,
            longitude,
            accuracy_meters,
            source,
            // The platform reports microseconds and the database keeps
            // milliseconds. Truncating here rather than on the way to disk
            // means a validated position equals the one read back, which is
            // what replication comparisons rely on.
            captured_at: captured_at.map(super::to_storage_precision),
        })
    }
}

/// A persisted incident. Every instance has already passed validation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Incident {
    pub id: String,
    /// `node_id` of the node that authored this incident.
    pub created_by: String,
    pub description: String,
    pub severity: Severity,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    /// Reported accuracy radius in metres. Meaningless without coordinates,
    /// and `None` whenever the reading carried no figure.
    pub accuracy_meters: Option<f64>,
    /// Provenance of the coordinates. `Unknown` for hand-entered positions and
    /// for incidents recorded before this field existed.
    pub location_source: LocationSource,
    /// When the position was measured, as distinct from `created_at`, which is
    /// when the incident was filed.
    pub location_captured_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub sync_status: SyncStatus,
}

impl Incident {
    /// The position as a single value, or `None` if this incident has no
    /// coordinates.
    ///
    /// The fields are stored flat because that is how they are stored in SQLite
    /// and sent over IPC; this regroups them for code that reasons about a
    /// position as one thing.
    pub fn location(&self) -> Option<Location> {
        match (self.latitude, self.longitude) {
            (Some(latitude), Some(longitude)) => Some(Location {
                latitude,
                longitude,
                accuracy_meters: self.accuracy_meters,
                source: self.location_source,
                captured_at: self.location_captured_at,
            }),
            _ => None,
        }
    }
}

/// A note appended to an incident.
///
/// Observations are how SecureMesh records developments. Nothing about the
/// original incident is ever mutated, so two nodes writing about the same
/// incident while partitioned produce two observations that merge by union —
/// there is no conflict to resolve and no update to lose.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Observation {
    pub id: String,
    pub incident_id: String,
    /// `node_id` of the node that recorded this observation.
    pub author_node: String,
    pub note: String,
    pub created_at: DateTime<Utc>,
}

/// Unvalidated incident input as it arrives from the UI.
///
/// This type is the trust boundary: it is the only thing the command layer
/// accepts, and it cannot become an [`Incident`] without passing
/// [`NewIncident::validate`].
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewIncident {
    pub description: String,
    pub severity: String,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    /// Optional in the wire form so an older client, or a hand-typed
    /// coordinate, is still accepted. Absent means unknown, never zero.
    #[serde(default)]
    pub accuracy_meters: Option<f64>,
    #[serde(default)]
    pub location_source: Option<LocationSource>,
    #[serde(default)]
    pub location_captured_at: Option<DateTime<Utc>>,
}

impl NewIncident {
    /// Validates the input and stamps it with an ID, author, and timestamps.
    ///
    /// Rules enforced here:
    /// - description is non-empty after trimming and at most
    ///   [`MAX_DESCRIPTION_CHARS`] characters;
    /// - severity is one of the four accepted labels (case-insensitive);
    /// - latitude and longitude are supplied together or not at all;
    /// - coordinates are finite and within valid WGS 84 ranges;
    /// - accuracy is finite and non-negative when present;
    /// - location metadata is not supplied without coordinates to describe.
    ///
    /// That last rule matters: accuracy or a capture time with no position is
    /// either a client bug or an attempt to imply a measurement that was never
    /// taken, and neither should reach a signed record.
    pub fn validate(self, author_node_id: &str) -> CoreResult<Incident> {
        let description = self.description.trim();
        if description.is_empty() {
            return Err(CoreError::validation("description must not be empty"));
        }
        if description.chars().count() > MAX_DESCRIPTION_CHARS {
            return Err(CoreError::validation(format!(
                "description must be at most {MAX_DESCRIPTION_CHARS} characters"
            )));
        }

        let severity: Severity = self.severity.parse()?;

        let location = match (self.latitude, self.longitude) {
            (None, None) => {
                if self.accuracy_meters.is_some()
                    || self.location_source.is_some()
                    || self.location_captured_at.is_some()
                {
                    return Err(CoreError::validation(
                        "location metadata requires coordinates",
                    ));
                }
                None
            }
            (Some(lat), Some(lon)) => Some(Location::new(
                lat,
                lon,
                self.accuracy_meters,
                self.location_source.unwrap_or_default(),
                self.location_captured_at,
            )?),
            _ => {
                return Err(CoreError::validation(
                    "latitude and longitude must be provided together",
                ))
            }
        };

        if author_node_id.trim().is_empty() {
            return Err(CoreError::validation("incident author must be identified"));
        }

        let now = super::now();
        Ok(Incident {
            id: Uuid::new_v4().to_string(),
            created_by: author_node_id.to_string(),
            description: description.to_string(),
            severity,
            latitude: location.map(|position| position.latitude),
            longitude: location.map(|position| position.longitude),
            accuracy_meters: location.and_then(|position| position.accuracy_meters),
            location_source: location.map_or(LocationSource::Unknown, |position| position.source),
            location_captured_at: location.and_then(|position| position.captured_at),
            created_at: now,
            updated_at: now,
            // Phase 1 has no networking, so everything starts unsynchronised.
            sync_status: SyncStatus::Pending,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NODE: &str = "a7f32c9e00000000000000000000000000000000000000000000000000000000";

    fn input(description: &str, severity: &str) -> NewIncident {
        NewIncident {
            description: description.to_string(),
            severity: severity.to_string(),
            latitude: None,
            longitude: None,
            accuracy_meters: None,
            location_source: None,
            location_captured_at: None,
        }
    }

    // --- Severity ---------------------------------------------------------

    #[test]
    fn severity_parses_all_accepted_labels() {
        for severity in Severity::ALL {
            assert_eq!(severity.as_str().parse::<Severity>().unwrap(), severity);
        }
    }

    #[test]
    fn severity_parsing_is_case_and_whitespace_insensitive() {
        assert_eq!("critical".parse::<Severity>().unwrap(), Severity::Critical);
        assert_eq!("  High  ".parse::<Severity>().unwrap(), Severity::High);
    }

    #[test]
    fn severity_rejects_unknown_labels() {
        for bad in ["", "URGENT", "low-ish", "0", "CRITICAL!"] {
            let err = bad.parse::<Severity>().unwrap_err();
            assert_eq!(err.code(), "VALIDATION_ERROR");
        }
    }

    #[test]
    fn severity_orders_from_low_to_critical() {
        assert!(Severity::Low < Severity::Medium);
        assert!(Severity::Medium < Severity::High);
        assert!(Severity::High < Severity::Critical);
    }

    #[test]
    fn severity_serializes_as_an_uppercase_label() {
        let json = serde_json::to_string(&Severity::Critical).unwrap();
        assert_eq!(json, r#""CRITICAL""#);
    }

    // --- Sync status ------------------------------------------------------

    #[test]
    fn sync_status_round_trips_through_its_string_form() {
        for status in [
            SyncStatus::Pending,
            SyncStatus::Syncing,
            SyncStatus::Synced,
            SyncStatus::Failed,
        ] {
            assert_eq!(status.as_str().parse::<SyncStatus>().unwrap(), status);
        }
    }

    // --- Validation -------------------------------------------------------

    #[test]
    fn valid_input_produces_an_incident() {
        let incident = input("Bridge collapse on NH-48", "HIGH")
            .validate(NODE)
            .unwrap();

        assert_eq!(incident.description, "Bridge collapse on NH-48");
        assert_eq!(incident.severity, Severity::High);
        assert_eq!(incident.created_by, NODE);
        assert_eq!(incident.sync_status, SyncStatus::Pending);
        assert_eq!(incident.created_at, incident.updated_at);
    }

    #[test]
    fn ids_are_unique_uuids() {
        let a = input("a", "LOW").validate(NODE).unwrap();
        let b = input("b", "LOW").validate(NODE).unwrap();

        assert_ne!(a.id, b.id);
        assert!(Uuid::parse_str(&a.id).is_ok());
        assert_eq!(Uuid::parse_str(&a.id).unwrap().get_version_num(), 4);
    }

    #[test]
    fn description_is_trimmed() {
        let incident = input("   flooding   ", "LOW").validate(NODE).unwrap();
        assert_eq!(incident.description, "flooding");
    }

    #[test]
    fn empty_or_whitespace_descriptions_are_rejected() {
        for bad in ["", "   ", "\t\n"] {
            let err = input(bad, "LOW").validate(NODE).unwrap_err();
            assert_eq!(err.code(), "VALIDATION_ERROR");
            assert!(err.message().contains("must not be empty"));
        }
    }

    #[test]
    fn overlong_descriptions_are_rejected_at_the_boundary() {
        let at_limit = "x".repeat(MAX_DESCRIPTION_CHARS);
        assert!(input(&at_limit, "LOW").validate(NODE).is_ok());

        let over_limit = "x".repeat(MAX_DESCRIPTION_CHARS + 1);
        let err = input(&over_limit, "LOW").validate(NODE).unwrap_err();
        assert_eq!(err.code(), "VALIDATION_ERROR");
    }

    #[test]
    fn multibyte_descriptions_are_measured_in_characters_not_bytes() {
        // Devanagari text: 3 bytes per character, so a byte-based limit would
        // wrongly reject this.
        let text = "बाढ़".repeat(300);
        assert!(text.len() > MAX_DESCRIPTION_CHARS);
        assert!(text.chars().count() <= MAX_DESCRIPTION_CHARS);
        assert!(input(&text, "LOW").validate(NODE).is_ok());
    }

    #[test]
    fn coordinates_are_optional_but_must_come_as_a_pair() {
        let mut only_lat = input("x", "LOW");
        only_lat.latitude = Some(12.9);
        let err = only_lat.validate(NODE).unwrap_err();
        assert!(err.message().contains("together"));

        let mut only_lon = input("x", "LOW");
        only_lon.longitude = Some(77.5);
        assert!(only_lon.validate(NODE).is_err());
    }

    #[test]
    fn valid_coordinates_are_preserved() {
        let mut with_location = input("x", "LOW");
        with_location.latitude = Some(12.9716);
        with_location.longitude = Some(77.5946);

        let incident = with_location.validate(NODE).unwrap();
        assert_eq!(incident.latitude, Some(12.9716));
        assert_eq!(incident.longitude, Some(77.5946));
    }

    #[test]
    fn coordinate_extremes_are_accepted() {
        for (lat, lon) in [(-90.0, -180.0), (90.0, 180.0), (0.0, 0.0)] {
            let mut candidate = input("x", "LOW");
            candidate.latitude = Some(lat);
            candidate.longitude = Some(lon);
            assert!(candidate.validate(NODE).is_ok(), "{lat},{lon} should be ok");
        }
    }

    #[test]
    fn out_of_range_coordinates_are_rejected() {
        for (lat, lon) in [(90.1, 0.0), (-90.1, 0.0), (0.0, 180.1), (0.0, -180.1)] {
            let mut candidate = input("x", "LOW");
            candidate.latitude = Some(lat);
            candidate.longitude = Some(lon);
            let err = candidate.validate(NODE).unwrap_err();
            assert_eq!(err.code(), "VALIDATION_ERROR");
        }
    }

    #[test]
    fn non_finite_coordinates_are_rejected_without_panicking() {
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let mut candidate = input("x", "LOW");
            candidate.latitude = Some(value);
            candidate.longitude = Some(0.0);
            let err = candidate.validate(NODE).unwrap_err();
            assert!(err.message().contains("finite"));
        }
    }

    #[test]
    fn an_unidentified_author_is_rejected() {
        let err = input("x", "LOW").validate("  ").unwrap_err();
        assert!(err.message().contains("author"));
    }

    #[test]
    fn control_characters_in_descriptions_do_not_panic() {
        let incident = input("line1\nline2\u{0}\u{7}", "LOW")
            .validate(NODE)
            .unwrap();
        assert!(incident.description.contains("line1"));
    }
}
