//! The Tauri IPC surface.
//!
//! Commands are deliberately thin: they unwrap the managed [`NodeRuntime`],
//! forward the call, and return a serialisable result. No business rule,
//! validation, or status decision lives here — that belongs to the runtime, so
//! it stays testable without a Tauri application and stays out of React.
//!
//! # Security
//!
//! Every type returned across this boundary is a public projection. There is
//! no command that returns a private key, and none that accepts one.

pub mod identity;
pub mod incidents;
pub mod intelligence;
pub mod location;
pub mod system;
pub mod trust;

// Glob re-exports rather than named ones: `#[tauri::command]` expands to a
// hidden `__cmd__*` item alongside each function, and `generate_handler!`
// needs both to resolve at this path.
pub use identity::*;
pub use incidents::*;
pub use intelligence::*;
pub use location::*;
pub use system::*;
pub use trust::*;

use crate::runtime::NodeRuntime;
use std::sync::Arc;

/// The application state Tauri manages and hands to each command.
pub struct AppState {
    pub runtime: Arc<NodeRuntime>,
}

impl AppState {
    pub fn new(runtime: Arc<NodeRuntime>) -> Self {
        Self { runtime }
    }
}
