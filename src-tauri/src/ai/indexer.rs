//! Keeps the local vector index caught up with the incident log.
//!
//! # Why this exists
//!
//! [`IntelligenceService::index_pending`] was always able to embed whatever was
//! missing, but nothing in the running application ever called it. An incident
//! was therefore stored, signed and replicated correctly and remained invisible
//! to retrieval for ever, so "Ask SecureMesh" refused every question with
//! `generation 0 ms` — the model was never even consulted. The gap was a missing
//! *caller*, not missing capability.
//!
//! # Where the work happens
//!
//! ```text
//!   create incident ─┐
//!   apply replicated ├─▶ nudge ─▶ [securemesh-index thread] ─▶ index_pending()
//!   startup / timer ─┘                                              │
//!                                                                   ▼
//!                                                        vectors in SQLite
//! ```
//!
//! Indexing runs on its own thread and is never on the path of a write. A write
//! is committed first and the nudge sent afterwards, so embedding — which needs
//! a model, and can therefore fail or be absent entirely — cannot affect whether
//! an incident is stored or replicated. That ordering is the whole point:
//! **the incident is authoritative, the vector is derived.**
//!
//! # Why a nudge rather than a queue
//!
//! `index_pending` already finds everything that lacks a vector, so the signal
//! carries no payload and duplicates are meaningless. A one-slot channel is
//! enough: if a nudge is already waiting, another changes nothing and is
//! dropped. Nothing can be lost by dropping it, because the *database* is the
//! queue — anything still missing a vector is found by the next pass, whether
//! that pass is triggered by a nudge, the periodic sweep, or the next restart.
//!
//! # The trust boundary is unchanged
//!
//! The thread is handed an [`IntelligenceService`] and nothing else, so it can
//! reach exactly what that service can: the database and the two engines. No
//! identity, no keystore, no trust mutation, no sync control. See [`crate::ai`].

use crate::ai::IntelligenceService;
use crate::error::CoreResult;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{sync_channel, RecvTimeoutError, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How long the worker waits for a nudge before sweeping anyway.
///
/// The sweep is a safety net, not the mechanism: it repairs anything a missed
/// nudge, a crash mid-pass, or a temporarily absent model left behind. Triggers
/// do the timely work.
const SWEEP_INTERVAL: Duration = Duration::from_secs(60);

/// Upper bound on incidents inspected when reporting state to the UI.
const STATE_LIMIT: u32 = 10_000;

/// Where one incident stands with respect to the local vector index.
///
/// Deliberately separate from [`crate::domain::SyncStatus`]: replication and
/// indexing are independent, and an operator who reads "SYNCED" as "searchable"
/// would be misled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum IndexState {
    /// Committed, but no vector yet. The next pass will pick it up.
    NotIndexed,
    /// A pass is working on it now.
    Indexing,
    /// A vector exists; retrieval can find it.
    Indexed,
    /// A pass tried and could not embed it — usually no model. It stays in the
    /// database and is retried; nothing is lost.
    IndexFailed,
}

/// One incident's derived index state, for the UI.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IncidentIndexState {
    pub incident_id: String,
    pub state: IndexState,
}

/// In-flight and failed markers. Absence means "ask the database".
///
/// Held in memory on purpose. `INDEXED` is a fact about stored vectors and is
/// read from SQLite, which is authoritative; `INDEXING` and `INDEX_FAILED` are
/// statements about a pass that is happening *now* and are meaningless after a
/// restart — where the correct reading is simply "not indexed yet", which is
/// what reconciliation then repairs. Persisting them would duplicate state the
/// schema already implies and risk it going stale.
#[derive(Debug, Default)]
struct Transient {
    indexing: HashSet<String>,
    failed: HashSet<String>,
}

/// A background worker that keeps the index current.
pub struct BackgroundIndexer {
    /// Dropping this ends the worker: the receiver disconnects and its loop
    /// exits, so a runtime that goes away does not leak a thread.
    nudge: SyncSender<()>,
    transient: Arc<Mutex<Transient>>,
    service: Arc<IntelligenceService>,
}

impl BackgroundIndexer {
    /// Starts the worker and asks for an immediate reconciliation pass.
    ///
    /// The opening pass is what repairs a node that was interrupted mid-index,
    /// started without a model, or upgraded from a build that never indexed at
    /// all — no manual step is required to make old incidents searchable.
    pub fn start(service: Arc<IntelligenceService>) -> Self {
        // One slot: a pending nudge already guarantees a pass, so a second is
        // redundant rather than lost.
        let (nudge, requests) = sync_channel::<()>(1);
        let transient = Arc::new(Mutex::new(Transient::default()));

        let worker_service = Arc::clone(&service);
        let worker_transient = Arc::clone(&transient);

        let spawned = std::thread::Builder::new()
            .name("securemesh-index".to_string())
            .spawn(move || loop {
                run_pass(&worker_service, &worker_transient);

                match requests.recv_timeout(SWEEP_INTERVAL) {
                    Ok(()) | Err(RecvTimeoutError::Timeout) => continue,
                    // The runtime has gone; so should this thread.
                    Err(RecvTimeoutError::Disconnected) => break,
                }
            });

        if let Err(error) = &spawned {
            // A node without the worker still stores and replicates everything;
            // it simply will not build vectors until something calls
            // `index_pending` directly. Worth saying, never worth failing for.
            eprintln!("[securemesh] could not start the indexing thread: {error}");
        }

        let indexer = Self {
            nudge,
            transient,
            service,
        };
        indexer.request();
        indexer
    }

    /// Asks for a pass. Never blocks, never fails.
    ///
    /// Called immediately after a commit, so it must not be able to delay or
    /// break the write it follows. A full channel means a pass is already
    /// queued, which is the same outcome.
    pub fn request(&self) {
        let _ = self.nudge.try_send(());
    }

    /// The index state of every incident, for the UI.
    pub fn states(&self) -> CoreResult<Vec<IncidentIndexState>> {
        let outstanding: HashSet<String> = self
            .service
            .unindexed_incident_ids(STATE_LIMIT)?
            .into_iter()
            .collect();

        let transient = self.lock_transient();
        let mut states = Vec::new();

        for incident_id in self.service.incident_ids(STATE_LIMIT)? {
            let state = if transient.indexing.contains(&incident_id) {
                IndexState::Indexing
            } else if !outstanding.contains(&incident_id) {
                // The database holds a vector for it: authoritative.
                IndexState::Indexed
            } else if transient.failed.contains(&incident_id) {
                IndexState::IndexFailed
            } else {
                IndexState::NotIndexed
            };
            states.push(IncidentIndexState { incident_id, state });
        }

        Ok(states)
    }

    /// Counts by state, for a compact status line.
    pub fn summary(&self) -> CoreResult<HashMap<String, u64>> {
        let mut counts: HashMap<String, u64> = HashMap::new();
        for entry in self.states()? {
            *counts
                .entry(format!("{:?}", entry.state).to_uppercase())
                .or_insert(0) += 1;
        }
        Ok(counts)
    }

    fn lock_transient(&self) -> std::sync::MutexGuard<'_, Transient> {
        self.transient
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// One reconciliation pass: embed whatever lacks a vector.
///
/// Records which incidents were attempted so the outcome can be reported per
/// incident, then re-reads what is still outstanding rather than trusting the
/// pass's own counters — the database is the authority on what exists.
fn run_pass(service: &Arc<IntelligenceService>, transient: &Arc<Mutex<Transient>>) {
    let attempted: Vec<String> = match service.unindexed_incident_ids(STATE_LIMIT) {
        Ok(ids) => ids,
        Err(error) => {
            eprintln!(
                "[securemesh] index pass could not read state: {}",
                error.message()
            );
            return;
        }
    };

    {
        let mut guard = transient.lock().unwrap_or_else(|p| p.into_inner());
        // Anything being retried is no longer "failed" while it is in flight.
        for id in &attempted {
            guard.failed.remove(id);
            guard.indexing.insert(id.clone());
        }
    }

    // `index_pending` works in batches, so keep going while it makes progress.
    // Stopping on no-progress is what prevents an unavailable model from
    // spinning this thread: the next nudge or sweep retries instead.
    loop {
        match service.index_pending() {
            Ok(report) => {
                if report.chunks_embedded + report.incidents_embedded == 0 {
                    break;
                }
            }
            Err(error) => {
                eprintln!("[securemesh] indexing pass failed: {}", error.message());
                break;
            }
        }
    }

    // Whatever is still missing a vector did not make it this time.
    let still_outstanding: HashSet<String> = match service.unindexed_incident_ids(STATE_LIMIT) {
        Ok(ids) => ids.into_iter().collect(),
        Err(_) => HashSet::new(),
    };

    let mut guard = transient.lock().unwrap_or_else(|p| p.into_inner());
    for id in attempted {
        guard.indexing.remove(&id);
        if still_outstanding.contains(&id) {
            guard.failed.insert(id);
        } else {
            guard.failed.remove(&id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_states_serialise_as_stable_screaming_snake_case() {
        // The UI matches on these strings; renaming one silently would leave it
        // rendering nothing.
        let json = |state: IndexState| serde_json::to_string(&state).unwrap();
        assert_eq!(json(IndexState::NotIndexed), "\"NOT_INDEXED\"");
        assert_eq!(json(IndexState::Indexing), "\"INDEXING\"");
        assert_eq!(json(IndexState::Indexed), "\"INDEXED\"");
        assert_eq!(json(IndexState::IndexFailed), "\"INDEX_FAILED\"");
    }

    #[test]
    fn the_sweep_is_a_safety_net_not_the_mechanism() {
        // If this were short it would be a polling loop, which is what the
        // triggers exist to avoid.
        const { assert!(SWEEP_INTERVAL.as_secs() >= 30) };
    }
}
