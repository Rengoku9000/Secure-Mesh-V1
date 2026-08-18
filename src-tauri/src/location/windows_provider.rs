//! Device location via the Windows location service.
//!
//! Uses `Windows.Devices.Geolocation` through the `windows` crate, which Tauri
//! already brings in — so this adds a feature, not a supply chain.
//!
//! # What Windows actually gives you
//!
//! Windows will answer even on a machine with no GNSS receiver, by triangulating
//! nearby Wi-Fi networks or by looking up the IP address. Those are legitimate
//! answers to "roughly where am I", and useless answers to "which building".
//! The platform tells us which it used, and that is reported verbatim as
//! [`LocationSource`] rather than being flattened into "GPS".
//!
//! **Only a satellite fix is genuinely offline.** Wi-Fi and IP positioning
//! involve the operating system contacting Microsoft — not SecureMesh making a
//! network call, but not offline either, and the UI says so.
//!
//! # Permission
//!
//! `RequestAccessAsync` is the only call that may prompt, and it is reached
//! exclusively from an explicit operator action. Reading the current state never
//! prompts, so rendering the form cannot pop a system dialog.

use super::{DeviceLocation, LocationPermission, LocationProvider, LocationSource};
use crate::error::{CoreError, CoreResult};
use chrono::{TimeZone, Utc};
use std::sync::Mutex;
use windows::Devices::Geolocation::{
    GeolocationAccessStatus, Geolocator, PositionAccuracy, PositionSource,
};
use windows_future::{AsyncOperationCompletedHandler, IAsyncOperation};

/// Longest a single fix may take before the platform is told to give up.
///
/// A cold GNSS start can genuinely take a while; beyond this the operator is
/// better served by an error they can act on than by a form that appears hung.
const FIX_TIMEOUT_SECONDS: i64 = 12;

/// Oldest cached fix that may be reused, in seconds.
///
/// A little staleness is fine and avoids waking the radio for a position taken
/// moments ago. Anything older is not "where I am now".
const MAX_AGE_SECONDS: i64 = 30;

pub struct WindowsLocationProvider {
    /// Last permission state the platform reported.
    ///
    /// Cached because `permission()` is called on every render and must not
    /// prompt or block; it is refreshed whenever an operator action learns
    /// something newer.
    known: Mutex<LocationPermission>,
}

impl WindowsLocationProvider {
    pub fn new() -> Self {
        Self {
            known: Mutex::new(LocationPermission::NotRequested),
        }
    }

    fn remember(&self, state: LocationPermission) -> LocationPermission {
        *self.known.lock().unwrap_or_else(|p| p.into_inner()) = state;
        state
    }
}

impl Default for WindowsLocationProvider {
    fn default() -> Self {
        Self::new()
    }
}

/// Waits for a WinRT async operation to finish, on this thread.
///
/// The blocking helper in `windows-future` is private in this version, and the
/// public route is `IntoFuture` — which would mean dragging an async executor
/// into a synchronous command, and risking a panic if one is already running.
/// Registering a completion handler and waiting on a channel is what that helper
/// does internally, so this is the same mechanism without the dependency.
///
/// `SetCompleted` fires immediately if the operation has already finished, so
/// there is no race between registering and completing.
fn wait_for<T>(operation: IAsyncOperation<T>) -> windows::core::Result<T>
where
    T: windows::core::RuntimeType + 'static,
{
    let (done, wait) = std::sync::mpsc::channel::<()>();
    operation.SetCompleted(&AsyncOperationCompletedHandler::<T>::new(
        move |_operation, _status| {
            // The receiver may already be gone if this fired synchronously.
            let _ = done.send(());
            Ok(())
        },
    ))?;
    let _ = wait.recv();
    operation.GetResults()
}

/// Maps the platform's own account of where a position came from.
fn source_of(raw: PositionSource) -> LocationSource {
    match raw {
        PositionSource::Satellite => LocationSource::Satellite,
        // Cellular and Wi-Fi are both "triangulated from nearby transmitters".
        PositionSource::Cellular | PositionSource::WiFi => LocationSource::Wireless,
        PositionSource::IPAddress => LocationSource::IpAddress,
        // `Default` and `Obfuscated` say nothing about the underlying method,
        // and `Obfuscated` means Windows deliberately degraded it.
        _ => LocationSource::Unknown,
    }
}

/// Unwraps an optional WinRT double, treating absence and error alike as
/// "the platform did not say".
fn optional_double(
    value: windows::core::Result<windows::Foundation::IReference<f64>>,
) -> Option<f64> {
    value.ok()?.Value().ok().filter(|v| v.is_finite())
}

impl LocationProvider for WindowsLocationProvider {
    fn permission(&self) -> LocationPermission {
        *self.known.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn request_permission(&self) -> LocationPermission {
        let status = Geolocator::RequestAccessAsync().and_then(wait_for);

        let resolved = match status {
            Ok(GeolocationAccessStatus::Allowed) => LocationPermission::Granted,
            Ok(GeolocationAccessStatus::Denied) => LocationPermission::Denied,
            // `Unspecified` means Windows could not determine a state — treated
            // as not yet answered rather than as a refusal.
            Ok(_) => LocationPermission::NotRequested,
            // The API itself is unreachable: the service is disabled or this
            // Windows edition has no location stack.
            Err(_) => LocationPermission::Unavailable,
        };

        self.remember(resolved)
    }

    fn current_location(&self) -> CoreResult<DeviceLocation> {
        // Asking without permission would prompt from a code path the operator
        // did not initiate, so the state is established first.
        if self.permission() != LocationPermission::Granted {
            let granted = self.request_permission();
            if granted != LocationPermission::Granted {
                return Err(CoreError::internal(match granted {
                    LocationPermission::Denied => {
                        "Location access is denied. Enable it for this app in \
                         Windows Settings › Privacy & security › Location."
                    }
                    LocationPermission::Unavailable => {
                        "This machine has no usable location service."
                    }
                    _ => "Location access was not granted.",
                }));
            }
        }

        let geolocator = Geolocator::new()
            .map_err(|e| CoreError::internal(format!("location service unavailable: {e}")))?;

        // Ask for the best the platform can do. On a machine with a receiver
        // this prefers satellite; without one it changes nothing.
        let _ = geolocator.SetDesiredAccuracy(PositionAccuracy::High);

        let position = geolocator
            .GetGeopositionAsyncWithAgeAndTimeout(
                windows::Foundation::TimeSpan {
                    // WinRT TimeSpan counts 100-nanosecond intervals.
                    Duration: MAX_AGE_SECONDS * 10_000_000,
                },
                windows::Foundation::TimeSpan {
                    Duration: FIX_TIMEOUT_SECONDS * 10_000_000,
                },
            )
            .and_then(wait_for)
            .map_err(|e| {
                CoreError::internal(format!(
                    "no position could be obtained within {FIX_TIMEOUT_SECONDS}s: {e}"
                ))
            })?;

        let coordinate = position
            .Coordinate()
            .map_err(|e| CoreError::internal(format!("position carried no coordinate: {e}")))?;

        let point = coordinate
            .Point()
            .and_then(|p| p.Position())
            .map_err(|e| CoreError::internal(format!("coordinate carried no point: {e}")))?;

        // The platform's own timestamp, so the fix is stamped with when it was
        // taken rather than when this code happened to read it.
        let captured_at = coordinate
            .Timestamp()
            .ok()
            .and_then(|stamp| {
                // WinRT DateTime is 100ns intervals since 1601-01-01 UTC.
                const EPOCH_DIFFERENCE_SECONDS: i64 = 11_644_473_600;
                let seconds = stamp.UniversalTime / 10_000_000 - EPOCH_DIFFERENCE_SECONDS;
                Utc.timestamp_opt(seconds, 0).single()
            })
            .unwrap_or_else(Utc::now);

        DeviceLocation {
            latitude: point.Latitude,
            longitude: point.Longitude,
            accuracy_meters: coordinate.Accuracy().ok().filter(|v| v.is_finite()),
            altitude_meters: Some(point.Altitude).filter(|v| v.is_finite() && *v != 0.0),
            heading_degrees: optional_double(coordinate.Heading()),
            speed_mps: optional_double(coordinate.Speed()),
            source: coordinate
                .PositionSource()
                .map(source_of)
                .unwrap_or(LocationSource::Unknown),
            captured_at,
        }
        .validated()
    }

    fn describe(&self) -> &'static str {
        "Windows location service (Windows.Devices.Geolocation)"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_platform_source_maps_to_an_honest_label() {
        assert_eq!(
            source_of(PositionSource::Satellite),
            LocationSource::Satellite
        );
        assert_eq!(source_of(PositionSource::WiFi), LocationSource::Wireless);
        assert_eq!(
            source_of(PositionSource::Cellular),
            LocationSource::Wireless
        );
        assert_eq!(
            source_of(PositionSource::IPAddress),
            LocationSource::IpAddress
        );
        // Obfuscated means Windows degraded the fix on purpose; claiming any
        // particular method for it would be an invention.
        assert_eq!(
            source_of(PositionSource::Obfuscated),
            LocationSource::Unknown
        );
        assert_eq!(source_of(PositionSource::Default), LocationSource::Unknown);
    }

    #[test]
    fn a_fresh_provider_has_not_asked_for_anything_yet() {
        // Constructing the provider must not prompt or probe: it is built during
        // startup, long before any operator asks for a position.
        let provider = WindowsLocationProvider::new();
        assert_eq!(provider.permission(), LocationPermission::NotRequested);
    }

    #[test]
    fn the_timeout_is_long_enough_for_a_cold_start_and_short_enough_to_report() {
        const { assert!(FIX_TIMEOUT_SECONDS >= 5) };
        const { assert!(FIX_TIMEOUT_SECONDS <= 30) };
    }
}
