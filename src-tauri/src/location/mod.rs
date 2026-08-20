//! Device location, obtained locally.
//!
//! # What this is, and what it deliberately is not
//!
//! This module answers one question — *where is this device now?* — and hands
//! the answer to the operator, who decides whether to attach it to an incident.
//! It does not geocode, it does not draw a map, and it does not track anything
//! over time. A position is a **snapshot** taken when the operator asks for one.
//!
//! Nothing here reaches the network on SecureMesh's behalf. There is no HTTP
//! client, no tile server, no geocoding service and no API key. The position
//! comes from the operating system's own location stack.
//!
//! # The honesty problem
//!
//! A desktop machine rarely has a GNSS receiver. Windows will still return a
//! position, but it may have derived it from nearby Wi-Fi networks or from the
//! IP address — which is a guess accurate to a city, not a street, and which
//! *does* involve the OS contacting Microsoft. Reporting that as "GPS" would be
//! a lie that matters: a responder deciding whether to walk to a coordinate
//! needs to know whether it is a satellite fix or an IP lookup.
//!
//! So [`DeviceLocation`] carries its [`LocationSource`] and its accuracy, both
//! reported exactly as the platform gave them, and the UI shows them. When the
//! platform declines to say, the answer is "unknown" rather than an assumption.
//!
//! **No position is ever invented.** If the platform cannot produce one, this
//! module returns an error saying so. There is no default coordinate, no
//! last-known fallback, and no placeholder.
//!
//! # Why a trait
//!
//! ```text
//!   LocationProvider (trait)
//!     ├── WindowsLocationProvider   platform location service (today)
//!     ├── UnavailableProvider       honest "this machine cannot" (today)
//!     └── NmeaSerialProvider        USB/UART GNSS module (Phase 6, not built)
//! ```
//!
//! The field hardware this project targets will carry a GNSS module on a serial
//! port, which is a genuinely offline source. Putting the seam here means that
//! provider can be added without the command layer, the UI, or the incident
//! model changing at all.

use crate::error::{CoreError, CoreResult};
use chrono::{DateTime, Utc};
use serde::Serialize;

#[cfg(target_os = "windows")]
mod windows_provider;

/// How the platform arrived at a position.
///
/// Carried through to the UI because the difference between a satellite fix and
/// an IP-address guess is the difference between metres and tens of kilometres.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum LocationSource {
    /// A GNSS receiver. The only source that works with no network at all.
    Satellite,
    /// Derived from nearby wireless networks or cell towers. Typically tens to
    /// hundreds of metres, and the lookup itself is not offline.
    Wireless,
    /// Derived from the IP address. City-level at best; effectively useless for
    /// dispatch, and shown as such rather than dressed up.
    IpAddress,
    /// The platform did not say.
    Unknown,
}

impl LocationSource {
    /// Whether this source can work with no network connection.
    ///
    /// Only satellite qualifies. Stated as a method so the UI cannot get the
    /// rule subtly wrong somewhere else.
    pub fn works_offline(self) -> bool {
        matches!(self, LocationSource::Satellite)
    }
}

/// Translates a platform reading into the vocabulary an incident record keeps.
///
/// The record deliberately holds a coarser set than the platform reports. It
/// keeps the one distinction that changes a decision — did this come off a
/// satellite, or did it not — and drops detail a reader could not act on.
///
/// `IpAddress` collapses to `Unknown` rather than to `Wireless`. An IP lookup
/// resolves to a city; calling it wireless would put it in the same bucket as a
/// Wi-Fi fix two orders of magnitude better. The accuracy radius travels with
/// it, so such a reading is stored as unknown provenance carrying a figure in
/// the tens of kilometres, which is an honest description of what it is.
///
/// The same table is expressed as serde aliases on
/// [`crate::domain::LocationSource`], which is how a value arriving as JSON from
/// the UI is mapped. A test asserts the two agree on every variant.
impl From<LocationSource> for crate::domain::LocationSource {
    fn from(source: LocationSource) -> Self {
        match source {
            LocationSource::Satellite => crate::domain::LocationSource::Gnss,
            LocationSource::Wireless => crate::domain::LocationSource::Wireless,
            LocationSource::IpAddress | LocationSource::Unknown => {
                crate::domain::LocationSource::Unknown
            }
        }
    }
}

/// Whether this device will give SecureMesh a position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum LocationPermission {
    /// Never asked. The operator has not pressed anything yet.
    NotRequested,
    /// The platform will provide positions.
    Granted,
    /// The operator or an administrator refused.
    Denied,
    /// No location stack on this build or this machine — nothing to permit.
    Unavailable,
}

/// One position fix, exactly as the platform reported it.
///
/// Every field except the coordinates is optional, because platforms genuinely
/// differ in what they supply and a missing value must read as "unknown", never
/// as zero.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceLocation {
    pub latitude: f64,
    pub longitude: f64,
    /// Radius of uncertainty in metres, if the platform reported one.
    pub accuracy_meters: Option<f64>,
    pub altitude_meters: Option<f64>,
    pub heading_degrees: Option<f64>,
    pub speed_mps: Option<f64>,
    pub source: LocationSource,
    /// When the fix was taken — the moment the position describes.
    pub captured_at: DateTime<Utc>,
}

impl DeviceLocation {
    /// Rejects a fix the incident model would refuse anyway.
    ///
    /// The same range and finiteness rules as `NewIncident::validate`, applied
    /// at the point of capture so a nonsensical platform reading is caught where
    /// it can be explained, rather than surfacing later as a form error the
    /// operator cannot act on. This is a second gate, not a replacement: the
    /// incident boundary still validates everything it is given.
    pub fn validated(self) -> CoreResult<Self> {
        if !self.latitude.is_finite() || !self.longitude.is_finite() {
            return Err(CoreError::internal(
                "the location provider returned a non-finite coordinate",
            ));
        }
        if !(-90.0..=90.0).contains(&self.latitude) {
            return Err(CoreError::internal(
                "the location provider returned a latitude outside -90..90",
            ));
        }
        if !(-180.0..=180.0).contains(&self.longitude) {
            return Err(CoreError::internal(
                "the location provider returned a longitude outside -180..180",
            ));
        }
        // A negative radius of uncertainty is meaningless; drop it rather than
        // display it.
        let accuracy_meters = self
            .accuracy_meters
            .filter(|value| value.is_finite() && *value >= 0.0);

        Ok(Self {
            accuracy_meters,
            ..self
        })
    }
}

/// A source of device positions.
///
/// Implementations must never fabricate a position. Returning an error is
/// always preferable to returning a plausible-looking coordinate.
pub trait LocationProvider: Send + Sync {
    /// Current permission state. Must not prompt.
    ///
    /// Called to render the UI, so it has to be cheap and silent — a check that
    /// popped a system dialog would fire every time the form opened.
    fn permission(&self) -> LocationPermission;

    /// Asks the platform for access, prompting if it chooses to.
    ///
    /// Only ever called from an explicit operator action.
    fn request_permission(&self) -> LocationPermission;

    /// Takes one fix. Blocks until the platform answers or gives up.
    fn current_location(&self) -> CoreResult<DeviceLocation>;

    /// Short description of where positions come from, for diagnostics.
    fn describe(&self) -> &'static str;
}

/// The provider for a build or machine with no location stack.
///
/// Exists so that "unavailable" is a first-class, tested state rather than an
/// error path nobody exercised. It is what runs on Linux and macOS today, and on
/// Windows if the platform APIs cannot be reached.
pub struct UnavailableProvider {
    reason: &'static str,
}

impl UnavailableProvider {
    pub fn new(reason: &'static str) -> Self {
        Self { reason }
    }
}

impl LocationProvider for UnavailableProvider {
    fn permission(&self) -> LocationPermission {
        LocationPermission::Unavailable
    }

    fn request_permission(&self) -> LocationPermission {
        LocationPermission::Unavailable
    }

    fn current_location(&self) -> CoreResult<DeviceLocation> {
        Err(CoreError::internal(self.reason))
    }

    fn describe(&self) -> &'static str {
        self.reason
    }
}

/// The provider for this platform.
///
/// Windows has a location service worth asking. Everything else reports
/// unavailable honestly until a GNSS provider is written for it — which is a
/// Phase 6 concern, on hardware that actually has a receiver.
pub fn platform_provider() -> Box<dyn LocationProvider> {
    #[cfg(target_os = "windows")]
    {
        Box::new(windows_provider::WindowsLocationProvider::new())
    }
    #[cfg(not(target_os = "windows"))]
    {
        Box::new(UnavailableProvider::new(
            "No location provider is implemented for this platform. Coordinates \
             can still be entered by hand.",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fix(latitude: f64, longitude: f64) -> DeviceLocation {
        DeviceLocation {
            latitude,
            longitude,
            accuracy_meters: Some(8.0),
            altitude_meters: None,
            heading_degrees: None,
            speed_mps: None,
            source: LocationSource::Satellite,
            captured_at: Utc::now(),
        }
    }

    #[test]
    fn a_normal_fix_is_accepted() {
        let location = fix(12.9716, 77.5946).validated().unwrap();
        assert_eq!(location.latitude, 12.9716);
        assert_eq!(location.accuracy_meters, Some(8.0));
    }

    #[test]
    fn the_extremes_of_the_coordinate_system_are_valid() {
        for (lat, lon) in [(-90.0, -180.0), (90.0, 180.0), (0.0, 0.0)] {
            assert!(
                fix(lat, lon).validated().is_ok(),
                "{lat},{lon} is a real place"
            );
        }
    }

    #[test]
    fn a_coordinate_outside_the_world_is_refused() {
        for (lat, lon) in [(90.1, 0.0), (-90.1, 0.0), (0.0, 180.1), (0.0, -180.1)] {
            assert!(
                fix(lat, lon).validated().is_err(),
                "{lat},{lon} is not a place"
            );
        }
    }

    #[test]
    fn a_non_finite_coordinate_is_refused() {
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(fix(value, 0.0).validated().is_err());
            assert!(fix(0.0, value).validated().is_err());
        }
    }

    #[test]
    fn a_meaningless_accuracy_is_dropped_rather_than_shown() {
        // "±-5 m" would be worse than saying nothing.
        let mut candidate = fix(1.0, 1.0);
        candidate.accuracy_meters = Some(-5.0);
        assert_eq!(candidate.validated().unwrap().accuracy_meters, None);

        let mut nan_accuracy = fix(1.0, 1.0);
        nan_accuracy.accuracy_meters = Some(f64::NAN);
        assert_eq!(nan_accuracy.validated().unwrap().accuracy_meters, None);
    }

    #[test]
    fn only_a_satellite_fix_counts_as_offline_capable() {
        assert!(LocationSource::Satellite.works_offline());
        for source in [
            LocationSource::Wireless,
            LocationSource::IpAddress,
            LocationSource::Unknown,
        ] {
            assert!(
                !source.works_offline(),
                "{source:?} needs a network and must not be presented as offline"
            );
        }
    }

    #[test]
    fn an_unavailable_provider_reports_rather_than_inventing() {
        let provider = UnavailableProvider::new("no receiver on this machine");

        assert_eq!(provider.permission(), LocationPermission::Unavailable);
        assert_eq!(
            provider.request_permission(),
            LocationPermission::Unavailable
        );

        // The property that matters: it fails instead of producing a plausible
        // coordinate that would be silently wrong.
        let error = provider.current_location().unwrap_err();
        assert!(error.message().contains("no receiver"));
    }

    #[test]
    fn states_serialise_as_the_ui_expects() {
        // The UI matches on these strings; a silent rename would leave it
        // rendering nothing.
        assert_eq!(
            serde_json::to_string(&LocationPermission::NotRequested).unwrap(),
            "\"NOT_REQUESTED\""
        );
        assert_eq!(
            serde_json::to_string(&LocationPermission::Granted).unwrap(),
            "\"GRANTED\""
        );
        assert_eq!(
            serde_json::to_string(&LocationPermission::Denied).unwrap(),
            "\"DENIED\""
        );
        assert_eq!(
            serde_json::to_string(&LocationPermission::Unavailable).unwrap(),
            "\"UNAVAILABLE\""
        );
        assert_eq!(
            serde_json::to_string(&LocationSource::Satellite).unwrap(),
            "\"SATELLITE\""
        );
        assert_eq!(
            serde_json::to_string(&LocationSource::IpAddress).unwrap(),
            "\"IP_ADDRESS\""
        );
    }
}
