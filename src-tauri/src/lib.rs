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

pub mod ai;
pub mod commands;
pub mod domain;
pub mod error;
pub mod identity;
pub mod location;
pub mod map;
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
            spawn_location_heartbeat(Arc::clone(&runtime));
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
            commands::get_link_states,
            commands::get_intelligence_status,
            commands::analyse_incident,
            commands::get_incident_analysis,
            commands::ask_securemesh,
            commands::index_intelligence,
            commands::get_incident_index_states,
            commands::get_location_permission,
            commands::request_location_permission,
            commands::get_current_location,
            commands::get_peer_locations,
            commands::get_knowledge_documents,
            commands::get_map_basemap,
            commands::get_map_geojson,
            commands::get_knowledge_summary,
            commands::install_operational_knowledge,
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
    let mut runtime =
        match networking::libp2p_transport::Libp2pTransport::start_for_data_dir(data_dir) {
            Ok(transport) => NodeRuntime::initialize_with_transport(data_dir, Box::new(transport))?,
            Err(error) => {
                eprintln!(
                    "[securemesh] mesh transport unavailable ({}); continuing standalone",
                    error.message()
                );
                NodeRuntime::initialize(data_dir)?
            }
        };

    attach_intelligence(&mut runtime);
    Ok(runtime)
}

/// Attaches local intelligence if a model has been provisioned.
///
/// Deliberately infallible. A missing model, a missing runtime, or a broken one
/// leaves the node fully functional without intelligence — the AI layer is
/// never allowed to prevent a node from starting.
///
/// **Nothing is downloaded.** The models are located on disk; if they are not
/// there, the dashboard says so and points at the provisioning instructions.
fn attach_intelligence(runtime: &mut NodeRuntime) {
    use ai::{IntelligenceService, LlamaConfig, LlamaServerEngine};
    use std::sync::Arc;

    // Models live beside the application rather than in the per-node data
    // directory: they are large, read-only, and shared by every node on the
    // machine, so copying them per node would waste gigabytes.
    let Some(root) = project_root() else {
        eprintln!("[securemesh] could not locate the model directory; intelligence disabled");
        return;
    };

    // Ports are derived from the process ID so two nodes on one machine do not
    // fight over a runtime port.
    let base_port = 18_000 + (std::process::id() % 1_000) as u16 * 2;

    let generation = LlamaConfig::generation(&root, base_port);
    let embedding = LlamaConfig::embedding(&root, base_port + 1);

    // Report what is missing rather than failing silently: "AI is unavailable"
    // is only actionable if an operator can see why.
    if let Err(reason) = generation.availability() {
        eprintln!(
            "[securemesh] local intelligence unavailable: {}",
            reason.detail()
        );
        return;
    }
    if let Err(reason) = embedding.availability() {
        eprintln!(
            "[securemesh] local intelligence unavailable: {}",
            reason.detail()
        );
        return;
    }

    let database = runtime.database_handle();
    let service = IntelligenceService::new(
        database,
        Arc::new(LlamaServerEngine::new(generation)),
        Arc::new(LlamaServerEngine::new(embedding)),
    );

    runtime.attach_intelligence(service);
    eprintln!("[securemesh] local intelligence attached (models provisioned, no network used)");
}

/// Locates the directory holding `ai/`.
///
/// Checks the executable's own directory first — that is where a staged or
/// installed build keeps its models — then walks up, which covers running from
/// `target/debug` during development.
fn project_root() -> Option<std::path::PathBuf> {
    if let Ok(explicit) = std::env::var("SECUREMESH_AI_ROOT") {
        return Some(std::path::PathBuf::from(explicit));
    }

    let executable = std::env::current_exe().ok()?;
    let mut directory = executable.parent()?.to_path_buf();

    for _ in 0..6 {
        if directory.join("ai/models").is_dir() {
            return Some(directory);
        }
        if !directory.pop() {
            break;
        }
    }
    None
}

/// Drives the sync engine.
///
/// The tick only *delivers* what the transport has already received; it is not
/// what causes synchronisation. Every legitimate cause — a connection, an
/// authorization, a local write, a peer announcing it is ahead — fires its own
/// trigger through the sync engine at the moment it happens.
///
/// The reconciliation sweep is a safety net for a message lost without a
/// disconnection event, and is deliberately infrequent. In Phase 2.5 this loop
/// ran a full round every five seconds and *was* the trigger, which is why an
/// approved peer could sit idle: nothing connected the decision to the work.
/// Publishes this node's position to authorized peers on a fixed interval.
///
/// # Why its own thread
///
/// Obtaining a position blocks — the Windows location service can take twelve
/// seconds, and a GNSS receiver longer. Doing that on the sync pump, which
/// ticks every hundred milliseconds, would stall replication for the duration
/// of every fix. One thread for the whole node, not one per peer.
///
/// # Why the first publication is not delayed
///
/// A node that waited five minutes before saying where it is would be invisible
/// on a peer's map for the whole of a short demonstration, and for the most
/// useful five minutes of a real deployment.
///
/// # Failure
///
/// A failed fix publishes nothing and advances no sequence. It is retried on a
/// shorter interval, but not so short that a device with no receiver is asked
/// constantly — a location service being unable to answer is a normal state,
/// not an error to hammer at.
fn spawn_location_heartbeat(runtime: Arc<NodeRuntime>) {
    if !runtime.mesh_attached() {
        return;
    }

    const INTERVAL: std::time::Duration = std::time::Duration::from_secs(5 * 60);
    const RETRY: std::time::Duration = std::time::Duration::from_secs(30);

    std::thread::Builder::new()
        .name("securemesh-location".to_string())
        .spawn(move || loop {
            let wait = match runtime.publish_location() {
                Ok(Some(sequence)) => {
                    eprintln!("[securemesh] location published (seq {sequence})");
                    INTERVAL
                }
                // No position available. Nothing was sent, nothing invented,
                // and the sequence is untouched.
                Ok(None) => RETRY,
                Err(error) => {
                    eprintln!(
                        "[securemesh] location heartbeat failed: {}",
                        error.message()
                    );
                    RETRY
                }
            };

            std::thread::sleep(wait);
        })
        .expect("spawn the location heartbeat thread");
}

fn spawn_mesh_loop(runtime: Arc<NodeRuntime>) {
    if !runtime.mesh_attached() {
        return;
    }

    // Short enough that inbound messages are handled promptly; this is a
    // delivery pump, not a retry loop.
    const TICK_MS: u64 = 100;
    const TICK: std::time::Duration = std::time::Duration::from_millis(TICK_MS);
    const RECONCILE_EVERY: u32 = (sync::RECONCILE_INTERVAL_SECS * 1000 / TICK_MS) as u32;

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
                if ticks.is_multiple_of(RECONCILE_EVERY) {
                    if let Ok((0, connected)) = runtime.authorized_peer_count() {
                        if connected > 0 {
                            eprintln!(
                                "[securemesh] {connected} peer(s) connected, none authorized — \
                                 enrollment required before anything is exchanged"
                            );
                        }
                    }

                    if let Err(error) = runtime.reconcile() {
                        eprintln!("[securemesh] reconciliation failed: {}", error.message());
                    }
                }

                std::thread::sleep(TICK);
            }
        })
        .map_err(|e| eprintln!("[securemesh] could not start the sync loop: {e}"))
        .ok();
}
