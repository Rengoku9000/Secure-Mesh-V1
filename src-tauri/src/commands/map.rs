//! Offline map commands.
//!
//! Two commands, split by cost. The dashboard polls status every couple of
//! seconds; geometry is read once. Collapsing them into one call would drag
//! megabytes of coastline across the IPC boundary thirty times a minute.
//!
//! Neither command reaches the network, because there is nothing in
//! `crate::map` that could: a basemap is a file on disk, read and handed on.
//! There is no tile request to make, so there is none to authorize.

use super::AppState;
use crate::error::CoreResult;
use crate::map::Basemap;
use tauri::State;

/// The provisioned basemap description, or `None` when nothing is installed.
///
/// Safe to poll: reports what is installed without returning its geometry, and
/// returns `None` rather than an error for an unprovisioned node, because
/// having no map data is a normal state rather than a failure.
#[tauri::command]
pub fn get_map_basemap(state: State<'_, AppState>) -> Option<Basemap> {
    state.runtime.map_basemap()
}

/// The basemap geometry, as the operator provisioned it.
///
/// Returned verbatim rather than re-serialised, so what the renderer draws is
/// exactly the bytes the checksum in [`Basemap`] describes. Called once when
/// the map mounts — never on the refresh cycle.
#[tauri::command]
pub fn get_map_geojson(state: State<'_, AppState>) -> CoreResult<String> {
    state.runtime.map_geojson()
}
