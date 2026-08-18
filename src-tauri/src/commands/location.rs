//! Device location commands.
//!
//! Three thin commands, all operator-initiated. There is no command that starts
//! a watch, and none that writes anything: a position becomes part of the record
//! only when the operator submits it with an incident, through the ordinary
//! `create_incident` path.
//!
//! # Why reading a position is not audited
//!
//! Reading a sensor is an observation, not a state change, and the audit log
//! records changes and decisions (see `crate::security::audit`). A record per
//! GPS read would be the same mistake that once put 86,000 lines a day of
//! `identity.public_disclosed` into the log. The moment worth auditing is
//! `incident.created`, which already happens and already carries the
//! coordinates.

use super::AppState;
use crate::error::CoreResult;
use crate::location::{DeviceLocation, LocationPermission};
use tauri::State;

/// Whether this device will report a position. Never prompts.
///
/// Safe to call while rendering, which is the point: the form can show its
/// state without a system dialog appearing at an arbitrary moment.
#[tauri::command]
pub fn get_location_permission(state: State<'_, AppState>) -> LocationPermission {
    state.runtime.location_permission()
}

/// Asks the platform for location access.
///
/// May show a system prompt, so it is reached only from an explicit operator
/// action — never from a render or a poll.
#[tauri::command]
pub fn request_location_permission(state: State<'_, AppState>) -> LocationPermission {
    state.runtime.request_location_permission()
}

/// Takes one position fix.
///
/// Returns an error rather than a placeholder when the platform cannot answer.
/// The operator can still create the incident without coordinates.
#[tauri::command]
pub fn get_current_location(state: State<'_, AppState>) -> CoreResult<DeviceLocation> {
    state.runtime.current_location()
}
