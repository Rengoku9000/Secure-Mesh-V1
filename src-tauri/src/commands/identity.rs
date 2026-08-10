//! Identity commands.

use super::AppState;
use crate::error::CoreResult;
use crate::identity::PublicIdentity;
use tauri::State;

/// Returns this node's public identity.
///
/// The response contains the node ID, display name, and public key only.
/// There is intentionally no command that exposes the private key: it never
/// leaves the identity module, so no IPC caller can request it.
#[tauri::command]
pub fn get_node_identity(state: State<'_, AppState>) -> CoreResult<PublicIdentity> {
    Ok(state.runtime.public_identity())
}
