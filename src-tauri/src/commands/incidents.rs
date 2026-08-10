//! Incident commands.

use super::AppState;
use crate::domain::{Incident, NewIncident, Observation};
use crate::error::CoreResult;
use tauri::State;

/// Validates and stores a new incident authored by this node.
///
/// `input` is untrusted. It is passed to the runtime unmodified, which routes
/// it through `NewIncident::validate` before anything touches the database.
#[tauri::command]
pub fn create_incident(state: State<'_, AppState>, input: NewIncident) -> CoreResult<Incident> {
    state.runtime.create_incident(input)
}

/// Returns recent incidents, newest first. The runtime clamps `limit`.
#[tauri::command]
pub fn get_incidents(state: State<'_, AppState>, limit: Option<u32>) -> CoreResult<Vec<Incident>> {
    state.runtime.list_incidents(limit)
}

/// Returns a single incident, or a `NOT_FOUND` error.
#[tauri::command]
pub fn get_incident(state: State<'_, AppState>, id: String) -> CoreResult<Incident> {
    state.runtime.get_incident(&id)
}

/// Observations appended to an incident, from this node or any peer.
#[tauri::command]
pub fn get_observations(
    state: State<'_, AppState>,
    incident_id: String,
) -> CoreResult<Vec<Observation>> {
    state.runtime.list_observations(&incident_id)
}

/// Appends an observation to an incident.
///
/// Phase 2 records developments by appending rather than editing, so two nodes
/// updating the same incident while partitioned cannot lose each other's work.
#[tauri::command]
pub fn add_observation(
    state: State<'_, AppState>,
    incident_id: String,
    note: String,
) -> CoreResult<()> {
    state.runtime.add_observation(&incident_id, &note)
}
