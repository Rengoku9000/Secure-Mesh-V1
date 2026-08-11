//! System and network status commands.

use super::AppState;
use crate::domain::Peer;
use crate::error::CoreResult;
use crate::runtime::{NetworkStatus, SystemStatus};
use crate::sync::LinkSnapshot;
use tauri::State;

/// Health of every subsystem, for the dashboard's status panel.
#[tauri::command]
pub fn get_system_status(state: State<'_, AppState>) -> CoreResult<SystemStatus> {
    Ok(state.runtime.system_status())
}

/// Mesh connectivity and how many records are awaiting propagation.
#[tauri::command]
pub fn get_network_status(state: State<'_, AppState>) -> CoreResult<NetworkStatus> {
    state.runtime.network_status()
}

/// Live synchronisation state for every open session.
///
/// Reports where each link has reached in its lifecycle, how many rounds it has
/// run, and how many events it has accepted — so "connected but idle because it
/// is up to date" is distinguishable from "connected but idle because nothing
/// authorized it".
#[tauri::command]
pub fn get_link_states(state: State<'_, AppState>) -> CoreResult<Vec<LinkSnapshot>> {
    Ok(state.runtime.link_states())
}

/// Known peers, with live connection state and per-peer sync backlog.
///
/// Returns public identity material only — node ID, name, public key. There is
/// no command that exposes any private key, this node's or a peer's.
#[tauri::command]
pub fn get_peers(state: State<'_, AppState>) -> CoreResult<Vec<Peer>> {
    state.runtime.list_peers()
}
