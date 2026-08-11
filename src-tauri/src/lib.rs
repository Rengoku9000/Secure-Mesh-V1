//! SecureMesh node core.
//!
//! An offline-first edge node that holds a cryptographic identity, persists
//! operational records locally, and — from Phase 2 — synchronises them with
//! nearby peers over a local mesh. Nothing in this crate contacts a cloud
//! service; see `docs/architecture/ARCHITECTURE.md`.
//!
//! ```text
//!   React UI  ──IPC──▶  commands/  ──▶  runtime  ──┬──▶  identity  ──▶  keystore
//!                                                  └──▶  storage   ──▶  SQLite
//! ```

pub mod commands;
pub mod domain;
pub mod error;
pub mod identity;
pub mod networking;
pub mod runtime;
pub mod security;
pub mod storage;
pub mod sync;

pub use error::{CoreError, CoreResult};
pub use runtime::NodeRuntime;

use commands::AppState;
use std::sync::Arc;
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            // The node's data directory is per-application and per-user; Tauri
            // resolves the platform-appropriate location for us.
            //
            // `SECUREMESH_DATA_DIR` overrides it so that a second node can be
            // run on one machine — needed to demonstrate a mesh without two
            // physical devices. Each node needs its own directory because the
            // directory *is* the node: it holds the identity and the log.
            let data_dir = match std::env::var_os("SECUREMESH_DATA_DIR") {
                Some(path) => std::path::PathBuf::from(path),
                None => app.path().app_data_dir().map_err(|e| {
                    format!("could not resolve the application data directory: {e}")
                })?,
            };

            let runtime = start_node(&data_dir).map_err(|e| {
                // Surfaces as a startup failure rather than a half-initialised
                // node: without an identity or a database there is nothing
                // meaningful the UI could do.
                format!("SecureMesh node failed to start: {e}")
            })?;

            eprintln!(
                "[securemesh] node {} ready, data directory: {}",
                runtime.node_name(),
                data_dir.display()
            );
            report_frontend_source(app.config());

            let runtime = Arc::new(runtime);
            spawn_mesh_loop(Arc::clone(&runtime));
            app.manage(AppState::new(runtime));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_node_identity,
            commands::get_system_status,
            commands::get_network_status,
            commands::get_peers,
            commands::create_incident,
            commands::get_incidents,
            commands::get_incident,
            commands::get_observations,
            commands::add_observation,
            commands::get_local_authority,
            commands::approve_peer,
            commands::reject_peer,
            commands::revoke_peer,
            commands::get_trust_audit_log,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

/// States where the window will load its UI from, and why.
///
/// `tauri-build` sets `cfg(dev)` for any ordinary `cargo build`, `cargo test`,
/// `cargo clippy` or `cargo run`. Such a binary loads the UI from `devUrl`
/// rather than from embedded assets, so running it without a dev server shows
/// only the WebView's own connection-refused page — which says nothing about
/// the cause.
///
/// One line here turns that dead end into an actionable message. It is
/// diagnostics, not a workaround: the binary still behaves exactly as its build
/// mode dictates.
fn report_frontend_source(config: &tauri::Config) {
    if cfg!(dev) {
        let dev_url = config
            .build
            .dev_url
            .as_ref()
            .map(|url| url.to_string())
            .unwrap_or_else(|| "the configured dev server".to_string());

        eprintln!(
            "[securemesh] DEVELOPMENT build - the UI is loaded from {dev_url}, not from \
             embedded assets."
        );
        eprintln!(
            "[securemesh] If the window shows ERR_CONNECTION_REFUSED, no dev server is \
             running there. Use `npm run tauri dev`, or build a standalone binary with \
             `npm run app:stage` and run it from dist-app/."
        );
    } else {
        eprintln!("[securemesh] production build - UI assets are embedded; no dev server needed.");
    }
}

/// Brings the node up, attaching the mesh if the transport will start.
///
/// A transport that cannot start — no network interface, a blocked UDP port —
/// is **not** a fatal error. Offline-first means a node that cannot reach the
/// network is still fully functional, so the failure is reported and the node
/// continues standalone rather than refusing to launch.
fn start_node(data_dir: &std::path::Path) -> CoreResult<NodeRuntime> {
    match networking::libp2p_transport::Libp2pTransport::start_for_data_dir(data_dir) {
        Ok(transport) => NodeRuntime::initialize_with_transport(data_dir, Box::new(transport)),
        Err(error) => {
            eprintln!(
                "[securemesh] mesh transport unavailable ({}); continuing standalone",
                error.message()
            );
            NodeRuntime::initialize(data_dir)
        }
    }
}

/// Drives the sync engine on a timer.
///
/// Two cadences, for two different jobs: a short tick to process whatever has
/// arrived, and a slower full round so that records created while a session was
/// already open still propagate without waiting for a reconnection.
fn spawn_mesh_loop(runtime: Arc<NodeRuntime>) {
    if !runtime.mesh_attached() {
        return;
    }

    const TICK: std::time::Duration = std::time::Duration::from_millis(250);
    const RESYNC_EVERY: u32 = 20; // ≈5 seconds

    std::thread::Builder::new()
        .name("securemesh-sync".to_string())
        .spawn(move || {
            let mut ticks: u32 = 0;
            loop {
                match runtime.sync_tick() {
                    // Silent when nothing happened, so an idle mesh stays
                    // quiet; anything else is worth a line, because "the peers
                    // are connected but nothing is replicating" is otherwise
                    // invisible from outside.
                    Ok(report) if report != Default::default() => {
                        eprintln!("[securemesh] sync {report:?}");
                    }
                    Ok(_) => {}
                    // One bad round must not stop replication for good.
                    Err(error) => {
                        eprintln!("[securemesh] sync tick failed: {}", error.message());
                    }
                }

                ticks = ticks.wrapping_add(1);
                if ticks.is_multiple_of(RESYNC_EVERY) {
                    match runtime.authorized_peer_count() {
                        Ok((0, connected)) if connected > 0 => eprintln!(
                            "[securemesh] {connected} peer(s) connected, none authorized — \
                             enrollment required before anything is exchanged"
                        ),
                        Ok((authorized, connected)) if connected > 0 => eprintln!(
                            "[securemesh] sync round: {authorized}/{connected} peer(s) authorized"
                        ),
                        _ => {}
                    }

                    if let Err(error) = runtime.request_sync() {
                        eprintln!("[securemesh] sync round failed: {}", error.message());
                    }
                }

                std::thread::sleep(TICK);
            }
        })
        .map_err(|e| eprintln!("[securemesh] could not start the sync loop: {e}"))
        .ok();
}
