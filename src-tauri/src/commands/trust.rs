//! Peer authorization commands.
//!
//! These are the *only* way a trust decision enters the system. Nothing a peer
//! sends over the mesh can reach them, which is what stops an enrolled peer
//! from promoting itself or anyone else.
//!
//! Each one delegates straight to the runtime, which performs the capability
//! check. The UI decides what to *draw*; it never decides what is *allowed*.

use super::AppState;
use crate::domain::trust::{Capability, PeerRole, TrustEvent, TrustState};
use crate::error::CoreResult;
use serde::Serialize;
use tauri::State;

/// What the local operator is permitted to do, for the UI to render against.
///
/// The UI uses this to decide which controls to show. That is presentation
/// only: the same checks run again in the core on every call, so a frontend
/// that ignored this and issued the command anyway would still be refused.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalAuthority {
    pub node_id: String,
    pub node_name: String,
    pub role: PeerRole,
    pub capabilities: Vec<Capability>,
    /// Whether this operator may approve or reject peers.
    pub can_enroll: bool,
    /// Whether this operator may revoke or reinstate peers.
    pub can_revoke: bool,
}

/// The local operator's role and capabilities.
#[tauri::command]
pub fn get_local_authority(state: State<'_, AppState>) -> CoreResult<LocalAuthority> {
    let role = state.runtime.local_role()?;
    let identity = state.runtime.public_identity();

    Ok(LocalAuthority {
        node_id: identity.node_id,
        node_name: identity.node_name,
        role,
        capabilities: role.capabilities(),
        can_enroll: role.grants(Capability::PeerEnroll),
        can_revoke: role.grants(Capability::PeerRevoke),
    })
}

/// Authorizes a peer, or reinstates a revoked one.
#[tauri::command]
pub fn approve_peer(
    state: State<'_, AppState>,
    node_id: String,
    note: Option<String>,
) -> CoreResult<TrustState> {
    state.runtime.approve_peer(&node_id, note.as_deref())
}

/// Refuses a peer that has never been authorized.
#[tauri::command]
pub fn reject_peer(
    state: State<'_, AppState>,
    node_id: String,
    note: Option<String>,
) -> CoreResult<TrustState> {
    state.runtime.reject_peer(&node_id, note.as_deref())
}

/// Withdraws authorization from a peer.
#[tauri::command]
pub fn revoke_peer(
    state: State<'_, AppState>,
    node_id: String,
    note: Option<String>,
) -> CoreResult<TrustState> {
    state.runtime.revoke_peer(&node_id, note.as_deref())
}

/// The local trust audit log, newest first.
#[tauri::command]
pub fn get_trust_audit_log(
    state: State<'_, AppState>,
    node_id: Option<String>,
    limit: Option<u32>,
) -> CoreResult<Vec<TrustEvent>> {
    state
        .runtime
        .trust_audit_log(node_id.as_deref(), limit.unwrap_or(100))
}
