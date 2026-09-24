//! The assembled SecureMesh node.
//!
//! [`NodeRuntime`] owns the node's identity and its local database and exposes
//! the operations the UI needs. All decision-making lives here rather than in
//! React: the frontend renders what the runtime reports and never derives
//! status of its own.
//!
//! The runtime is deliberately free of Tauri types so it can be constructed
//! and driven directly from integration tests.

use crate::ai::insight::{self, Candidate, IncidentInsight, SituationBrief};
use crate::ai::nlp;
use crate::ai::{
    BackgroundIndexer, GroundedAnswer, IncidentIndexState, IndexReport, IntelligenceService,
    IntelligenceStatus,
};
use crate::domain::event::{EventKind, IncidentCreatedPayload, IncidentObservationPayload};
use crate::domain::trust::{Capability, PeerRole, TrustEvent, TrustState};
use crate::domain::{Incident, MeshEvent, NewIncident, Observation, SyncStatus};
use crate::error::{CoreError, CoreResult};
use crate::identity::keystore::FileKeyStore;
use crate::identity::{NodeIdentity, PublicIdentity};
use crate::location::{DeviceLocation, LocationPermission, LocationProvider};
use crate::networking::lora_event_codec;
use crate::networking::lora_event_ingest;
use crate::networking::lora_sync_requester::{self, LoraSyncRequesterState};
use crate::networking::lora_sync_responder::{self, PeerSyncState};
use crate::networking::{MeshTransport, PeerDescriptor};
use crate::security::{audit, AuditEvent, AuditOutcome};
use crate::storage::events::ApplyOutcome;
use crate::storage::intelligence::KnowledgeDocument;
use crate::storage::Database;
use crate::sync::{LinkSnapshot, SyncEngine, SyncReport};
use serde::Serialize;
use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;
use uuid::Uuid;

/// File names created inside the node's data directory.
///
/// Public so that a harness can construct the node's identity before the
/// runtime exists — attaching a transport requires knowing the node ID, which
/// is derived from the key.
pub const DATABASE_FILE: &str = "securemesh.sqlite";
pub const KEYSTORE_FILE: &str = "node_identity.json";

/// Largest number of LoRa `SecureMeshEvent` frames processed in one
/// [`NodeRuntime::lora_receive_tick`].
///
/// At the E22's 9600-baud bench configuration, one ~190-byte frame takes
/// roughly 170ms to transmit, so a real link cannot deliver more than a
/// handful of frames between ticks even at a tight polling interval. 8 is
/// comfortably above any realistic single-tick arrival rate while still
/// bounding a flood or replay burst to a small, fixed amount of
/// signature-verification and database work per tick. Frames beyond the cap
/// are not deferred to the next tick — they were already drained from the
/// transport's inbox by the time this limit is applied, so excess frames in
/// one batch are simply discarded, not queued.
pub const LORA_EVENT_RX_TICK_LIMIT: usize = 8;

/// Largest number of `SyncRequest` frames the responder takes off the
/// transport's queue in one [`NodeRuntime::lora_sync_responder_tick`].
///
/// Fixed at 1, not [`LORA_EVENT_RX_TICK_LIMIT`]: unlike an event frame, a
/// sync request can make this node do real work (a database scan and, if
/// accepted, radio transmission), so a queue filled by a hostile or broken
/// peer must not be able to spend more than one tick's worth of that per
/// pass. The queue itself is still bounded independently — see
/// [`crate::networking::lora_transport::MAX_QUEUED_SYNC_REQUESTS`].
pub const LORA_SYNC_REQUESTS_PER_TICK: usize = 1;

/// Largest number of encoded historical event payloads held awaiting
/// transmission at once.
///
/// Equal to the largest single answer ([`lora_sync::MAX_SYNC_REQUEST_EVENTS`]),
/// which is what one accepted request can produce. A second request accepted
/// while the first is still draining queues behind it rather than growing
/// this bound, and any payload that would overflow it is dropped and logged
/// — never silently retried into an unbounded backlog.
pub const LORA_SYNC_OUTBOX_LIMIT: usize =
    crate::networking::lora_sync::MAX_SYNC_REQUEST_EVENTS as usize;

/// Enables LoRa transmission of locally-created events. Must be exactly
/// `"1"` — absent, empty, or any other value leaves it off, and a local
/// event is created exactly as it was before this existed.
///
/// Deliberately opt-in rather than automatic: unlike QUIC, which only
/// activates on a local network the operator already controls, a LoRa
/// transmission leaves the device over open radio, so it is not something a
/// node does by default just because a serial port happened to be attached.
pub const SECUREMESH_LORA_EVENT_TX_ENV: &str = "SECUREMESH_LORA_EVENT_TX";

/// A running SecureMesh node.
pub struct NodeRuntime {
    identity: NodeIdentity,
    /// Shared so the intelligence service can read incidents and write derived
    /// tables without being handed the runtime itself.
    database: Arc<Database>,
    /// Serialises local event creation.
    ///
    /// Allocating a sequence number and storing the event are two separate
    /// database operations. Without this guard, two concurrent local writes
    /// could read the same next sequence number and the second would land as a
    /// self-equivocation — the node accusing itself of forking its own log.
    local_append: Mutex<()>,
    /// Local intelligence, when a model is provisioned.
    ///
    /// An `Option` rather than a field that must be populated: the type says
    /// that a node without AI is a normal node, not a broken one.
    intelligence: Option<Arc<IntelligenceService>>,
    /// Keeps the vector index level with the incident log.
    ///
    /// Present exactly when `intelligence` is. Held here rather than inside the
    /// service because the runtime is what observes the two events worth
    /// indexing after — a local write and an applied replication — and it is
    /// the only place that can guarantee the invariant for *every* caller,
    /// whether that is the GUI, a test, or a future CLI.
    indexer: Option<BackgroundIndexer>,
    /// The mesh, when one is attached. `None` means this node runs standalone,
    /// which is a fully supported mode rather than a failure.
    mesh: Option<Mutex<SyncEngine<Box<dyn MeshTransport>>>>,
    /// This node's own monotonic location counter.
    ///
    /// Increments only after a position is successfully obtained, so a failed
    /// fix leaves no gap and a peer never sees a sequence that stood for
    /// nothing. In memory: it orders the heartbeats of one process lifetime,
    /// and a restart re-announces from 1 against a peer that has no record of
    /// the previous run either.
    location_sequence: std::sync::atomic::AtomicU64,
    /// Where device positions come from.
    ///
    /// Always present, because "this machine cannot report a position" is itself
    /// an answer the UI needs, not a reason to leave the field empty. On a
    /// platform with no provider this is the honest one that says so.
    location: Box<dyn LocationProvider>,
    /// Per-requester freshness/replay/rate-limit state for the LoRa
    /// historical-sync responder. In memory only, and deliberately so — see
    /// [`lora_sync_responder::respond_to_sync_request`]'s own doc comment on
    /// why a restart is allowed to clear it.
    lora_sync_peer_state: Mutex<std::collections::HashMap<String, PeerSyncState>>,
    /// Encoded type-2 event payloads awaiting transmission, one per LoRa
    /// receive tick. Bounded by [`LORA_SYNC_OUTBOX_LIMIT`] so a burst of
    /// large historical answers cannot grow this without limit.
    lora_sync_outbox: Mutex<std::collections::VecDeque<Vec<u8>>>,
    /// Per-origin cooldown state for outgoing LoRa historical-sync requests
    /// (Phase 6 step 4). In memory only — see
    /// [`lora_sync_requester::LoraSyncRequesterState`]'s own doc comment.
    lora_sync_requester_state: Mutex<LoraSyncRequesterState>,
}

impl NodeRuntime {
    /// Brings a node up in `data_dir`, creating its identity and database on
    /// first launch and reusing both thereafter.
    ///
    /// Ordering matters: the identity is established first, because the local
    /// node row is a foreign-key target for every incident this node authors.
    pub fn initialize(data_dir: impl AsRef<Path>) -> CoreResult<Self> {
        Self::initialize_inner(data_dir.as_ref(), None)
    }

    /// Brings a node up with a mesh transport attached.
    ///
    /// Taking the transport as a trait object is what lets the identical
    /// runtime serve the real QUIC mesh and the deterministic test network, so
    /// replication is exercised by tests through exactly the code that ships.
    pub fn initialize_with_transport(
        data_dir: impl AsRef<Path>,
        transport: Box<dyn MeshTransport>,
    ) -> CoreResult<Self> {
        Self::initialize_inner(data_dir.as_ref(), Some(transport))
    }

    fn initialize_inner(
        data_dir: &Path,
        transport: Option<Box<dyn MeshTransport>>,
    ) -> CoreResult<Self> {
        let keystore = FileKeyStore::new(data_dir.join(KEYSTORE_FILE));
        let identity = NodeIdentity::load_or_create(&keystore)?;

        let database = Arc::new(Database::open(data_dir.join(DATABASE_FILE))?);
        database.register_local_node(
            identity.node_id(),
            identity.node_name(),
            &identity.public_key_hex(),
            identity.created_at(),
        )?;

        // No session survives a process exit, so any peer left marked ONLINE by
        // the previous run is stale and would otherwise be reported as reachable.
        database.mark_all_peers_offline()?;

        let runtime = Self {
            identity,
            database,
            local_append: Mutex::new(()),
            mesh: transport.map(|t| Mutex::new(SyncEngine::new(t))),
            location_sequence: std::sync::atomic::AtomicU64::new(0),
            intelligence: None,
            indexer: None,
            location: crate::location::platform_provider(),
            lora_sync_peer_state: Mutex::new(std::collections::HashMap::new()),
            lora_sync_outbox: Mutex::new(std::collections::VecDeque::new()),
            lora_sync_requester_state: Mutex::new(LoraSyncRequesterState::new()),
        };
        runtime.backfill_legacy_incidents()?;
        Ok(runtime)
    }

    /// Processes everything the mesh has delivered since the last call.
    ///
    /// Returns an empty report for a standalone node, so callers need no
    /// special case for running without a network.
    /// Takes a position and publishes it to authorized peers.
    ///
    /// **Blocking**: the platform location service can take seconds to answer,
    /// which is why the caller runs this on its own thread rather than on the
    /// sync pump. A slow fix must not stall replication.
    ///
    /// Returns the sequence published, or `None` when no position was
    /// available. Nothing is invented on failure and the sequence does not
    /// advance — a counter that moved without a position would tell peers a
    /// heartbeat had been missed rather than never made.
    pub fn publish_location(&self) -> CoreResult<Option<u64>> {
        let Some(mesh) = &self.mesh else {
            return Ok(None);
        };

        // The existing provider, asked exactly as the UI asks it. A failure is
        // reported, never substituted.
        let fix = match self.current_location() {
            Ok(fix) => fix,
            Err(_) => return Ok(None),
        };

        let sequence = self
            .location_sequence
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            + 1;

        let reading = crate::domain::LocationReport {
            latitude: fix.latitude,
            longitude: fix.longitude,
            accuracy_meters: fix.accuracy_meters,
            // The record's vocabulary, mapped once in the location module. A
            // wireless fix stays wireless.
            source: fix.source.into(),
            captured_at: fix.captured_at,
            sequence,
        }
        .validated()?;

        let mut engine = mesh.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        engine.broadcast_location(&self.database, &self.identity, reading)?;

        Ok(Some(sequence))
    }

    /// The highest location sequence this node has published.
    pub fn location_sequence(&self) -> u64 {
        self.location_sequence
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Positions peers have reported, with freshness judged now.
    ///
    /// Empty on a node with no mesh. Read-only: nothing here is persisted, and
    /// reading it neither takes a fix nor contacts a peer.
    pub fn peer_locations(&self) -> Vec<crate::domain::PeerLocationView> {
        let Some(mesh) = &self.mesh else {
            return Vec::new();
        };
        let engine = mesh.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        engine.peer_locations(crate::domain::now())
    }

    /// Sends a small diagnostic payload over the mesh transport's LoRa side.
    ///
    /// **Not incident/event synchronisation.** This calls the transport
    /// directly through [`SyncEngine::transport`] rather than
    /// `SyncEngine::send`, so it never touches the sync engine's trust
    /// gating, watermarks, or storage — it either reaches
    /// [`crate::networking::lora_transport::LoraTransport`] or it doesn't.
    /// Fails cleanly (without affecting QUIC) when this node has no mesh
    /// transport attached, or when the attached transport has no LoRa side.
    pub fn send_lora_diagnostic(&self, payload: &[u8]) -> CoreResult<()> {
        let Some(mesh) = &self.mesh else {
            return Err(CoreError::internal("no mesh transport is attached"));
        };
        let engine = mesh.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        engine.transport().send_lora_diagnostic(payload)
    }

    pub fn sync_tick(&self) -> CoreResult<SyncReport> {
        let Some(mesh) = &self.mesh else {
            return Ok(SyncReport::default());
        };
        let report = {
            let mut engine = mesh.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            engine.tick(&self.database, &self.identity)?
        };

        // **Index trigger: replication delivered something.** A vector is
        // derived *local* state, so it is never carried across the mesh — each
        // node embeds its own copy with its own model. Without this, an incident
        // that arrived from a peer would replicate perfectly and still be
        // invisible to that node's own retrieval.
        //
        // The engine lock is released first: indexing must not be attempted
        // while the sync engine is held.
        if report.events_applied > 0 {
            self.request_indexing();
        }

        Ok(report)
    }

    /// Processes `SecureMeshEvent` frames received over this node's LoRa
    /// side, if it has one.
    ///
    /// **Receive-only.** This never sends anything: it neither retransmits a
    /// frame it received nor generates any LoRa traffic as a side effect.
    /// Nothing here duplicates trust or signature logic — every frame is
    /// handed to [`lora_event_ingest::ingest_event_frame`], the same gate
    /// proven in isolation by that module's own test suite, and its verdict
    /// is taken as-is. A rejected frame changes nothing in storage and does
    /// not enroll its claimed sender.
    ///
    /// Returns an empty report for a standalone node or one with no LoRa side
    /// attached, exactly like [`Self::sync_tick`] does for QUIC.
    pub fn lora_receive_tick(&self) -> CoreResult<LoraReceiveReport> {
        let Some(mesh) = &self.mesh else {
            return Ok(LoraReceiveReport::default());
        };

        // Only the transport call needs the mesh lock. It is taken, used, and
        // released here — before any signature verification, trust lookup, or
        // database write — exactly as `sync_tick` releases its lock before
        // indexing. Everything below runs unlocked.
        let frames = {
            let engine = mesh.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            engine.transport().poll_lora_event_frames()
        };

        let mut report = LoraReceiveReport {
            frames_received: frames.len(),
            ..Default::default()
        };

        // Bounded: see `LORA_EVENT_RX_TICK_LIMIT`. Frames beyond the cap were
        // already taken out of the transport's inbox above, so they are
        // discarded here rather than processed or deferred.
        for frame in frames.iter().take(LORA_EVENT_RX_TICK_LIMIT) {
            match lora_event_ingest::ingest_event_frame(
                &self.database,
                self.identity.node_id(),
                frame,
            ) {
                Ok(lora_event_ingest::IngestedEvent {
                    outcome: ApplyOutcome::Stored,
                    origin_seq,
                }) => {
                    report.events_applied += 1;
                    // **Phase 6 step 4: gap-triggered historical sync.** Only
                    // ever reached for an event this call itself just
                    // verified and stored — never for a rejected, duplicate,
                    // or conflicting one. See
                    // `maybe_request_lora_historical_sync`'s own doc comment.
                    self.maybe_request_lora_historical_sync(frame, origin_seq);
                }
                Ok(lora_event_ingest::IngestedEvent {
                    outcome: ApplyOutcome::Duplicate,
                    ..
                }) => report.events_duplicate += 1,
                Ok(lora_event_ingest::IngestedEvent {
                    outcome: ApplyOutcome::Conflict,
                    ..
                }) => report.events_conflicting += 1,
                // Covers a malformed frame, a bad signature, and an
                // unauthorized sender alike: none of them touch storage, and
                // none of them are treated as anything but a rejection.
                Err(error) => {
                    report.frames_rejected += 1;
                    eprintln!(
                        "[securemesh] LoRa event frame rejected: {}",
                        error.message()
                    );
                }
            }
        }

        // Same trigger `sync_tick` uses, and for the same reason: an event
        // applied by any path needs indexing, and this is the one place both
        // paths already agree on calling it from outside their respective
        // locks.
        if report.events_applied > 0 {
            self.request_indexing();
        }

        // **Phase 6 step 3: answer historical sync requests.** Piggybacks on
        // this tick rather than running its own timer — see
        // `lora_sync_responder_tick`'s own doc comment for why one bounded
        // pass here is enough, and why no separate loop is needed.
        self.lora_sync_responder_tick();

        Ok(report)
    }

    /// Serves at most [`LORA_SYNC_REQUESTS_PER_TICK`] queued LoRa
    /// `SyncRequest` frames, then transmits at most one queued historical
    /// event payload.
    ///
    /// **Opt-in, like local event transmission.** Serving a request means
    /// putting this node's own incident data on open radio, so it happens
    /// only when [`SECUREMESH_LORA_EVENT_TX_ENV`] is exactly `"1"` — the same
    /// gate [`Self::maybe_transmit_lora_event`] already applies to a locally
    /// created event. A node running receive-only never answers, and never
    /// even looks at the sync-request queue.
    ///
    /// **Pacing.** At most one historical event frame leaves per tick,
    /// spreading a full multi-frame answer across this loop's existing
    /// 250ms cadence (see `spawn_lora_event_loop` in `lib.rs`) rather than
    /// bursting several frames at the serial port at once. This reuses the
    /// tick interval that already exists for event reception instead of
    /// adding a second timer.
    ///
    /// Infallible from the caller's point of view, for the same reason
    /// `sync_tick`'s own per-message handling is: one rejected or malformed
    /// request must not stop this node from receiving anything else this
    /// tick, or the next.
    fn lora_sync_responder_tick(&self) {
        if std::env::var(SECUREMESH_LORA_EVENT_TX_ENV).as_deref() != Ok("1") {
            return;
        }
        let Some(mesh) = &self.mesh else {
            return;
        };

        let frames = {
            let engine = mesh.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            engine
                .transport()
                .poll_lora_sync_requests(LORA_SYNC_REQUESTS_PER_TICK)
        };

        if let Some(frame) = frames.into_iter().next() {
            let now_ms = crate::domain::now().timestamp_millis();
            let mut peer_state = self
                .lora_sync_peer_state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());

            match lora_sync_responder::respond_to_sync_request(
                &self.database,
                &self.identity,
                &mut peer_state,
                &frame,
                now_ms,
            ) {
                Ok(payloads) => {
                    if !payloads.is_empty() {
                        let mut outbox = self
                            .lora_sync_outbox
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                        for payload in payloads {
                            if outbox.len() >= LORA_SYNC_OUTBOX_LIMIT {
                                eprintln!(
                                    "[securemesh] lora sync: outbox full, dropping a historical \
                                     event frame"
                                );
                                break;
                            }
                            outbox.push_back(payload);
                        }
                    }
                }
                Err(error) => {
                    eprintln!(
                        "[securemesh] lora sync request rejected: {}",
                        error.message()
                    );
                }
            }
        }

        let next = {
            let mut outbox = self
                .lora_sync_outbox
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            outbox.pop_front()
        };
        if let Some(payload) = next {
            let engine = mesh.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Err(error) = engine.transport().send_lora_event_payload(&payload) {
                eprintln!(
                    "[securemesh] lora sync: historical event transmission failed: {}",
                    error.message()
                );
            }
        }
    }

    /// Asks `frame`'s origin for the historical events between this node's
    /// watermark and `origin_seq`, if `origin_seq` reveals a gap.
    ///
    /// Called only from [`Self::lora_receive_tick`], immediately after an
    /// event has been verified and durably stored — never speculatively,
    /// never on a timer, and never merely because a peer exists or LoRa is
    /// available. See [`lora_sync_requester::maybe_request_sync`]'s own doc
    /// comment for the exact precondition and every case it declines to
    /// request for (no gap, unauthorized origin, cooldown).
    ///
    /// **Opt-in, like the responder.** A historical-sync request is itself a
    /// LoRa transmission, so it is gated behind
    /// [`SECUREMESH_LORA_EVENT_TX_ENV`] exactly as
    /// [`Self::lora_sync_responder_tick`] is — a receive-only node detects
    /// the gap (harmlessly; nothing is recorded about the attempt) but never
    /// keys the radio to ask about it.
    fn maybe_request_lora_historical_sync(
        &self,
        frame: &crate::networking::lora_transport::LoraFrame,
        origin_seq: u64,
    ) {
        if std::env::var(SECUREMESH_LORA_EVENT_TX_ENV).as_deref() != Ok("1") {
            return;
        }
        let Some(mesh) = &self.mesh else {
            return;
        };

        let origin = hex::encode(frame.source_node_id);
        let now = std::time::Instant::now();
        let now_ms = crate::domain::now().timestamp_millis();

        let payload = {
            let mut state = self
                .lora_sync_requester_state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            match lora_sync_requester::maybe_request_sync(
                &self.database,
                &self.identity,
                &mut state,
                &origin,
                origin_seq,
                now,
                now_ms,
            ) {
                Ok(payload) => payload,
                Err(error) => {
                    eprintln!(
                        "[securemesh] lora sync: could not build a historical sync request: {}",
                        error.message()
                    );
                    None
                }
            }
        };

        if let Some(payload) = payload {
            let engine = mesh.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Err(error) = engine.transport().send_lora_sync_request(&payload) {
                eprintln!(
                    "[securemesh] lora sync: historical sync request transmission failed: {}",
                    error.message()
                );
            }
        }
    }

    /// How many connected peers are authorized, and how many are connected.
    pub fn authorized_peer_count(&self) -> CoreResult<(usize, usize)> {
        let Some(mesh) = &self.mesh else {
            return Ok((0, 0));
        };
        let engine = mesh.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        engine.authorized_peer_count(&self.database)
    }

    /// Opens a fresh sync round with every connected peer.
    ///
    /// Needed because a session that is already open receives no further
    /// `PeerConnected` event, so events created after it opened would
    /// otherwise wait for a reconnection.
    pub fn request_sync(&self) -> CoreResult<()> {
        let Some(mesh) = &self.mesh else {
            return Ok(());
        };
        let mut engine = mesh.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        engine.sync_all_peers(&self.database, &self.identity)
    }

    /// Peers with an open authenticated session.
    pub fn connected_peers(&self) -> Vec<PeerDescriptor> {
        self.mesh
            .as_ref()
            .map(|mesh| {
                mesh.lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .connected_peers()
            })
            .unwrap_or_default()
    }

    /// Every known peer, with live connection state folded in.
    pub fn list_peers(&self) -> CoreResult<Vec<crate::domain::Peer>> {
        let connected: Vec<String> = self
            .connected_peers()
            .into_iter()
            .map(|peer| peer.node_id)
            .collect();
        self.database.list_peers(&connected)
    }

    // --- Device location -------------------------------------------------
    //
    // A position is a *snapshot* the operator chooses to attach to an incident.
    // Nothing here writes to the database, starts a watch, or emits an audit
    // record: reading a sensor is an observation, not a state change. Once the
    // operator commits the coordinates, they travel as ordinary incident data —
    // validated, signed, replicated — with no separate location channel.

    /// Whether this device will report a position. Never prompts.
    pub fn location_permission(&self) -> LocationPermission {
        self.location.permission()
    }

    /// Asks the platform for location access, prompting if it chooses to.
    ///
    /// Only ever reached from an explicit operator action, so the form can be
    /// opened without a system dialog appearing.
    pub fn request_location_permission(&self) -> LocationPermission {
        self.location.request_permission()
    }

    /// Takes one position fix.
    ///
    /// Fails rather than guessing when the platform cannot answer: a plausible
    /// but invented coordinate is far worse than none, because an operator
    /// cannot tell it apart from a real one.
    pub fn current_location(&self) -> CoreResult<DeviceLocation> {
        self.location.current_location()
    }

    /// Where positions come from on this machine, for diagnostics.
    pub fn location_provider_name(&self) -> &'static str {
        self.location.describe()
    }

    /// Whether a mesh transport is attached to this node.
    pub fn mesh_attached(&self) -> bool {
        self.mesh.is_some()
    }

    /// Whether the attached mesh transport has a LoRa side.
    ///
    /// A mesh transport can exist with no LoRa side (QUIC-only), so this is
    /// deliberately distinct from [`Self::mesh_attached`] — a caller deciding
    /// whether to poll for LoRa events must check this, not just that a mesh
    /// exists.
    pub fn lora_available(&self) -> bool {
        let Some(mesh) = &self.mesh else {
            return false;
        };
        let engine = mesh.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        engine.transport().lora_available()
    }

    // --- Local intelligence (Phase 3) -------------------------------------

    /// Attaches local intelligence.
    ///
    /// Separate from construction on purpose: a node is fully functional before
    /// this is called and remains so if it never is. Nothing below this point
    /// can make incident capture or replication fail.
    pub fn attach_intelligence(&mut self, service: IntelligenceService) {
        let service = Arc::new(service);
        // Starting the worker performs an immediate reconciliation pass, which
        // is what makes incidents stored before this build — or before a model
        // was provisioned — searchable without any manual step.
        self.indexer = Some(BackgroundIndexer::start(Arc::clone(&service)));
        self.intelligence = Some(service);
    }

    /// Asks for a vector-index pass.
    ///
    /// Deliberately infallible and non-blocking. It is called immediately after
    /// a commit, and a write that has already succeeded must not be able to fail
    /// — or be delayed — because a model is slow, missing, or broken. Anything
    /// not indexed now is found by the next pass, because the database is the
    /// queue.
    fn request_indexing(&self) {
        if let Some(indexer) = &self.indexer {
            indexer.request();
        }
    }

    /// Index state for every incident, for the UI.
    ///
    /// Empty when no model is provisioned: a node without intelligence has no
    /// index to report on, which the UI renders as absence rather than failure.
    pub fn incident_index_states(&self) -> CoreResult<Vec<IncidentIndexState>> {
        match &self.indexer {
            Some(indexer) => indexer.states(),
            None => Ok(Vec::new()),
        }
    }

    /// The intelligence service, if one is attached.
    pub fn intelligence(&self) -> Option<Arc<IntelligenceService>> {
        self.intelligence.clone()
    }

    /// Status for the dashboard, including when no service is attached.
    pub fn intelligence_status(&self) -> IntelligenceStatus {
        match &self.intelligence {
            Some(service) => service.status(),
            None => IntelligenceStatus {
                state: "UNAVAILABLE".to_string(),
                detail: "Local intelligence is not configured on this node.".to_string(),
                model_name: None,
                model_id: None,
                quantisation: None,
                inference: "LOCAL".to_string(),
                network_dependency: "NONE".to_string(),
                embedding_model: None,
                analyses_stored: 0,
                documents_indexed: 0,
                chunks_indexed: 0,
                vectors_stored: 0,
            },
        }
    }

    /// Refuses cleanly when intelligence is not available.
    fn require_intelligence(&self) -> CoreResult<Arc<IntelligenceService>> {
        self.intelligence.clone().ok_or_else(|| {
            CoreError::internal(
                "Local intelligence is not available on this node. \
                 Incident capture and synchronisation are unaffected.",
            )
        })
    }

    /// Analyses an incident locally, stores it, and reports what the
    /// deterministic rule layer makes of the model's answer.
    pub fn analyse_incident(
        &self,
        incident_id: &str,
    ) -> CoreResult<crate::ai::consistency::AnalysisOutcome> {
        self.require_intelligence()?.analyse_incident(incident_id)
    }

    /// The stored analysis for an incident, if any, with the rules' verdict.
    pub fn incident_analysis(
        &self,
        incident_id: &str,
    ) -> CoreResult<Option<crate::ai::consistency::AnalysisOutcome>> {
        match &self.intelligence {
            Some(service) => service.analysis_for(incident_id),
            // Absent intelligence means no analysis, not an error: the incident
            // view must render on a node with no model.
            None => Ok(None),
        }
    }

    /// Derived insight about one incident: extracted facts, category,
    /// explainable severity, and related reports.
    ///
    /// Works with no model at all — the rule layer and lexical matching need
    /// only records this node already holds. With a model provisioned,
    /// related reports are matched by vector and a semantic category fallback
    /// is available. Read-only: nothing is stored and nothing is sent.
    pub fn incident_insight(&self, incident_id: &str) -> CoreResult<IncidentInsight> {
        if let Some(service) = &self.intelligence {
            return service.insight(incident_id);
        }

        let started = std::time::Instant::now();
        let target = self.database.get_incident(incident_id)?;
        let incidents = self.database.list_incidents(Some(1_000))?;
        let extractions: Vec<_> = incidents
            .iter()
            .map(|i| nlp::extract(&i.description))
            .collect();
        let target_extraction = nlp::extract(&target.description);
        let others: Vec<Candidate<'_>> = incidents
            .iter()
            .zip(extractions.iter())
            .map(|(incident, extraction)| Candidate {
                incident,
                extraction,
                vector: None,
            })
            .collect();

        Ok(insight::build_insight(
            &Candidate {
                incident: &target,
                extraction: &target_extraction,
                vector: None,
            },
            &others,
            None,
            None,
            false,
            started.elapsed().as_millis() as u64,
        ))
    }

    /// A situation digest across the incidents this node holds.
    ///
    /// The figures never need a model. A prose summary is attempted only when
    /// asked for and only when a model is attached.
    pub fn situation_brief(&self, summarise: bool) -> CoreResult<SituationBrief> {
        if let Some(service) = &self.intelligence {
            return service.situation_brief(summarise);
        }

        let started = std::time::Instant::now();
        let incidents = self
            .database
            .list_incidents(Some(insight::BRIEF_LIMIT as u32))?;
        let extractions: Vec<_> = incidents
            .iter()
            .map(|i| nlp::extract(&i.description))
            .collect();
        let candidates: Vec<Candidate<'_>> = incidents
            .iter()
            .zip(extractions.iter())
            .map(|(incident, extraction)| Candidate {
                incident,
                extraction,
                vector: None,
            })
            .collect();

        let mut brief = insight::build_brief(&candidates, started.elapsed().as_millis() as u64);
        if summarise {
            brief.summary_note = Some(
                "No local model is available on this node, so there is no prose summary. \
                 The figures above are computed without one."
                    .to_string(),
            );
        }
        Ok(brief)
    }

    /// Answers a question from this node's own records.
    pub fn ask_intelligence(
        &self,
        question: &str,
        top_k: Option<usize>,
    ) -> CoreResult<GroundedAnswer> {
        self.require_intelligence()?.ask(question, top_k)
    }

    /// Embeds whatever is not yet indexed.
    pub fn index_intelligence(&self) -> CoreResult<IndexReport> {
        self.require_intelligence()?.index_pending()
    }

    /// Ingests a document into the local knowledge base.
    pub fn ingest_document(
        &self,
        title: &str,
        source: &str,
        source_type: &str,
        text: &str,
    ) -> CoreResult<Option<String>> {
        self.require_intelligence()?
            .ingest_document(title, source, source_type, text)
    }

    /// Documents held in the local knowledge base.
    pub fn knowledge_documents(&self) -> CoreResult<Vec<KnowledgeDocument>> {
        match &self.intelligence {
            Some(service) => service.documents(),
            None => Ok(Vec::new()),
        }
    }

    /// Installs the operational knowledge pack compiled into this binary.
    ///
    /// Requires intelligence, because a document with no way to embed it is not
    /// knowledge this node can retrieve — installing into a node with no model
    /// would report success and change nothing an operator could use.
    pub fn install_operational_knowledge(
        &self,
    ) -> CoreResult<crate::ai::knowledge_pack::InstallReport> {
        let report = self
            .require_intelligence()?
            .install_operational_knowledge()?;

        audit(
            AuditEvent::KnowledgeInstalled,
            AuditOutcome::Success,
            &format!(
                "pack={} installed={} already_present={} chunks={}",
                crate::ai::knowledge_pack::PACK_VERSION,
                report.documents_installed,
                report.documents_already_present,
                report.chunks_created
            ),
        );

        // Newly installed chunks hold no vectors yet. Embedding is derived work
        // on the background indexer, exactly as it is for an incident, so a
        // slow or failing model cannot leave the install half-applied.
        self.request_indexing();
        Ok(report)
    }

    /// What local knowledge this node holds.
    ///
    /// Returns an empty summary rather than an error when no model is present:
    /// a node with no intelligence has no knowledge base, which is a state to
    /// display, not a failure.
    pub fn knowledge_summary(&self) -> CoreResult<crate::ai::KnowledgeBaseSummary> {
        match &self.intelligence {
            Some(service) => service.knowledge_summary(),
            None => Ok(crate::ai::KnowledgeBaseSummary {
                pack_documents_available: crate::ai::knowledge_pack::DOCUMENTS.len(),
                live_incidents_total: self.database.count_incidents()?,
                ..Default::default()
            }),
        }
    }

    // --- Peer authorization (Phase 2.5) -----------------------------------

    /// The role this node's own operator holds.
    pub fn local_role(&self) -> CoreResult<PeerRole> {
        self.database.role_of(self.identity.node_id())
    }

    /// Refuses the operation unless the local operator holds `capability`.
    ///
    /// Every trust-changing entry point goes through here, so authorization is
    /// enforced in the core rather than by whether the UI drew a button. A
    /// denial is audited: an attempt to use authority one does not hold is
    /// worth a record.
    fn require_local_capability(&self, capability: Capability) -> CoreResult<()> {
        let role = self.local_role()?;
        if role.grants(capability) {
            return Ok(());
        }

        audit(
            AuditEvent::AuthorizationDenied,
            AuditOutcome::Failure,
            &format!(
                "capability={capability} role={role} node={}",
                self.identity.node_name()
            ),
        );
        Err(CoreError::validation(format!(
            "this node is not authorized to {capability}; its role is {role}"
        )))
    }

    /// Approves a peer, or reinstates a revoked one.
    ///
    /// Requires [`Capability::PeerEnroll`]. Nothing a peer sends can reach this
    /// path: enrollment decisions originate only from the local operator, which
    /// is what stops an approved peer from promoting itself or anyone else.
    pub fn approve_peer(&self, node_id: &str, note: Option<&str>) -> CoreResult<TrustState> {
        self.require_local_capability(Capability::PeerEnroll)?;
        let state = self.database.approve_peer(&self.identity, node_id, note)?;
        self.notify_authorization_changed(node_id);
        Ok(state)
    }

    /// Refuses a peer that has never been trusted. Requires
    /// [`Capability::PeerEnroll`].
    pub fn reject_peer(&self, node_id: &str, note: Option<&str>) -> CoreResult<TrustState> {
        self.require_local_capability(Capability::PeerEnroll)?;
        let state = self.database.reject_peer(&self.identity, node_id, note)?;
        self.notify_authorization_changed(node_id);
        Ok(state)
    }

    /// Withdraws authorization from a peer. Requires
    /// [`Capability::PeerRevoke`].
    ///
    /// The link is moved out of a sync-capable state immediately, so an open
    /// session stops replicating without waiting for a reconnection.
    pub fn revoke_peer(&self, node_id: &str, note: Option<&str>) -> CoreResult<TrustState> {
        self.require_local_capability(Capability::PeerRevoke)?;
        let state = self.database.revoke_peer(&self.identity, node_id, note)?;

        // A revoked peer's claim about where it is should not linger on the
        // map after the operator has said it is no longer trusted. The
        // position was only ever held on that peer's authority.
        if let Some(mesh) = &self.mesh {
            mesh.lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .forget_peer_location(node_id);
        }

        self.notify_authorization_changed(node_id);
        Ok(state)
    }

    /// Live synchronisation state for every open session.
    pub fn link_states(&self) -> Vec<LinkSnapshot> {
        self.mesh
            .as_ref()
            .map(|mesh| {
                mesh.lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .link_snapshots()
            })
            .unwrap_or_default()
    }

    /// Runs the periodic reconciliation sweep. See
    /// [`crate::sync::RECONCILE_INTERVAL_SECS`].
    pub fn reconcile(&self) -> CoreResult<()> {
        let Some(mesh) = &self.mesh else {
            return Ok(());
        };
        let mut engine = mesh.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        engine.reconcile(&self.database, &self.identity)
    }

    /// The authorization state of a peer.
    pub fn trust_state_of(&self, node_id: &str) -> CoreResult<TrustState> {
        self.database.trust_state_of(node_id)
    }

    /// The local trust audit log, newest first.
    pub fn trust_audit_log(
        &self,
        node_id: Option<&str>,
        limit: u32,
    ) -> CoreResult<Vec<TrustEvent>> {
        self.database.trust_audit_log(node_id, limit)
    }

    /// Sets this node's own role.
    ///
    /// Provisioning hook for locking a field node down to `NODE`, so its
    /// operator cannot enroll peers. On hardware the operator physically
    /// controls this is a policy control, not a cryptographic one — see
    /// `docs/security/SECURITY.md`.
    pub fn set_local_role(&self, role: PeerRole) -> CoreResult<()> {
        self.database.set_local_role(self.identity.node_id(), role)
    }

    /// Gives Phase 1 incidents an event, so an upgraded node can replicate the
    /// records it already holds.
    ///
    /// Only incidents this node authored are eligible: replicating a record
    /// requires signing it, and this node holds no other node's key. In a
    /// Phase 1 database every incident is local, because there was no
    /// networking, so nothing is left behind.
    fn backfill_legacy_incidents(&self) -> CoreResult<()> {
        let pending = self
            .database
            .incidents_awaiting_backfill(self.identity.node_id())?;
        if pending.is_empty() {
            return Ok(());
        }

        let _guard = self.lock_local_append();
        for incident in &pending {
            let sequence = self.database.next_local_sequence(self.identity.node_id())?;
            let event = MeshEvent::create(
                &self.identity,
                sequence,
                EventKind::IncidentCreated,
                IncidentCreatedPayload {
                    incident_id: incident.id.clone(),
                    description: incident.description.clone(),
                    severity: incident.severity.as_str().to_string(),
                    latitude: incident.latitude,
                    longitude: incident.longitude,
                    accuracy_meters: incident.accuracy_meters,
                    location_source: incident.location_source,
                    location_captured_at: incident.location_captured_at,
                },
            )?;
            self.database
                .backfill_incident_event(&event, &incident.id)?;
        }

        audit(
            AuditEvent::IncidentCreated,
            AuditOutcome::Success,
            &format!("backfilled {} pre-Phase-2 incident(s)", pending.len()),
        );
        Ok(())
    }

    /// Takes the local-append guard, recovering it if a previous holder
    /// panicked. The data it protects is a database sequence read, which is
    /// re-derived on every call, so a poisoned lock carries no stale state.
    fn lock_local_append(&self) -> std::sync::MutexGuard<'_, ()> {
        self.local_append
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Signs and applies an event authored by this node, then tells the mesh.
    fn append_local_event(
        &self,
        kind: EventKind,
        payload: impl Serialize,
    ) -> CoreResult<MeshEvent> {
        let event = {
            let _guard = self.lock_local_append();

            let sequence = self.database.next_local_sequence(self.identity.node_id())?;
            let event = MeshEvent::create(&self.identity, sequence, kind, payload)?;
            self.database
                .apply_event(&event, self.identity.node_id(), None)?;
            event
        };

        // **Trigger: a local write.** Without this a record created during an
        // open session would sit until something else happened to start a
        // round. The append lock is released first so a slow mesh cannot block
        // the next local write.
        self.notify_local_event();

        // **LoRa TX, opt-in and best-effort.** This function is the *only*
        // place a local event is created and signed — a QUIC-replicated event
        // reaches storage through `SyncEngine::tick`, and a LoRa-received one
        // through `lora_event_ingest::ingest_event_frame`, and neither of
        // those paths calls back into this one. So gating LoRa TX here, and
        // nowhere else, is what guarantees a replicated or LoRa-received
        // event is never retransmitted over LoRa — there is no code path by
        // which one could reach this line.
        self.maybe_transmit_lora_event(&event);

        Ok(event)
    }

    /// Sends `event` over this node's LoRa side, if TX is enabled
    /// ([`SECUREMESH_LORA_EVENT_TX_ENV`] is exactly `"1"`) and a side is
    /// attached.
    ///
    /// Always called *after* [`append_local_event`](Self::append_local_event)
    /// has already committed `event` to storage, and is infallible from the
    /// caller's point of view for the same reason
    /// [`notify_local_event`](Self::notify_local_event) is: nothing here can
    /// undo a write that already succeeded. An event too large for the
    /// codec's frame budget, a missing LoRa side, or a dead I/O thread all
    /// end the same way — logged and skipped, never propagated as a failure
    /// of the local event that was actually created.
    fn maybe_transmit_lora_event(&self, event: &MeshEvent) {
        if std::env::var(SECUREMESH_LORA_EVENT_TX_ENV).as_deref() != Ok("1") {
            return;
        }
        let Some(mesh) = &self.mesh else {
            return;
        };

        // Encoding is pure local computation — carrying the event's existing
        // signature verbatim, never minting a new one (see
        // `lora_event_codec::encode`'s own doc comment) — so it needs no
        // lock. An event that does not fit the codec's frame budget is
        // refused here, not truncated or fragmented; the database commit
        // above is already durable either way.
        let payload = match lora_event_codec::encode(event) {
            Ok(payload) => payload,
            Err(error) => {
                eprintln!(
                    "[securemesh] LoRa event transmission skipped (event={}): {}",
                    event.event_id,
                    error.message()
                );
                return;
            }
        };

        // Only the transport call itself needs the mesh lock, and only for as
        // long as it takes to hand the frame to the LoRa I/O thread's
        // channel — the same pattern `send_lora_diagnostic` already uses. The
        // actual serial write happens on that thread, not here.
        let result = {
            let engine = mesh.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            engine.transport().send_lora_event_payload(&payload)
        };

        if let Err(error) = result {
            eprintln!(
                "[securemesh] LoRa event transmission failed (event={}): {}",
                event.event_id,
                error.message()
            );
        }
    }

    /// Opens a round with connected peers because local state changed.
    ///
    /// Deliberately infallible from the caller's point of view: a write has
    /// already been committed durably, and a mesh that cannot be reached is a
    /// normal field condition, not a reason to fail the write. The event log is
    /// the queue, so nothing is lost — the next trigger or reconciliation
    /// carries it.
    fn notify_local_event(&self) {
        let Some(mesh) = &self.mesh else {
            return;
        };
        let mut engine = mesh.lock().unwrap_or_else(|poisoned| poisoned.into_inner());

        if let Err(error) = engine.on_local_event(&self.database, &self.identity) {
            eprintln!(
                "[securemesh] could not announce a local event: {}",
                error.message()
            );
        }
    }

    /// Tells the mesh that a peer's authorization changed.
    ///
    /// **Trigger: authorization.** This is the fix for the observed failure:
    /// approving a peer that is already connected starts replication from the
    /// decision itself, rather than leaving it to a periodic sweep.
    fn notify_authorization_changed(&self, peer_node_id: &str) {
        let Some(mesh) = &self.mesh else {
            return;
        };
        let mut engine = mesh.lock().unwrap_or_else(|poisoned| poisoned.into_inner());

        if let Err(error) =
            engine.on_authorization_changed(&self.database, &self.identity, peer_node_id)
        {
            eprintln!(
                "[securemesh] could not act on an authorization change: {}",
                error.message()
            );
        }
    }

    /// The operator-facing node name, for startup logging and diagnostics.
    pub fn node_name(&self) -> &str {
        self.identity.node_name()
    }

    /// The node's public identity. Never includes private key material.
    ///
    /// # Why this is not audited
    ///
    /// It is a pure read of data that is immutable for the life of the process,
    /// contains no secret, and is already broadcast to every peer on the link by
    /// mDNS. Recording it told an operator nothing they could act on.
    ///
    /// It was audited once, and the dashboard polled it twice every two seconds
    /// — about 86,000 records a day, enough to bury a revocation. The moment
    /// worth auditing is when the identity is *created* or *loaded*, and both
    /// still are. See [`crate::security::audit`].
    pub fn public_identity(&self) -> PublicIdentity {
        self.identity.public()
    }

    /// Validates and stores a new incident authored by this node.
    ///
    /// The incident is written by **appending an event**, not by inserting a
    /// row: local creation and remote replication then travel the identical
    /// apply path, so anything true of a replicated incident is true of a
    /// locally created one.
    pub fn create_incident(&self, input: NewIncident) -> CoreResult<Incident> {
        let validated = input.validate(self.identity.node_id())?;

        let event = self.append_local_event(
            EventKind::IncidentCreated,
            IncidentCreatedPayload {
                incident_id: validated.id.clone(),
                description: validated.description.clone(),
                severity: validated.severity.as_str().to_string(),
                latitude: validated.latitude,
                longitude: validated.longitude,
                accuracy_meters: validated.accuracy_meters,
                location_source: validated.location_source,
                location_captured_at: validated.location_captured_at,
            },
        )?;

        audit(
            AuditEvent::IncidentCreated,
            AuditOutcome::Success,
            &format!("id={} event={}", validated.id, event.event_id),
        );

        // **Index trigger: a local write.** Strictly after the commit above, so
        // the incident is already durable and replicable. Embedding is derived
        // work and happens on another thread; this call cannot block or fail
        // the creation it follows.
        self.request_indexing();

        // Read back the projection rather than returning the validated value,
        // so the caller sees exactly what was persisted.
        self.database.get_incident(&validated.id)
    }

    /// Appends an observation to an existing incident.
    ///
    /// Observations are how SecureMesh handles updates in Phase 2. Nothing is
    /// ever mutated, so two nodes writing about the same incident while
    /// partitioned produce two observations that merge by union — there is no
    /// conflict to resolve and no update to lose.
    pub fn add_observation(&self, incident_id: &str, note: &str) -> CoreResult<()> {
        let note = note.trim();
        if note.is_empty() {
            return Err(CoreError::validation("observation must not be empty"));
        }
        if note.chars().count() > crate::domain::incident::MAX_DESCRIPTION_CHARS {
            return Err(CoreError::validation("observation is too long"));
        }

        // Fails with NOT_FOUND if the incident is unknown, so an observation
        // cannot be attached to nothing.
        self.database.get_incident(incident_id)?;

        self.append_local_event(
            EventKind::IncidentObservation,
            IncidentObservationPayload {
                observation_id: Uuid::new_v4().to_string(),
                incident_id: incident_id.to_string(),
                note: note.to_string(),
            },
        )?;
        Ok(())
    }

    pub fn list_incidents(&self, limit: Option<u32>) -> CoreResult<Vec<Incident>> {
        self.database.list_incidents(limit)
    }

    pub fn get_incident(&self, id: &str) -> CoreResult<Incident> {
        self.database.get_incident(id)
    }

    /// Observations appended to an incident, from any node.
    pub fn list_observations(&self, incident_id: &str) -> CoreResult<Vec<Observation>> {
        self.database.list_observations(incident_id)
    }

    /// A health summary of every subsystem, for the dashboard.
    ///
    /// Subsystems that do not exist yet report exactly that. The runtime never
    /// reports a capability SecureMesh does not currently have.
    pub fn system_status(&self) -> SystemStatus {
        let database = match (
            self.database.health_check(),
            self.database.count_incidents(),
        ) {
            (Ok(()), Ok(count)) => ComponentStatus::operational(
                "Healthy",
                format!(
                    "{count} incident(s) stored, schema v{}",
                    self.database.schema_version().unwrap_or(0)
                ),
            ),
            _ => ComponentStatus::degraded(
                "Unavailable",
                "The local database did not respond to a health check.".to_string(),
            ),
        };

        let identity = ComponentStatus::operational(
            "Active",
            format!(
                "{} keypair held by the {} keystore",
                self.identity.public().algorithm,
                self.identity.public().key_backend
            ),
        );

        SystemStatus {
            database,
            identity,
            network: match (self.mesh_attached(), self.connected_peers().len()) {
                (false, _) => ComponentStatus::inactive(
                    "Offline",
                    "No mesh transport is attached. This node operates standalone.".to_string(),
                ),
                // "Nothing is connected" and "nothing was ever found" are
                // different states, and saying the second when the first is true
                // sent an operator hunting for a discovery fault that did not
                // exist. If this node already holds peer records, say so.
                (true, 0) => match self.database.count_known_peers().unwrap_or(0) {
                    0 => ComponentStatus::inactive(
                        "Listening",
                        "Encrypted QUIC mesh is running. No peers discovered yet — this is \
                         normal when operating alone."
                            .to_string(),
                    ),
                    known => ComponentStatus::degraded(
                        "Disconnected",
                        format!(
                            "Encrypted QUIC mesh is running. {known} known peer(s), none \
                             currently connected — records are held locally until one is \
                             reachable again."
                        ),
                    ),
                },
                (true, count) => ComponentStatus::operational(
                    "Connected",
                    format!(
                        "{count} authenticated peer(s) over encrypted QUIC. Discovery is local; \
                         no server is involved."
                    ),
                ),
            },
            ai: {
                let status = self.intelligence_status();
                match status.state.as_str() {
                    "READY" => ComponentStatus::operational(
                        "Ready",
                        format!(
                            "{} running locally. No network dependency.",
                            status.model_name.as_deref().unwrap_or("Local model")
                        ),
                    ),
                    "LOADING" => ComponentStatus::inactive("Loading", status.detail),
                    // Unavailable intelligence is not a degraded node: the rest
                    // of SecureMesh is unaffected, so it reads as inactive.
                    _ => ComponentStatus::inactive("Unavailable", status.detail),
                }
            },
            // Phase 5. Claiming otherwise on a normal OS process would be false.
            // Reports the *provider and permission* state only. Reading this
            // never takes a fix: a status row that woke the GPS on every
            // dashboard refresh would drain a field device for nothing.
            location: match self.location_permission() {
                LocationPermission::Granted => ComponentStatus::operational(
                    "Available",
                    format!(
                        "{}. Captured only when requested.",
                        self.location_provider_name()
                    ),
                ),
                LocationPermission::NotRequested => ComponentStatus::inactive(
                    "Not requested",
                    format!(
                        "{}. No position has been requested on this node.",
                        self.location_provider_name()
                    ),
                ),
                LocationPermission::Denied => ComponentStatus::degraded(
                    "Denied",
                    "Location access was refused. Coordinates can still be entered by hand."
                        .to_string(),
                ),
                LocationPermission::Unavailable => ComponentStatus::inactive(
                    "Unavailable",
                    format!(
                        "{}. Coordinates can be entered by hand.",
                        self.location_provider_name()
                    ),
                ),
            },
            // Reports whether *geographic data* is installed, which is a
            // different question from whether the network is reachable. The map
            // is offline either way; without a basemap it draws a coordinate
            // grid with real markers rather than nothing.
            map: match crate::map::describe() {
                Ok(basemap) => ComponentStatus::operational(
                    "Ready",
                    format!(
                        "Offline geographic data available: {} feature(s) from {}.",
                        basemap.feature_count, basemap.name
                    ),
                ),
                // Absence is a normal state an operator resolves, not a fault.
                Err(reason) if reason.is_absence() => ComponentStatus::inactive(
                    "Not provisioned",
                    "Map data has not been installed on this node. Incident and node                      positions are still shown on a coordinate grid."
                        .to_string(),
                ),
                Err(reason) => ComponentStatus::degraded("Error", reason.detail()),
            },
            tee: ComponentStatus::inactive(
                "Not available",
                "No trusted execution environment is in use. Keys are held in software."
                    .to_string(),
            ),
        }
    }

    /// The provisioned basemap description, or why there is none.
    ///
    /// Separate from [`Self::map_geojson`] so the dashboard can poll status
    /// without pulling geometry across IPC.
    pub fn map_basemap(&self) -> Option<crate::map::Basemap> {
        crate::map::describe().ok()
    }

    /// The provisioned basemap geometry, for the renderer.
    pub fn map_geojson(&self) -> CoreResult<String> {
        crate::map::geojson()
    }

    /// Mesh connectivity as it actually stands.
    pub fn network_status(&self) -> CoreResult<NetworkStatus> {
        let connected = self.connected_peers().len() as u64;
        let known_peers = self.database.count_known_peers()?;

        let detail = if !self.mesh_attached() {
            "No mesh transport is attached. This node operates standalone and stores \
             everything locally."
                .to_string()
        } else if connected > 0 {
            format!("Connected to {connected} peer(s) over an encrypted local mesh.")
        } else {
            "Listening for peers on the local network. Records are held locally until a \
             peer is reachable."
                .to_string()
        };

        Ok(NetworkStatus {
            // Reachability means at least one authenticated session, not merely
            // that a transport exists.
            online: connected > 0,
            connected_peers: connected,
            known_peers,
            pending_sync: self
                .database
                .count_incidents_by_sync_status(SyncStatus::Pending)?,
            transport: if self.mesh_attached() { "quic" } else { "none" },
            detail,
        })
    }

    /// Test and diagnostic access to the underlying database.
    pub fn database(&self) -> &Database {
        self.database.as_ref()
    }

    /// A shared handle to the database, for the intelligence service.
    ///
    /// Intelligence reads incidents and writes only derived tables; it holds no
    /// identity, keystore, or sync handle. See `crate::ai` for the boundary.
    pub fn database_handle(&self) -> Arc<Database> {
        Arc::clone(&self.database)
    }
}

/// Whether a subsystem is working, impaired, or absent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum ComponentState {
    /// Present and working.
    Operational,
    /// Present but not working correctly.
    Degraded,
    /// Not present in this build, or deliberately not running.
    Inactive,
}

/// The reported state of one subsystem.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComponentStatus {
    pub state: ComponentState,
    /// Short label for the dashboard, e.g. "Healthy".
    pub label: String,
    /// One sentence explaining the state, shown as supporting text.
    pub detail: String,
}

impl ComponentStatus {
    fn operational(label: &str, detail: String) -> Self {
        Self {
            state: ComponentState::Operational,
            label: label.to_string(),
            detail,
        }
    }

    fn degraded(label: &str, detail: String) -> Self {
        Self {
            state: ComponentState::Degraded,
            label: label.to_string(),
            detail,
        }
    }

    fn inactive(label: &str, detail: String) -> Self {
        Self {
            state: ComponentState::Inactive,
            label: label.to_string(),
            detail,
        }
    }
}

/// Health of every subsystem the dashboard reports on.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemStatus {
    pub database: ComponentStatus,
    pub identity: ComponentStatus,
    pub network: ComponentStatus,
    pub ai: ComponentStatus,
    pub location: ComponentStatus,
    /// Whether offline geographic data is installed. Unrelated to network
    /// reachability — the map never uses the network either way.
    pub map: ComponentStatus,
    pub tee: ComponentStatus,
}

/// Mesh connectivity summary.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkStatus {
    pub online: bool,
    pub connected_peers: u64,
    pub known_peers: u64,
    /// Incidents created locally that no peer has yet received.
    pub pending_sync: u64,
    /// Transport currently carrying mesh traffic; `"none"` in Phase 1.
    pub transport: &'static str,
    pub detail: String,
}

/// Outcome of one [`NodeRuntime::lora_receive_tick`], for tests and logging.
///
/// Mirrors [`SyncReport`]'s shape for the fields that have a LoRa-side
/// equivalent; there is no `peers_connected`/`peers_disconnected` because
/// LoRa never surfaces a peer session (see
/// [`MeshTransport::poll_lora_event_frames`]).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct LoraReceiveReport {
    /// Frames taken from the transport's event queue this tick, before the
    /// per-tick cap was applied.
    pub frames_received: usize,
    /// Events accepted and newly stored.
    pub events_applied: usize,
    /// Events already held, and therefore ignored.
    pub events_duplicate: usize,
    /// Equivocations detected: the origin signed two different events at the
    /// same sequence number.
    pub events_conflicting: usize,
    /// Frames refused before storage: malformed, unauthorized sender, or a
    /// bad signature.
    pub frames_rejected: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn input(description: &str, severity: &str) -> NewIncident {
        NewIncident {
            description: description.to_string(),
            severity: severity.to_string(),
            latitude: None,
            longitude: None,
            accuracy_meters: None,
            location_source: None,
            location_captured_at: None,
        }
    }

    #[test]
    fn initialising_creates_an_identity_and_a_database() {
        let dir = TempDir::new().unwrap();
        let node = NodeRuntime::initialize(dir.path()).unwrap();

        assert!(dir.path().join(KEYSTORE_FILE).exists());
        assert!(dir.path().join(DATABASE_FILE).exists());
        assert!(node.public_identity().node_name.starts_with("SM-"));
    }

    // --- Phase 2: LoRa diagnostic path --------------------------------------
    //
    // `LoopbackTransport` stands in for QUIC here, exactly as the rest of the
    // codebase tests mesh behaviour without a real network. What is under
    // test is the wiring — that a `CompositeTransport` behaves like its QUIC
    // side when LoRa is absent, and that the diagnostic call reaches the
    // transport without touching sync/storage — not libp2p itself, which
    // Phase 1 already left untouched.

    #[test]
    fn a_node_with_no_lora_side_still_operates_normally_over_its_mesh_transport() {
        use crate::networking::composite_transport::CompositeTransport;
        use crate::networking::loopback::LoopbackNetwork;

        let dir = TempDir::new().unwrap();
        let identity = NodeIdentity::load_or_create(&crate::identity::keystore::FileKeyStore::new(
            dir.path().join(KEYSTORE_FILE),
        ))
        .unwrap();

        let network = LoopbackNetwork::new();
        let quic = network.attach(identity.node_id(), &identity.public_key_hex());
        let composite = CompositeTransport::new(quic, None);

        let node = NodeRuntime::initialize_with_transport(dir.path(), Box::new(composite)).unwrap();

        // Ordinary mesh-dependent operations work exactly as with a bare
        // transport: no mesh peers yet, no error, nothing LoRa-shaped leaks
        // through.
        assert_eq!(node.sync_tick().unwrap().events_applied, 0);
        assert!(node.peer_locations().is_empty());
    }

    #[test]
    fn diagnostic_request_without_lora_returns_a_clean_error() {
        use crate::networking::composite_transport::CompositeTransport;
        use crate::networking::loopback::LoopbackNetwork;

        let dir = TempDir::new().unwrap();
        let identity = NodeIdentity::load_or_create(&crate::identity::keystore::FileKeyStore::new(
            dir.path().join(KEYSTORE_FILE),
        ))
        .unwrap();

        let network = LoopbackNetwork::new();
        let quic = network.attach(identity.node_id(), &identity.public_key_hex());
        let composite = CompositeTransport::new(quic, None);
        let node = NodeRuntime::initialize_with_transport(dir.path(), Box::new(composite)).unwrap();

        assert!(node
            .send_lora_diagnostic(b"SECUREMESH-LORA-RUST-TEST-01")
            .is_err());
    }

    #[test]
    fn a_node_with_no_mesh_transport_fails_the_lora_diagnostic_cleanly() {
        let dir = TempDir::new().unwrap();
        let node = NodeRuntime::initialize(dir.path()).unwrap();
        assert!(node.send_lora_diagnostic(b"test").is_err());
    }

    // --- `lora_available`: the receive loop's startup condition ------------

    #[test]
    fn lora_is_unavailable_on_a_node_with_no_mesh_transport() {
        let dir = TempDir::new().unwrap();
        let node = NodeRuntime::initialize(dir.path()).unwrap();
        assert!(!node.lora_available());
    }

    #[test]
    fn lora_is_unavailable_on_a_quic_only_node() {
        // A mesh transport attached with no LoRa side — the case
        // `mesh_attached()` alone cannot distinguish from a genuine LoRa
        // attachment, which is why the receive loop must check this instead.
        use crate::networking::composite_transport::CompositeTransport;
        use crate::networking::loopback::LoopbackNetwork;

        let dir = TempDir::new().unwrap();
        let identity =
            NodeIdentity::load_or_create(&FileKeyStore::new(dir.path().join(KEYSTORE_FILE)))
                .unwrap();
        let network = LoopbackNetwork::new();
        let quic = network.attach(identity.node_id(), &identity.public_key_hex());
        let composite = CompositeTransport::new(quic, None);
        let node = NodeRuntime::initialize_with_transport(dir.path(), Box::new(composite)).unwrap();

        assert!(node.mesh_attached());
        assert!(!node.lora_available());
    }

    // --- Stage 4A: LoRa SecureMeshEvent receive path -------------------------
    //
    // Each test here builds a real `CompositeTransport<LoopbackTransport>`
    // with a real `LoraTransport` behind a `MockSerial`, so `lora_receive_tick`
    // runs through every production layer it would in the field — the I/O
    // thread, the Diagnostic/SecureMeshEvent queue split, `lora_event_codec`,
    // `lora_event_ingest`'s trust gate, and `Database::apply_event` — with
    // only the physical serial device mocked out. No COM port, serial device,
    // or RF is touched anywhere below; `MockSerial` is pure in-memory bytes,
    // and genuine test identities are built the same way every other module
    // in this codebase already builds them.

    use crate::networking::composite_transport::CompositeTransport;
    use crate::networking::loopback::LoopbackNetwork;
    use crate::networking::lora_transport::test_support::MockSerial;
    use crate::networking::lora_transport::{LoraFrame, LoraMessageType, LoraTransport};

    fn identity_in(dir: &TempDir) -> NodeIdentity {
        NodeIdentity::load_or_create(&FileKeyStore::new(dir.path().join("id.json"))).unwrap()
    }

    /// A node with a real, mock-backed LoRa side attached. The returned
    /// `MockSerial` is how a test injects inbound bytes and inspects what the
    /// node wrote — the only stand-in for hardware anywhere in this section.
    fn node_with_lora(dir: &TempDir) -> (NodeRuntime, MockSerial) {
        let identity =
            NodeIdentity::load_or_create(&FileKeyStore::new(dir.path().join(KEYSTORE_FILE)))
                .unwrap();

        let network = LoopbackNetwork::new();
        let quic = network.attach(identity.node_id(), &identity.public_key_hex());

        let mock = MockSerial::default();
        let lora = LoraTransport::open_with_io(mock.clone(), identity.node_id().to_string());
        let composite = CompositeTransport::new(quic, Some(lora));

        let node = NodeRuntime::initialize_with_transport(dir.path(), Box::new(composite)).unwrap();
        (node, mock)
    }

    /// A genuine Ed25519-signed `IncidentCreated` event from `signer`,
    /// encoded through the real `lora_event_codec` and wrapped in a real
    /// `SecureMeshEvent` LoRa frame's wire bytes — ready to queue into a
    /// `MockSerial`.
    fn signed_event_frame_bytes(signer: &NodeIdentity, seq: u64, description: &str) -> Vec<u8> {
        let event = MeshEvent::create(
            signer,
            seq,
            EventKind::IncidentCreated,
            IncidentCreatedPayload {
                incident_id: Uuid::new_v4().to_string(),
                description: description.to_string(),
                severity: "HIGH".to_string(),
                latitude: None,
                longitude: None,
                accuracy_meters: None,
                location_source: crate::domain::LocationSource::Unknown,
                location_captured_at: None,
            },
        )
        .unwrap();

        let mut source_node_id = [0u8; 32];
        source_node_id.copy_from_slice(&hex::decode(signer.node_id()).unwrap());

        let frame = LoraFrame {
            message_type: LoraMessageType::SecureMeshEvent,
            source_node_id,
            sequence: seq as u32,
            payload: lora_event_codec::encode(&event).unwrap(),
        };
        frame.encode().unwrap()
    }

    /// Ticks `lora_receive_tick` until it observes at least one frame (or a
    /// two-second timeout), accumulating every tick's counters. The mock I/O
    /// thread decodes asynchronously, so a single immediate tick can race
    /// ahead of it; this is the same "poll until it shows up" pattern the
    /// transport-level tests use for the same reason.
    fn drain_lora_events(node: &NodeRuntime) -> LoraReceiveReport {
        let mut total = LoraReceiveReport::default();
        let mut waited = std::time::Duration::ZERO;
        loop {
            let report = node.lora_receive_tick().unwrap();
            total.frames_received += report.frames_received;
            total.events_applied += report.events_applied;
            total.events_duplicate += report.events_duplicate;
            total.events_conflicting += report.events_conflicting;
            total.frames_rejected += report.frames_rejected;

            if report.frames_received > 0 || waited >= std::time::Duration::from_secs(2) {
                return total;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
            waited += std::time::Duration::from_millis(10);
        }
    }

    #[test]
    fn lora_is_available_on_a_node_with_a_lora_side_attached() {
        let dir = TempDir::new().unwrap();
        let (node, _mock) = node_with_lora(&dir);
        assert!(node.lora_available());
    }

    // --- 1 & 3: a trusted, signed event reaches ingest and storage ---------

    #[test]
    fn a_trusted_signed_lora_event_is_applied_through_the_runtime_receive_path() {
        let dir = TempDir::new().unwrap();
        let (node, mock) = node_with_lora(&dir);

        let peer_dir = TempDir::new().unwrap();
        let peer = identity_in(&peer_dir);
        node.database()
            .register_peer(peer.node_id(), &peer.public_key_hex(), None)
            .unwrap();
        node.approve_peer(peer.node_id(), None).unwrap();

        mock.queue_inbound(&signed_event_frame_bytes(&peer, 1, "Bridge out on NH44"));
        let report = drain_lora_events(&node);

        assert_eq!(report.events_applied, 1);
        assert_eq!(node.database().count_events().unwrap(), 1);

        let incidents = node.list_incidents(None).unwrap();
        assert_eq!(incidents.len(), 1);
        assert_eq!(incidents[0].description, "Bridge out on NH44");
    }

    // --- 4: unknown / pending / revoked senders are rejected ----------------

    #[test]
    fn unknown_pending_and_revoked_lora_senders_are_rejected_by_the_runtime_path() {
        let dir = TempDir::new().unwrap();
        let (node, mock) = node_with_lora(&dir);

        // Unknown: never registered.
        let unknown_dir = TempDir::new().unwrap();
        let unknown = identity_in(&unknown_dir);
        mock.queue_inbound(&signed_event_frame_bytes(&unknown, 1, "unknown"));
        assert_eq!(drain_lora_events(&node).frames_rejected, 1);

        // Pending: registered, but never approved.
        let pending_dir = TempDir::new().unwrap();
        let pending = identity_in(&pending_dir);
        node.database()
            .register_peer(pending.node_id(), &pending.public_key_hex(), None)
            .unwrap();
        mock.queue_inbound(&signed_event_frame_bytes(&pending, 1, "pending"));
        assert_eq!(drain_lora_events(&node).frames_rejected, 1);

        // Revoked: was trusted, then withdrawn.
        let revoked_dir = TempDir::new().unwrap();
        let revoked = identity_in(&revoked_dir);
        node.database()
            .register_peer(revoked.node_id(), &revoked.public_key_hex(), None)
            .unwrap();
        node.approve_peer(revoked.node_id(), None).unwrap();
        node.revoke_peer(revoked.node_id(), None).unwrap();
        mock.queue_inbound(&signed_event_frame_bytes(&revoked, 1, "revoked"));
        assert_eq!(drain_lora_events(&node).frames_rejected, 1);

        // None of the three left a trace in the event log.
        assert_eq!(node.database().count_events().unwrap(), 0);
    }

    // --- 5: a malformed SecureMeshEvent frame does not crash the runtime ---

    #[test]
    fn a_malformed_securemesh_event_frame_does_not_crash_the_runtime() {
        let dir = TempDir::new().unwrap();
        let (node, mock) = node_with_lora(&dir);

        // A trusted sender, so this exercises `lora_event_codec::decode`'s own
        // failure path (an unsupported codec version byte) rather than being
        // rejected earlier by the trust gate.
        let peer_dir = TempDir::new().unwrap();
        let peer = identity_in(&peer_dir);
        node.database()
            .register_peer(peer.node_id(), &peer.public_key_hex(), None)
            .unwrap();
        node.approve_peer(peer.node_id(), None).unwrap();

        let mut source_node_id = [0u8; 32];
        source_node_id.copy_from_slice(&hex::decode(peer.node_id()).unwrap());
        let frame = LoraFrame {
            message_type: LoraMessageType::SecureMeshEvent,
            source_node_id,
            sequence: 1,
            payload: vec![0xFF; 20], // not a valid compact-codec event
        };
        mock.queue_inbound(&frame.encode().unwrap());

        let report = drain_lora_events(&node);
        assert_eq!(report.frames_rejected, 1);
        assert_eq!(report.events_applied, 0);
        assert_eq!(node.database().count_events().unwrap(), 0);
    }

    // --- 6: duplicate delivery follows existing apply_event semantics ------

    #[test]
    fn duplicate_lora_events_follow_existing_apply_event_semantics() {
        let dir = TempDir::new().unwrap();
        let (node, mock) = node_with_lora(&dir);

        let peer_dir = TempDir::new().unwrap();
        let peer = identity_in(&peer_dir);
        node.database()
            .register_peer(peer.node_id(), &peer.public_key_hex(), None)
            .unwrap();
        node.approve_peer(peer.node_id(), None).unwrap();

        let bytes = signed_event_frame_bytes(&peer, 1, "dup");

        mock.queue_inbound(&bytes);
        assert_eq!(drain_lora_events(&node).events_applied, 1);

        mock.queue_inbound(&bytes);
        let second = drain_lora_events(&node);
        assert_eq!(second.events_duplicate, 1);
        assert_eq!(second.events_applied, 0);

        assert_eq!(node.database().count_events().unwrap(), 1);
    }

    // --- 7: receiving never causes a LoRa transmission ----------------------

    #[test]
    fn receiving_a_lora_event_generates_no_lora_transmission() {
        let dir = TempDir::new().unwrap();
        let (node, mock) = node_with_lora(&dir);

        let peer_dir = TempDir::new().unwrap();
        let peer = identity_in(&peer_dir);
        node.database()
            .register_peer(peer.node_id(), &peer.public_key_hex(), None)
            .unwrap();
        node.approve_peer(peer.node_id(), None).unwrap();

        mock.queue_inbound(&signed_event_frame_bytes(&peer, 1, "no echo"));
        assert_eq!(drain_lora_events(&node).events_applied, 1);

        assert!(
            mock.written.lock().unwrap().is_empty(),
            "receiving a LoRa event must never generate LoRa transmission"
        );
    }

    // --- 8: MeshTransport::poll_events semantics are unchanged --------------

    #[test]
    fn poll_events_never_surfaces_a_received_lora_event() {
        let dir = TempDir::new().unwrap();
        let (node, mock) = node_with_lora(&dir);

        let peer_dir = TempDir::new().unwrap();
        let peer = identity_in(&peer_dir);
        node.database()
            .register_peer(peer.node_id(), &peer.public_key_hex(), None)
            .unwrap();
        node.approve_peer(peer.node_id(), None).unwrap();

        mock.queue_inbound(&signed_event_frame_bytes(&peer, 1, "not a peer event"));
        assert_eq!(drain_lora_events(&node).events_applied, 1);

        // `sync_tick` drains `MeshTransport::poll_events()` through the
        // QUIC-facing engine. A LoRa-applied event must never surface there
        // as a PeerConnected/MessageReceived, or move any of its counters.
        assert_eq!(node.sync_tick().unwrap(), SyncReport::default());
        assert!(node.connected_peers().is_empty());
    }

    // --- 9: the per-tick processing cap is enforced -------------------------

    #[test]
    fn the_per_tick_lora_processing_cap_is_enforced() {
        let dir = TempDir::new().unwrap();
        let (node, mock) = node_with_lora(&dir);

        // An unregistered sender: cheap to construct per frame, and every one
        // is rejected at the trust gate — this test is about the cap, not
        // about re-proving the crypto/trust path already covered above.
        let stranger_dir = TempDir::new().unwrap();
        let stranger = identity_in(&stranger_dir);

        let frame_count = LORA_EVENT_RX_TICK_LIMIT + 4;
        let mut wire = Vec::new();
        for seq in 1..=frame_count as u64 {
            wire.extend(signed_event_frame_bytes(&stranger, seq, "flood"));
        }
        mock.queue_inbound(&wire);

        // All frames are queued at once; give the I/O thread time to decode
        // every one of them into the event inbox before the single tick
        // below drains it, so this measures the processing cap and not a
        // race with the decoder.
        std::thread::sleep(std::time::Duration::from_millis(200));

        let report = node.lora_receive_tick().unwrap();
        assert_eq!(report.frames_received, frame_count);

        let processed = report.events_applied
            + report.events_duplicate
            + report.events_conflicting
            + report.frames_rejected;
        assert_eq!(
            processed, LORA_EVENT_RX_TICK_LIMIT,
            "at most LORA_EVENT_RX_TICK_LIMIT frames may be processed in one tick"
        );
    }

    // --- Stage 4B: LoRa SecureMeshEvent transmit path ------------------------
    //
    // `SECUREMESH_LORA_EVENT_TX` is process-global, and this test binary runs
    // tests in parallel by default. Every test below that needs a particular
    // value goes through `LoraEventTxGuard`, which holds a dedicated mutex for
    // its entire lifetime so no two of these tests can have the variable set
    // to conflicting values at the same time — the same hazard the existing
    // `SECUREMESH_LORA_SERIAL` tests accept implicitly, made explicit here
    // because "enabled" and "disabled" tests have opposite expectations and a
    // race between them would be a real flake, not just a redundant check.

    static LORA_EVENT_TX_ENV_LOCK: Mutex<()> = Mutex::new(());

    struct LoraEventTxGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl LoraEventTxGuard {
        fn enabled() -> Self {
            let lock = LORA_EVENT_TX_ENV_LOCK
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            // SAFETY: test-local; serialised against every other test that
            // touches this variable by `LORA_EVENT_TX_ENV_LOCK`.
            unsafe {
                std::env::set_var(SECUREMESH_LORA_EVENT_TX_ENV, "1");
            }
            Self { _lock: lock }
        }

        fn disabled() -> Self {
            let lock = LORA_EVENT_TX_ENV_LOCK
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            // SAFETY: see `enabled`.
            unsafe {
                std::env::remove_var(SECUREMESH_LORA_EVENT_TX_ENV);
            }
            Self { _lock: lock }
        }
    }

    impl Drop for LoraEventTxGuard {
        fn drop(&mut self) {
            // SAFETY: see `enabled`; still held under `_lock` at this point.
            unsafe {
                std::env::remove_var(SECUREMESH_LORA_EVENT_TX_ENV);
            }
        }
    }

    fn wait_until(mut predicate: impl FnMut() -> bool) {
        let mut waited = std::time::Duration::ZERO;
        while !predicate() && waited < std::time::Duration::from_secs(2) {
            std::thread::sleep(std::time::Duration::from_millis(10));
            waited += std::time::Duration::from_millis(10);
        }
    }

    // --- 1 & 7: disabled by default, no serial write at all -----------------

    #[test]
    fn lora_event_tx_is_disabled_by_default_and_writes_nothing() {
        let _guard = LoraEventTxGuard::disabled();
        let dir = TempDir::new().unwrap();
        let (node, mock) = node_with_lora(&dir);

        node.create_incident(input("no lora by default", "LOW"))
            .unwrap();

        // Nothing to wait *for* here — this asserts an absence, so a fixed
        // pause is the only honest way to give a (wrongly) spawned
        // transmission a chance to appear before declaring it did not.
        std::thread::sleep(std::time::Duration::from_millis(150));
        assert!(
            mock.written.lock().unwrap().is_empty(),
            "TX must stay off unless SECUREMESH_LORA_EVENT_TX is exactly \"1\""
        );
    }

    #[test]
    fn lora_event_tx_stays_off_for_any_value_other_than_exactly_one() {
        let _lock = LORA_EVENT_TX_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        let dir = TempDir::new().unwrap();
        let (node, mock) = node_with_lora(&dir);

        for value in ["", "true", "yes", "0", "01", " 1", "1 "] {
            // SAFETY: held under `_lock` for the duration of this test.
            unsafe {
                std::env::set_var(SECUREMESH_LORA_EVENT_TX_ENV, value);
            }
            node.create_incident(input(&format!("value={value:?}"), "LOW"))
                .unwrap();
        }
        // SAFETY: see above.
        unsafe {
            std::env::remove_var(SECUREMESH_LORA_EVENT_TX_ENV);
        }

        std::thread::sleep(std::time::Duration::from_millis(150));
        assert!(
            mock.written.lock().unwrap().is_empty(),
            "only the exact value \"1\" may enable LoRa event TX"
        );
    }

    // --- 2, 3, 4, 5 & 8: enabled produces exactly one correct, decodable ----
    // --- frame carrying the original signature ------------------------------

    #[test]
    fn lora_event_tx_enabled_produces_one_correct_securemeshevent_frame() {
        let _guard = LoraEventTxGuard::enabled();
        let dir = TempDir::new().unwrap();
        let (node, mock) = node_with_lora(&dir);

        let incident = node.create_incident(input("Bridge down", "HIGH")).unwrap();

        wait_until(|| !mock.written.lock().unwrap().is_empty());
        // Let the one expected frame finish landing before reading it back.
        std::thread::sleep(std::time::Duration::from_millis(50));

        let bytes = mock.written.lock().unwrap().clone();

        // Exactly one frame's worth of bytes: `LoraFrame::decode` requires
        // the buffer's length to match its own declared payload length
        // precisely, so if a second frame (or partial frame) had also been
        // written, this would fail rather than silently pass.
        let frame = LoraFrame::decode(&bytes).expect("exactly one valid frame must be written");

        assert_eq!(&bytes[0..4], b"SMLR", "the frame must carry the SMLR magic");
        assert_eq!(bytes[4], 1, "the frame must be wire version 1");
        assert_eq!(frame.message_type, LoraMessageType::SecureMeshEvent);
        assert_eq!(
            hex::encode(frame.source_node_id),
            node.public_identity().node_id,
            "the frame's source node ID must be this node's own ID"
        );
        assert!(!frame.payload.is_empty());
        // `LoraFrame::decode` already verified the CRC as part of succeeding;
        // re-encoding and comparing is an independent check that nothing
        // about the frame was lost or reordered.
        assert_eq!(frame.encode().unwrap(), bytes);

        // Decode the transmitted payload with the *real* codec and compare
        // every field against the event actually stored locally.
        let node_id = node.public_identity().node_id;
        let stored = node.database().events_since(&node_id, 0, 10).unwrap();
        assert_eq!(stored.len(), 1);
        let original = &stored[0];

        let decoded = lora_event_codec::decode(
            &frame.payload,
            &frame.source_node_id,
            &node.public_identity().public_key,
        )
        .unwrap();

        assert_eq!(decoded.event_id, original.event_id);
        assert_eq!(decoded.origin_node, original.origin_node);
        assert_eq!(decoded.origin_seq, original.origin_seq);
        assert_eq!(decoded.kind, original.kind);
        assert_eq!(decoded.payload, original.payload);
        assert_eq!(decoded.created_at, original.created_at);
        // No second signature was created: the transmitted event carries the
        // *original* signature, byte for byte.
        assert_eq!(decoded.signature, original.signature);
        assert!(decoded.verify().is_ok());

        let created = decoded.incident_created_payload().unwrap();
        assert_eq!(created.description, "Bridge down");
        assert_eq!(created.incident_id, incident.id);
    }

    // --- 6: an oversized event commits locally but is never transmitted ----

    #[test]
    fn an_oversized_event_commits_locally_but_is_not_transmitted() {
        let _guard = LoraEventTxGuard::enabled();
        let dir = TempDir::new().unwrap();
        let (node, mock) = node_with_lora(&dir);

        // Well inside the domain's 2000-character limit, but far past what
        // `lora_event_codec` can fit in one 192-byte frame (74 bytes of
        // description for a located-free INCIDENT_CREATED — see
        // `lora_event_codec`'s own `the_text_budget_is_exactly_what_the_frame_allows`).
        let long_description = "x".repeat(500);
        let incident = node
            .create_incident(input(&long_description, "LOW"))
            .unwrap();

        // The local commit is unaffected by whether LoRa can carry it.
        assert_eq!(incident.description, long_description);
        assert_eq!(node.database().count_events().unwrap(), 1);
        assert_eq!(node.list_incidents(None).unwrap().len(), 1);

        std::thread::sleep(std::time::Duration::from_millis(150));
        assert!(
            mock.written.lock().unwrap().is_empty(),
            "an oversized event must be refused, not truncated or fragmented onto the air"
        );
    }

    // --- 9: a QUIC-replicated event is never retransmitted over LoRa -------

    #[test]
    fn a_quic_replicated_event_does_not_transmit_over_lora() {
        let _guard = LoraEventTxGuard::enabled();

        let dir_a = TempDir::new().unwrap();
        let dir_b = TempDir::new().unwrap();
        let identity_a =
            NodeIdentity::load_or_create(&FileKeyStore::new(dir_a.path().join(KEYSTORE_FILE)))
                .unwrap();
        let identity_b =
            NodeIdentity::load_or_create(&FileKeyStore::new(dir_b.path().join(KEYSTORE_FILE)))
                .unwrap();

        let network = LoopbackNetwork::new();
        let quic_a = network.attach(identity_a.node_id(), &identity_a.public_key_hex());
        let quic_b = network.attach(identity_b.node_id(), &identity_b.public_key_hex());
        network.connect(identity_a.node_id(), identity_b.node_id());

        let mock_a = MockSerial::default();
        let lora_a = LoraTransport::open_with_io(mock_a.clone(), identity_a.node_id().to_string());
        let composite_a = CompositeTransport::new(quic_a, Some(lora_a));
        let node_a =
            NodeRuntime::initialize_with_transport(dir_a.path(), Box::new(composite_a)).unwrap();

        let composite_b = CompositeTransport::new(quic_b, None);
        let node_b =
            NodeRuntime::initialize_with_transport(dir_b.path(), Box::new(composite_b)).unwrap();

        // Mutual trust so replication is actually authorized both ways.
        node_a
            .database()
            .register_peer(identity_b.node_id(), &identity_b.public_key_hex(), None)
            .unwrap();
        node_a.approve_peer(identity_b.node_id(), None).unwrap();
        node_b
            .database()
            .register_peer(identity_a.node_id(), &identity_a.public_key_hex(), None)
            .unwrap();
        node_b.approve_peer(identity_a.node_id(), None).unwrap();

        // B authors an incident. B has no LoRa side, so this alone could not
        // produce a LoRa write; the point under test is what happens when it
        // reaches A, which does.
        node_b.create_incident(input("only on B", "LOW")).unwrap();

        for _ in 0..50 {
            node_a.sync_tick().unwrap();
            node_b.sync_tick().unwrap();
            if node_a.database().count_events().unwrap() > 0 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(
            node_a.database().count_events().unwrap(),
            1,
            "A must have replicated B's event over QUIC for this test to mean anything"
        );

        std::thread::sleep(std::time::Duration::from_millis(150));
        assert!(
            mock_a.written.lock().unwrap().is_empty(),
            "a QUIC-replicated event must never be retransmitted over LoRa"
        );
    }

    // --- 10: a LoRa-received event is never retransmitted over LoRa --------

    #[test]
    fn a_lora_received_event_does_not_retransmit_over_lora() {
        let _guard = LoraEventTxGuard::enabled();
        let dir = TempDir::new().unwrap();
        let (node, mock) = node_with_lora(&dir);

        let peer_dir = TempDir::new().unwrap();
        let peer = identity_in(&peer_dir);
        node.database()
            .register_peer(peer.node_id(), &peer.public_key_hex(), None)
            .unwrap();
        node.approve_peer(peer.node_id(), None).unwrap();

        mock.queue_inbound(&signed_event_frame_bytes(&peer, 1, "received over lora"));
        let report = drain_lora_events(&node);
        assert_eq!(report.events_applied, 1);

        // This is the bounce guard from first principles: A's LoRa -> B's DB
        // -> B's LoRa -> ... never gets a first hop, on any single node,
        // because applying a received event never calls the function that
        // gates transmission.
        std::thread::sleep(std::time::Duration::from_millis(150));
        assert!(
            mock.written.lock().unwrap().is_empty(),
            "a LoRa-received event must never be retransmitted over LoRa"
        );
    }

    // --- 11: a transmission failure never affects the committed event ------

    #[test]
    fn a_lora_transmission_failure_does_not_affect_the_committed_local_event() {
        let _guard = LoraEventTxGuard::enabled();
        let dir = TempDir::new().unwrap();
        let identity =
            NodeIdentity::load_or_create(&FileKeyStore::new(dir.path().join(KEYSTORE_FILE)))
                .unwrap();
        let network = LoopbackNetwork::new();
        let quic = network.attach(identity.node_id(), &identity.public_key_hex());

        // TX enabled, but with no LoRa side attached: `send_lora_event_payload`
        // fails cleanly with "transport not available", exercising the exact
        // `Err` branch `maybe_transmit_lora_event` takes for any transmission
        // failure — a dead I/O thread would fail the same way, through the
        // same branch.
        let composite = CompositeTransport::new(quic, None);
        let node = NodeRuntime::initialize_with_transport(dir.path(), Box::new(composite)).unwrap();

        let incident = node
            .create_incident(input("must still succeed", "LOW"))
            .unwrap();

        assert_eq!(incident.description, "must still succeed");
        assert_eq!(node.database().count_events().unwrap(), 1);
        assert_eq!(node.list_incidents(None).unwrap().len(), 1);
    }

    // --- The audit log records changes, not reads --------------------------

    #[test]
    fn reading_the_public_identity_is_silent() {
        // The dashboard polls this. Auditing it produced roughly 86,000 records
        // a day that said nothing happened, which is enough to bury a
        // revocation. Asserted by capture rather than by reading the source, so
        // it stays true if the call moves.
        let dir = TempDir::new().unwrap();
        let node = NodeRuntime::initialize(dir.path()).unwrap();

        let (_, records) = crate::security::audit::capture(|| {
            for _ in 0..100 {
                let _ = node.public_identity();
            }
        });

        assert!(
            records.is_empty(),
            "a pure read must not write to the audit log, got {records:?}"
        );
    }

    #[test]
    fn the_other_dashboard_reads_are_silent_too() {
        // One poll of everything the dashboard fetches on a timer. Whatever the
        // interval, an idle node must produce an idle audit log.
        let dir = TempDir::new().unwrap();
        let node = NodeRuntime::initialize(dir.path()).unwrap();

        let (_, records) = crate::security::audit::capture(|| {
            let _ = node.system_status();
            let _ = node.network_status();
            let _ = node.list_incidents(None);
            let _ = node.list_peers();
            let _ = node.local_role();
            let _ = node.intelligence_status();
        });

        assert!(
            records.is_empty(),
            "polling an idle node must be silent, got {records:?}"
        );
    }

    #[test]
    fn a_real_security_event_is_still_recorded() {
        // The other half of the claim: quietening reads must not have
        // quietened anything that matters.
        let dir = TempDir::new().unwrap();
        let node = NodeRuntime::initialize(dir.path()).unwrap();

        let (_, records) = crate::security::audit::capture(|| {
            node.create_incident(input("Bridge collapsed", "CRITICAL"))
                .unwrap();
        });

        assert!(
            records
                .iter()
                .any(|(event, _)| *event == crate::security::AuditEvent::IncidentCreated),
            "writing a record must still be audited, got {records:?}"
        );
    }

    #[test]
    fn identity_creation_and_loading_are_still_recorded() {
        // The events the audit log exists for. Creating a node writes both an
        // identity record and a database record.
        let dir = TempDir::new().unwrap();

        let (node, first_run) =
            crate::security::audit::capture(|| NodeRuntime::initialize(dir.path()).unwrap());
        drop(node);

        assert!(
            first_run
                .iter()
                .any(|(event, _)| *event == crate::security::AuditEvent::IdentityCreated),
            "generating a keypair must be audited, got {first_run:?}"
        );

        // Reopening the same directory loads the existing identity rather than
        // creating one, and says so.
        let (_node, second_run) =
            crate::security::audit::capture(|| NodeRuntime::initialize(dir.path()).unwrap());

        assert!(
            second_run
                .iter()
                .any(|(event, _)| *event == crate::security::AuditEvent::IdentityLoaded),
            "loading an existing keypair must be audited, got {second_run:?}"
        );
    }

    #[test]
    fn the_local_node_is_registered_so_it_can_author_incidents() {
        let dir = TempDir::new().unwrap();
        let node = NodeRuntime::initialize(dir.path()).unwrap();

        let identity = node.public_identity();
        let record = node.database().get_node(&identity.node_id).unwrap();
        assert_eq!(record.node_name, identity.node_name);
        assert_eq!(record.public_key, identity.public_key);
    }

    #[test]
    fn a_created_incident_is_attributed_to_this_node() {
        let dir = TempDir::new().unwrap();
        let node = NodeRuntime::initialize(dir.path()).unwrap();

        let incident = node
            .create_incident(input("Power line down", "HIGH"))
            .unwrap();
        assert_eq!(incident.created_by, node.public_identity().node_id);
        assert_eq!(incident.sync_status, SyncStatus::Pending);
    }

    #[test]
    fn invalid_input_is_rejected_before_it_reaches_storage() {
        let dir = TempDir::new().unwrap();
        let node = NodeRuntime::initialize(dir.path()).unwrap();

        assert!(node.create_incident(input("   ", "HIGH")).is_err());
        assert!(node.create_incident(input("valid", "URGENT")).is_err());
        assert_eq!(node.list_incidents(None).unwrap().len(), 0);
    }

    #[test]
    fn system_status_does_not_claim_capabilities_that_do_not_exist() {
        let dir = TempDir::new().unwrap();
        let node = NodeRuntime::initialize(dir.path()).unwrap();
        let status = node.system_status();

        assert_eq!(status.database.state, ComponentState::Operational);
        assert_eq!(status.identity.state, ComponentState::Operational);

        // These three must stay inactive until their phases actually land.
        assert_eq!(status.network.state, ComponentState::Inactive);
        assert_eq!(status.ai.state, ComponentState::Inactive);
        assert_eq!(status.tee.state, ComponentState::Inactive);
    }

    #[test]
    fn network_status_reports_an_isolated_node() {
        let dir = TempDir::new().unwrap();
        let node = NodeRuntime::initialize(dir.path()).unwrap();

        let network = node.network_status().unwrap();
        assert!(!network.online);
        assert_eq!(network.connected_peers, 0);
        assert_eq!(network.known_peers, 0);
        assert_eq!(network.transport, "none");
    }

    #[test]
    fn pending_sync_count_tracks_unsynchronised_incidents() {
        let dir = TempDir::new().unwrap();
        let node = NodeRuntime::initialize(dir.path()).unwrap();

        node.create_incident(input("one", "LOW")).unwrap();
        node.create_incident(input("two", "LOW")).unwrap();

        assert_eq!(node.network_status().unwrap().pending_sync, 2);
    }

    #[test]
    fn no_serialised_runtime_output_contains_the_private_key() {
        let dir = TempDir::new().unwrap();
        let node = NodeRuntime::initialize(dir.path()).unwrap();
        node.create_incident(input("check for leaks", "LOW"))
            .unwrap();

        let keyfile = std::fs::read_to_string(dir.path().join(KEYSTORE_FILE)).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&keyfile).unwrap();
        let secret = parsed["secret_key"].as_str().unwrap();

        // Everything the command layer can hand to the frontend.
        let payloads = vec![
            serde_json::to_string(&node.public_identity()).unwrap(),
            serde_json::to_string(&node.system_status()).unwrap(),
            serde_json::to_string(&node.network_status().unwrap()).unwrap(),
            serde_json::to_string(&node.list_incidents(None).unwrap()).unwrap(),
        ];

        for payload in payloads {
            assert!(
                !payload.contains(secret),
                "a command response leaked private key material"
            );
        }
    }

    // --- Restart behaviour -------------------------------------------------

    #[test]
    fn a_restarted_node_keeps_its_identity_and_its_incidents() {
        let dir = TempDir::new().unwrap();

        let (node_id, incident_id) = {
            let node = NodeRuntime::initialize(dir.path()).unwrap();
            let incident = node
                .create_incident(input("Shelter at full capacity", "CRITICAL"))
                .unwrap();
            (node.public_identity().node_id, incident.id)
        };

        // A second initialisation in the same directory is a restart.
        let restarted = NodeRuntime::initialize(dir.path()).unwrap();
        assert_eq!(restarted.public_identity().node_id, node_id);

        let incident = restarted.get_incident(&incident_id).unwrap();
        assert_eq!(incident.description, "Shelter at full capacity");
        assert_eq!(restarted.list_incidents(None).unwrap().len(), 1);
    }

    #[test]
    fn two_nodes_in_separate_directories_are_independent() {
        let dir_a = TempDir::new().unwrap();
        let dir_b = TempDir::new().unwrap();

        let node_a = NodeRuntime::initialize(dir_a.path()).unwrap();
        let node_b = NodeRuntime::initialize(dir_b.path()).unwrap();

        assert_ne!(
            node_a.public_identity().node_id,
            node_b.public_identity().node_id
        );

        node_a.create_incident(input("only on A", "LOW")).unwrap();

        // Without a sync engine, B must not see A's records.
        assert_eq!(node_a.list_incidents(None).unwrap().len(), 1);
        assert_eq!(node_b.list_incidents(None).unwrap().len(), 0);
    }

    // --- Event-backed incident creation ------------------------------------

    #[test]
    fn creating_an_incident_appends_a_verifiable_event() {
        let dir = TempDir::new().unwrap();
        let node = NodeRuntime::initialize(dir.path()).unwrap();

        let incident = node.create_incident(input("Bridge down", "HIGH")).unwrap();

        let events = node
            .database()
            .events_since(&node.public_identity().node_id, 0, 10)
            .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].origin_seq, 1);
        assert!(events[0].verify().is_ok());

        let payload = events[0].incident_created_payload().unwrap();
        assert_eq!(payload.incident_id, incident.id);
        assert_eq!(payload.description, "Bridge down");
    }

    #[test]
    fn local_sequence_numbers_are_contiguous_from_one() {
        let dir = TempDir::new().unwrap();
        let node = NodeRuntime::initialize(dir.path()).unwrap();

        for n in 0..5 {
            node.create_incident(input(&format!("incident {n}"), "LOW"))
                .unwrap();
        }

        let node_id = node.public_identity().node_id;
        let events = node.database().events_since(&node_id, 0, 100).unwrap();
        let sequences: Vec<u64> = events.iter().map(|e| e.origin_seq).collect();
        assert_eq!(sequences, vec![1, 2, 3, 4, 5]);
        assert_eq!(node.database().watermark_for(&node_id).unwrap(), 5);
    }

    #[test]
    fn sequence_numbers_continue_across_a_restart() {
        let dir = TempDir::new().unwrap();

        {
            let node = NodeRuntime::initialize(dir.path()).unwrap();
            node.create_incident(input("before restart", "LOW"))
                .unwrap();
        }

        let node = NodeRuntime::initialize(dir.path()).unwrap();
        node.create_incident(input("after restart", "LOW")).unwrap();

        let node_id = node.public_identity().node_id;
        let events = node.database().events_since(&node_id, 0, 100).unwrap();
        assert_eq!(events.len(), 2, "the log must not restart at 1");
        assert_eq!(events[1].origin_seq, 2);
    }

    #[test]
    fn a_locally_created_incident_starts_unsynchronised() {
        let dir = TempDir::new().unwrap();
        let node = NodeRuntime::initialize(dir.path()).unwrap();

        let incident = node
            .create_incident(input("awaiting a peer", "LOW"))
            .unwrap();
        assert_eq!(incident.sync_status, SyncStatus::Pending);
    }

    #[test]
    fn concurrent_local_creation_does_not_self_equivocate() {
        use std::sync::Arc;

        let dir = TempDir::new().unwrap();
        let node = Arc::new(NodeRuntime::initialize(dir.path()).unwrap());

        // Two threads racing to append is exactly what the local-append guard
        // exists to prevent turning into a duplicate sequence number.
        let handles: Vec<_> = (0..8)
            .map(|n| {
                let node = Arc::clone(&node);
                std::thread::spawn(move || {
                    node.create_incident(input(&format!("racer {n}"), "LOW"))
                })
            })
            .collect();

        for handle in handles {
            handle
                .join()
                .unwrap()
                .expect("concurrent create should succeed");
        }

        let node_id = node.public_identity().node_id;
        assert_eq!(node.database().count_events().unwrap(), 8);
        assert_eq!(node.database().count_event_conflicts().unwrap(), 0);
        assert_eq!(node.database().watermark_for(&node_id).unwrap(), 8);
    }

    // --- Observations ------------------------------------------------------

    #[test]
    fn an_observation_appends_rather_than_mutating() {
        let dir = TempDir::new().unwrap();
        let node = NodeRuntime::initialize(dir.path()).unwrap();

        let incident = node
            .create_incident(input("Initial report", "MEDIUM"))
            .unwrap();
        node.add_observation(&incident.id, "Water level rising")
            .unwrap();

        // The incident itself is untouched.
        let reloaded = node.get_incident(&incident.id).unwrap();
        assert_eq!(reloaded.description, "Initial report");

        let node_id = node.public_identity().node_id;
        let events = node.database().events_since(&node_id, 0, 10).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[1].kind, EventKind::IncidentObservation);
        assert_eq!(
            events[1].incident_observation_payload().unwrap().note,
            "Water level rising"
        );
    }

    #[test]
    fn observations_are_validated() {
        let dir = TempDir::new().unwrap();
        let node = NodeRuntime::initialize(dir.path()).unwrap();
        let incident = node.create_incident(input("report", "LOW")).unwrap();

        assert!(node.add_observation(&incident.id, "   ").is_err());
        assert!(node.add_observation("no-such-incident", "note").is_err());
        assert_eq!(node.database().count_events().unwrap(), 1);
    }

    // --- Upgrade from Phase 1 ----------------------------------------------

    #[test]
    fn phase_one_incidents_are_backfilled_into_the_event_log() {
        let dir = TempDir::new().unwrap();

        // Build a node, then strip the event log to model a Phase 1 database
        // that has just been opened by a Phase 2 build.
        let legacy_ids = {
            let node = NodeRuntime::initialize(dir.path()).unwrap();
            let a = node.create_incident(input("legacy one", "HIGH")).unwrap();
            let b = node.create_incident(input("legacy two", "LOW")).unwrap();

            let conn = node.database().conn();
            conn.execute("UPDATE incidents SET origin_event_id = NULL", [])
                .unwrap();
            conn.execute("DELETE FROM events", []).unwrap();
            vec![a.id, b.id]
        };

        let upgraded = NodeRuntime::initialize(dir.path()).unwrap();
        let node_id = upgraded.public_identity().node_id;

        // Every legacy incident now has a signed, replicable event.
        assert_eq!(upgraded.database().count_events().unwrap(), 2);
        assert_eq!(upgraded.database().watermark_for(&node_id).unwrap(), 2);

        for event in upgraded.database().events_since(&node_id, 0, 10).unwrap() {
            assert!(event.verify().is_ok());
            let payload = event.incident_created_payload().unwrap();
            assert!(legacy_ids.contains(&payload.incident_id));
        }

        // The incidents themselves are untouched and not duplicated.
        assert_eq!(upgraded.list_incidents(None).unwrap().len(), 2);
    }

    #[test]
    fn backfill_does_not_run_twice() {
        let dir = TempDir::new().unwrap();

        {
            let node = NodeRuntime::initialize(dir.path()).unwrap();
            node.create_incident(input("once", "LOW")).unwrap();
        }

        // Several restarts must not mint additional events for the same record.
        for _ in 0..3 {
            let node = NodeRuntime::initialize(dir.path()).unwrap();
            assert_eq!(node.database().count_events().unwrap(), 1);
        }
    }

    // --- Phase 6 step 3: the LoRa sync responder, wired through the tick ----
    //
    // These drive genuine `SyncRequest` bytes through the real
    // `LoraTransport` I/O thread via `MockSerial`, exactly as the Stage 4A
    // event-receive tests above do, so `lora_receive_tick` runs through
    // every production layer it would in the field with only the physical
    // serial device mocked out.

    use crate::networking::lora_sync;

    fn sync_request_wire_bytes(
        requester: &NodeIdentity,
        target: &NodeIdentity,
        watermark: u64,
        bitmap: u64,
        max_events: u8,
        request_ts: i64,
    ) -> Vec<u8> {
        let mut target_raw = [0u8; 32];
        target_raw.copy_from_slice(&hex::decode(target.node_id()).unwrap());
        let request = lora_sync::SyncRequest {
            target_origin: target_raw,
            watermark,
            have_bitmap: bitmap,
            max_events,
            request_ts,
        };
        let payload = lora_sync::encode(requester, &request).unwrap();

        let mut source = [0u8; 32];
        source.copy_from_slice(&hex::decode(requester.node_id()).unwrap());
        LoraFrame {
            message_type: LoraMessageType::SyncRequest,
            source_node_id: source,
            sequence: 0,
            payload,
        }
        .encode()
        .unwrap()
    }

    fn wait_for_written(mock: &MockSerial, minimum_len: usize) -> Vec<u8> {
        let mut waited = std::time::Duration::ZERO;
        loop {
            let bytes = mock.written.lock().unwrap().clone();
            if bytes.len() >= minimum_len || waited >= std::time::Duration::from_secs(2) {
                return bytes;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
            waited += std::time::Duration::from_millis(10);
        }
    }

    #[test]
    fn a_valid_sync_request_is_answered_end_to_end_through_the_tick() {
        // Held for the whole test: the incident is seeded with LoRa event TX
        // off, so the only frame that can land on the wire is the
        // responder's answer, not `create_incident`'s own live transmission.
        let _lock = LORA_EVENT_TX_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let dir = TempDir::new().unwrap();
        let (node, mock) = node_with_lora(&dir);

        let requester_dir = TempDir::new().unwrap();
        let requester = identity_in(&requester_dir);
        node.database()
            .register_peer(requester.node_id(), &requester.public_key_hex(), None)
            .unwrap();
        node.approve_peer(requester.node_id(), None).unwrap();

        node.create_incident(input("first", "LOW")).unwrap();
        assert!(mock.written.lock().unwrap().is_empty());

        // SAFETY: serialised by `_lock`, held for the rest of this test.
        unsafe {
            std::env::set_var(SECUREMESH_LORA_EVENT_TX_ENV, "1");
        }

        let request_bytes = sync_request_wire_bytes(&requester, &node.identity, 0, 0, 8, 1_000);
        mock.queue_inbound(&request_bytes);

        // Several ticks: one to receive and process the request, more to
        // drain the (single-event) outbox at one frame per tick.
        let mut waited = std::time::Duration::ZERO;
        loop {
            node.lora_receive_tick().unwrap();
            if !mock.written.lock().unwrap().is_empty()
                || waited >= std::time::Duration::from_secs(2)
            {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
            waited += std::time::Duration::from_millis(10);
        }

        let written = mock.written.lock().unwrap().clone();
        let frame = LoraFrame::decode(&written).expect("a type-2 event frame should be written");
        assert_eq!(frame.message_type, LoraMessageType::SecureMeshEvent);

        // SAFETY: still under `_lock`.
        unsafe {
            std::env::remove_var(SECUREMESH_LORA_EVENT_TX_ENV);
        }
    }

    #[test]
    fn a_sync_request_is_ignored_when_lora_event_tx_is_disabled() {
        let _guard = LoraEventTxGuard::disabled();
        let dir = TempDir::new().unwrap();
        let (node, mock) = node_with_lora(&dir);

        let requester_dir = TempDir::new().unwrap();
        let requester = identity_in(&requester_dir);
        node.database()
            .register_peer(requester.node_id(), &requester.public_key_hex(), None)
            .unwrap();
        node.approve_peer(requester.node_id(), None).unwrap();
        node.create_incident(input("first", "LOW")).unwrap();

        let request_bytes = sync_request_wire_bytes(&requester, &node.identity, 0, 0, 8, 1_000);
        mock.queue_inbound(&request_bytes);

        for _ in 0..10 {
            node.lora_receive_tick().unwrap();
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(
            mock.written.lock().unwrap().is_empty(),
            "receive-only nodes (LoRa event TX disabled) must never answer a sync request"
        );
    }

    #[test]
    fn an_unauthorized_sync_requester_is_answered_with_nothing() {
        let _lock = LORA_EVENT_TX_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let dir = TempDir::new().unwrap();
        let (node, mock) = node_with_lora(&dir);

        let stranger_dir = TempDir::new().unwrap();
        let stranger = identity_in(&stranger_dir);
        // Never registered or approved.
        node.create_incident(input("first", "LOW")).unwrap();
        assert!(mock.written.lock().unwrap().is_empty());

        // SAFETY: serialised by `_lock`.
        unsafe {
            std::env::set_var(SECUREMESH_LORA_EVENT_TX_ENV, "1");
        }

        let request_bytes = sync_request_wire_bytes(&stranger, &node.identity, 0, 0, 8, 1_000);
        mock.queue_inbound(&request_bytes);

        for _ in 0..10 {
            node.lora_receive_tick().unwrap();
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(mock.written.lock().unwrap().is_empty());

        // SAFETY: still under `_lock`.
        unsafe {
            std::env::remove_var(SECUREMESH_LORA_EVENT_TX_ENV);
        }
    }

    #[test]
    fn the_responder_serves_at_most_one_request_per_tick() {
        let _lock = LORA_EVENT_TX_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let dir = TempDir::new().unwrap();
        let (node, mock) = node_with_lora(&dir);

        let requester_dir = TempDir::new().unwrap();
        let requester = identity_in(&requester_dir);
        node.database()
            .register_peer(requester.node_id(), &requester.public_key_hex(), None)
            .unwrap();
        node.approve_peer(requester.node_id(), None).unwrap();
        node.create_incident(input("first", "LOW")).unwrap();

        // SAFETY: serialised by `_lock`, held for the rest of this test.
        unsafe {
            std::env::set_var(SECUREMESH_LORA_EVENT_TX_ENV, "1");
        }

        // Two requests queued at once, distinguished by `request_ts` so a
        // replay/rate-limit rejection cannot be mistaken for "not yet
        // processed" — both would otherwise look identical from outside.
        let mut wire = sync_request_wire_bytes(&requester, &node.identity, 0, 0, 1, 1_000);
        wire.extend(sync_request_wire_bytes(
            &requester,
            &node.identity,
            0,
            0,
            1,
            2_000,
        ));
        mock.queue_inbound(&wire);

        // Give the I/O thread time to decode both frames into the queue
        // before the single tick below drains at most one of them.
        std::thread::sleep(std::time::Duration::from_millis(200));
        node.lora_receive_tick().unwrap();

        // One request accepted (whichever the transport queue offers first)
        // advances the peer's timestamp state to exactly one of the two
        // values, and the other stays queued for a later tick — proven by
        // the queue still holding a request after this single call.
        let engine = node
            .mesh
            .as_ref()
            .unwrap()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let remaining = engine.transport().poll_lora_sync_requests(usize::MAX);
        drop(engine);
        assert_eq!(
            remaining.len(),
            1,
            "exactly one of the two queued requests must remain unprocessed after one tick"
        );

        // SAFETY: still under `_lock`.
        unsafe {
            std::env::remove_var(SECUREMESH_LORA_EVENT_TX_ENV);
        }
    }

    #[test]
    fn multiple_historical_events_are_paced_one_frame_per_tick() {
        // Held for the whole test: the 3 incidents are seeded with LoRa
        // event TX off, so none of them transmits live — every frame that
        // reaches the wire below comes from the responder answering the one
        // queued sync request.
        let _lock = LORA_EVENT_TX_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let dir = TempDir::new().unwrap();
        let (node, mock) = node_with_lora(&dir);

        let requester_dir = TempDir::new().unwrap();
        let requester = identity_in(&requester_dir);
        node.database()
            .register_peer(requester.node_id(), &requester.public_key_hex(), None)
            .unwrap();
        node.approve_peer(requester.node_id(), None).unwrap();

        for n in 0..3 {
            node.create_incident(input(&format!("event {n}"), "LOW"))
                .unwrap();
        }
        assert!(mock.written.lock().unwrap().is_empty());

        // SAFETY: serialised by `_lock`, held for the rest of this test.
        unsafe {
            std::env::set_var(SECUREMESH_LORA_EVENT_TX_ENV, "1");
        }

        let request_bytes = sync_request_wire_bytes(&requester, &node.identity, 0, 0, 8, 1_000);
        mock.queue_inbound(&request_bytes);
        std::thread::sleep(std::time::Duration::from_millis(200));

        // Tick 1: the request is accepted and all 3 payloads are queued;
        // at most one is transmitted this same tick.
        node.lora_receive_tick().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(50));
        let after_first_tick = mock.written.lock().unwrap().clone();
        let decoded = LoraFrame::decode(&after_first_tick);
        assert!(
            matches!(
                decoded.as_ref().map(|f| f.message_type),
                Ok(LoraMessageType::SecureMeshEvent)
            ),
            "exactly one frame should be on the wire after the first tick, got {decoded:?}"
        );

        // Ticks 2 and 3 each drain one more queued payload.
        node.lora_receive_tick().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(50));
        let after_second_tick = mock.written.lock().unwrap().len();
        assert!(after_second_tick > after_first_tick.len());

        node.lora_receive_tick().unwrap();
        let written = wait_for_written(&mock, after_second_tick + 1);
        assert!(written.len() > after_second_tick);

        // SAFETY: still under `_lock`.
        unsafe {
            std::env::remove_var(SECUREMESH_LORA_EVENT_TX_ENV);
        }
    }

    #[test]
    fn a_third_partys_event_is_never_relayed_through_the_responder() {
        let _guard = LoraEventTxGuard::enabled();
        let dir = TempDir::new().unwrap();
        let (node, mock) = node_with_lora(&dir);

        let requester_dir = TempDir::new().unwrap();
        let requester = identity_in(&requester_dir);
        node.database()
            .register_peer(requester.node_id(), &requester.public_key_hex(), None)
            .unwrap();
        node.approve_peer(requester.node_id(), None).unwrap();

        // A third node's event, already replicated into this node's log by
        // some other path (as a prior LoRa event frame would leave it).
        let third_dir = TempDir::new().unwrap();
        let third = identity_in(&third_dir);
        node.database()
            .register_peer(third.node_id(), &third.public_key_hex(), None)
            .unwrap();
        let their_event = MeshEvent::create(
            &third,
            1,
            EventKind::IncidentCreated,
            IncidentCreatedPayload {
                incident_id: Uuid::new_v4().to_string(),
                description: "not this node's".to_string(),
                severity: "HIGH".to_string(),
                latitude: None,
                longitude: None,
                accuracy_meters: None,
                location_source: crate::domain::LocationSource::Unknown,
                location_captured_at: None,
            },
        )
        .unwrap();
        node.database()
            .apply_event(&their_event, node.identity.node_id(), Some(third.node_id()))
            .unwrap();
        // This node has no event of its own at all.

        let request_bytes = sync_request_wire_bytes(&requester, &node.identity, 0, 0, 8, 1_000);
        mock.queue_inbound(&request_bytes);

        for _ in 0..10 {
            node.lora_receive_tick().unwrap();
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(
            mock.written.lock().unwrap().is_empty(),
            "a third party's replicated event must never be answered back out over LoRa"
        );
    }

    #[test]
    fn live_type_2_event_reception_is_unaffected_by_the_responder() {
        // Regression: the responder hook inside `lora_receive_tick` must not
        // change Phase 5A's own behaviour when no sync request is involved
        // at all — this is exactly the pre-existing accepted-event test,
        // run again after the responder was wired in.
        let _guard = LoraEventTxGuard::disabled();
        let dir = TempDir::new().unwrap();
        let (node, mock) = node_with_lora(&dir);

        let peer_dir = TempDir::new().unwrap();
        let peer = identity_in(&peer_dir);
        node.database()
            .register_peer(peer.node_id(), &peer.public_key_hex(), None)
            .unwrap();
        node.approve_peer(peer.node_id(), None).unwrap();

        mock.queue_inbound(&signed_event_frame_bytes(&peer, 1, "still works"));
        assert_eq!(drain_lora_events(&node).events_applied, 1);
    }

    // --- Phase 6 step 4: the LoRa sync REQUESTER, wired through the tick ----
    //
    // These drive genuine bytes through the real `LoraTransport` I/O thread
    // via `MockSerial`, exactly as the responder's own integration tests do.

    #[test]
    fn a_gap_revealing_event_transmits_exactly_one_sync_request_when_tx_is_enabled() {
        let _lock = LORA_EVENT_TX_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let dir = TempDir::new().unwrap();
        let (node, mock) = node_with_lora(&dir);

        let peer_dir = TempDir::new().unwrap();
        let peer = identity_in(&peer_dir);
        node.database()
            .register_peer(peer.node_id(), &peer.public_key_hex(), None)
            .unwrap();
        node.approve_peer(peer.node_id(), None).unwrap();

        // SAFETY: serialised by `_lock`, held for the rest of this test.
        unsafe {
            std::env::set_var(SECUREMESH_LORA_EVENT_TX_ENV, "1");
        }

        // watermark starts at 0; seq 1..=3 first, then a jump to 6.
        for seq in 1..=3 {
            mock.queue_inbound(&signed_event_frame_bytes(&peer, seq, "in order"));
        }
        tick_until(&node, || {
            node.database().watermark_for(peer.node_id()).unwrap() == 3
        });
        assert!(
            mock.written.lock().unwrap().is_empty(),
            "no gap yet: nothing should be transmitted"
        );

        mock.queue_inbound(&signed_event_frame_bytes(&peer, 6, "reveals a gap"));
        tick_until(&node, || !mock.written.lock().unwrap().is_empty());

        let written = mock.written.lock().unwrap().clone();
        let frame = LoraFrame::decode(&written).expect("a sync request frame should be written");
        assert_eq!(frame.message_type, LoraMessageType::SyncRequest);

        let mut local_raw = [0u8; 32];
        local_raw.copy_from_slice(&hex::decode(node.identity.node_id()).unwrap());
        assert_eq!(
            frame.source_node_id, local_raw,
            "requester ID is the local node ID"
        );

        let request =
            lora_sync::decode_verified(&frame.payload, &local_raw, &node.identity.public_key_hex())
                .expect("the generated request must verify with the existing verification");
        let mut expected_target = [0u8; 32];
        expected_target.copy_from_slice(&hex::decode(peer.node_id()).unwrap());
        assert_eq!(request.target_origin, expected_target);
        assert_eq!(request.watermark, 3);

        // SAFETY: still under `_lock`.
        unsafe {
            std::env::remove_var(SECUREMESH_LORA_EVENT_TX_ENV);
        }
    }

    #[test]
    fn a_gap_revealing_event_transmits_nothing_when_tx_is_disabled() {
        let _guard = LoraEventTxGuard::disabled();
        let dir = TempDir::new().unwrap();
        let (node, mock) = node_with_lora(&dir);

        let peer_dir = TempDir::new().unwrap();
        let peer = identity_in(&peer_dir);
        node.database()
            .register_peer(peer.node_id(), &peer.public_key_hex(), None)
            .unwrap();
        node.approve_peer(peer.node_id(), None).unwrap();

        for seq in 1..=3 {
            mock.queue_inbound(&signed_event_frame_bytes(&peer, seq, "in order"));
        }
        tick_until(&node, || {
            node.database().watermark_for(peer.node_id()).unwrap() == 3
        });

        mock.queue_inbound(&signed_event_frame_bytes(&peer, 6, "reveals a gap"));
        // The gap is genuinely detected (seq 6 is stored, so a request
        // *would* be built) — only transmission is suppressed.
        tick_until(&node, || {
            node.database()
                .held_sequences(peer.node_id(), 6, 6)
                .unwrap()
                == vec![6]
        });

        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(
            mock.written.lock().unwrap().is_empty(),
            "a receive-only node must detect the gap without ever transmitting a request for it"
        );
    }

    #[test]
    fn an_untrusted_origins_gap_does_not_trigger_a_sync_request() {
        let _lock = LORA_EVENT_TX_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let dir = TempDir::new().unwrap();
        let (node, mock) = node_with_lora(&dir);

        // SAFETY: serialised by `_lock`.
        unsafe {
            std::env::set_var(SECUREMESH_LORA_EVENT_TX_ENV, "1");
        }

        // A stranger's frame is rejected by ingest itself (unknown sender),
        // so `IngestedEvent::Stored` — the only path that can trigger a
        // request — is never reached at all.
        let stranger_dir = TempDir::new().unwrap();
        let stranger = identity_in(&stranger_dir);
        mock.queue_inbound(&signed_event_frame_bytes(&stranger, 6, "untrusted"));

        for _ in 0..10 {
            node.lora_receive_tick().unwrap();
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(mock.written.lock().unwrap().is_empty());

        // SAFETY: still under `_lock`.
        unsafe {
            std::env::remove_var(SECUREMESH_LORA_EVENT_TX_ENV);
        }
    }

    /// Ticks `node`'s LoRa receive loop until `predicate` holds (or a
    /// two-second timeout), giving the transport's own background I/O
    /// thread — which decodes `MockSerial` bytes asynchronously — repeated
    /// chances to catch up between polls. A single tick immediately after
    /// queuing bytes can otherwise run before anything has been decoded yet.
    fn tick_until(node: &NodeRuntime, mut predicate: impl FnMut() -> bool) {
        let mut waited = std::time::Duration::ZERO;
        loop {
            node.lora_receive_tick().unwrap();
            if predicate() || waited >= std::time::Duration::from_secs(2) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
            waited += std::time::Duration::from_millis(10);
        }
    }

    /// The wire length of one frame starting at `bytes[0]`, computed from its
    /// own header exactly as `lora_transport::drain_frames` does — magic(4) +
    /// version(1) + type(1) + source(32) + sequence(4) + payload_len(1) is
    /// 43 bytes, followed by that many payload bytes and a 4-byte CRC.
    /// [`LoraFrame::decode`] takes exactly one frame's bytes, never a
    /// multi-frame stream, so callers walking concatenated frames (as
    /// `MockSerial`'s written buffer accumulates them) must slice by this
    /// length first.
    const FRAME_HEADER_LEN: usize = 43;
    fn frame_len(bytes: &[u8]) -> usize {
        let payload_len = bytes[FRAME_HEADER_LEN - 1] as usize;
        FRAME_HEADER_LEN + payload_len + 4
    }

    /// Splits `buf` into individual, decoded LoRa frames, in order. Used to
    /// walk `MockSerial`'s accumulated written bytes, which may hold several
    /// concatenated frames.
    fn split_frames(buf: &[u8]) -> Vec<LoraFrame> {
        let mut offset = 0;
        let mut frames = Vec::new();
        while offset + FRAME_HEADER_LEN <= buf.len() {
            let len = frame_len(&buf[offset..]);
            if offset + len > buf.len() {
                break; // an in-flight, not-yet-complete frame
            }
            match LoraFrame::decode(&buf[offset..offset + len]) {
                Ok(frame) => frames.push(frame),
                Err(_) => break,
            }
            offset += len;
        }
        frames
    }

    /// How many complete, decodable LoRa frames are present in `buf`.
    /// Used to wait for a specific number of frames to have been
    /// transmitted without depending on their exact byte length.
    fn count_frames(buf: &[u8]) -> usize {
        split_frames(buf).len()
    }

    /// A relay function standing in for "the air": copies bytes newly
    /// written to `from` since `from_offset`, feeding them into `to`'s
    /// inbound queue exactly as an E22 pair would carry them, and returns
    /// the new length of `from`'s written buffer so the caller can track
    /// the next offset. No sleep here — the caller decides how long to wait
    /// for the destination's own I/O thread to decode what was just queued.
    fn relay(from: &MockSerial, from_offset: usize, to: &MockSerial) -> usize {
        let bytes = from.written.lock().unwrap().clone();
        to.queue_inbound(&bytes[from_offset..]);
        bytes.len()
    }

    #[test]
    fn mock_end_to_end_a_gap_is_detected_requested_and_resolved_without_an_echo() {
        // Node A owns seq 1..=6. Node B starts with 1, 2, 3, 6 (as if it had
        // received A's seq 6 live before this test begins) and must recover
        // 4 and 5 from A over LoRa, entirely through the production
        // request/respond/ingest path — no QUIC connection is involved.
        let _lock = LORA_EVENT_TX_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        let dir_a = TempDir::new().unwrap();
        let dir_b = TempDir::new().unwrap();
        let (node_a, mock_a) = node_with_lora(&dir_a);
        let (node_b, mock_b) = node_with_lora(&dir_b);

        // Mutual trust: B must accept A's events, and A must accept B's
        // sync request.
        node_a
            .database()
            .register_peer(
                node_b.identity.node_id(),
                &node_b.identity.public_key_hex(),
                None,
            )
            .unwrap();
        node_a
            .approve_peer(node_b.identity.node_id(), None)
            .unwrap();
        node_b
            .database()
            .register_peer(
                node_a.identity.node_id(),
                &node_a.identity.public_key_hex(),
                None,
            )
            .unwrap();
        node_b
            .approve_peer(node_a.identity.node_id(), None)
            .unwrap();

        // A authors seq 1..=6 with TX disabled, so seeding never transmits.
        for n in 0..6 {
            node_a
                .create_incident(input(&format!("A event {n}"), "LOW"))
                .unwrap();
        }
        let a_events = node_a
            .database()
            .events_since(node_a.identity.node_id(), 0, 10)
            .unwrap();
        assert_eq!(a_events.len(), 6);

        // B already holds A's seq 1, 2, 3 (out-of-band prior sync), applied
        // directly to storage exactly as the runtime's own ingest path would
        // have left them.
        for event in &a_events[0..3] {
            node_b
                .database()
                .apply_event(
                    event,
                    node_b.identity.node_id(),
                    Some(node_a.identity.node_id()),
                )
                .unwrap();
        }
        assert_eq!(
            node_b
                .database()
                .watermark_for(node_a.identity.node_id())
                .unwrap(),
            3
        );
        // TX was off throughout seeding, so nothing was transmitted yet.
        assert!(mock_a.written.lock().unwrap().is_empty());
        assert!(mock_b.written.lock().unwrap().is_empty());

        // SAFETY: serialised by `_lock`, held for the rest of this test.
        unsafe {
            std::env::set_var(SECUREMESH_LORA_EVENT_TX_ENV, "1");
        }

        // Step 1: B receives A's seq 6 live over LoRa (the frame A would
        // genuinely have transmitted for it).
        let seq6 = a_events[5].clone();
        let seq6_frame_bytes = {
            let mut source = [0u8; 32];
            source.copy_from_slice(&hex::decode(node_a.identity.node_id()).unwrap());
            LoraFrame {
                message_type: LoraMessageType::SecureMeshEvent,
                source_node_id: source,
                sequence: 0,
                payload: lora_event_codec::encode(&seq6).unwrap(),
            }
            .encode()
            .unwrap()
        };
        mock_b.queue_inbound(&seq6_frame_bytes);

        tick_until(&node_b, || {
            node_b.database().has_event(&seq6.event_id).unwrap()
        });

        // B: seq 6 applied, watermark still 3 (4 and 5 are missing).
        assert_eq!(
            node_b
                .database()
                .watermark_for(node_a.identity.node_id())
                .unwrap(),
            3
        );
        assert!(node_b.database().has_event(&seq6.event_id).unwrap());

        // B must have produced exactly one sync request.
        tick_until(&node_b, || !mock_b.written.lock().unwrap().is_empty());
        let request_bytes = mock_b.written.lock().unwrap().clone();
        let request_frame =
            LoraFrame::decode(&request_bytes).expect("B must transmit a sync request");
        assert_eq!(request_frame.message_type, LoraMessageType::SyncRequest);
        let mut b_raw = [0u8; 32];
        b_raw.copy_from_slice(&hex::decode(node_b.identity.node_id()).unwrap());
        let decoded_request = lora_sync::decode_verified(
            &request_frame.payload,
            &b_raw,
            &node_b.identity.public_key_hex(),
        )
        .unwrap();
        let mut a_raw = [0u8; 32];
        a_raw.copy_from_slice(&hex::decode(node_a.identity.node_id()).unwrap());
        assert_eq!(decoded_request.target_origin, a_raw);
        assert_eq!(decoded_request.watermark, 3);
        // Bit 1 (watermark + 3 = 6) must be set: B already holds seq 6.
        assert_eq!(
            decoded_request.have_bitmap & 0b10,
            0b10,
            "the bitmap must indicate seq 6 is already held"
        );

        // Step 2: relay B's request to A.
        let mut b_offset = relay(&mock_b, 0, &mock_a);

        // A serves the request: one responder tick to accept it, further
        // ticks to drain the paced outbox (one frame per tick).
        tick_until(&node_a, || {
            count_frames(&mock_a.written.lock().unwrap()) >= 2
        });

        let a_written = mock_a.written.lock().unwrap().clone();
        let a_frames = split_frames(&a_written);
        assert_eq!(
            a_frames.len(),
            2,
            "A must have transmitted exactly 2 frames"
        );
        let mut a_sent_sequences = Vec::new();
        for frame in &a_frames {
            assert_eq!(frame.message_type, LoraMessageType::SecureMeshEvent);
            let decoded = lora_event_codec::decode(
                &frame.payload,
                &frame.source_node_id,
                &node_a.identity.public_key_hex(),
            )
            .unwrap();
            a_sent_sequences.push(decoded.origin_seq);
        }
        // A must select exactly seq 4 and seq 5 — never seq 6, which the
        // bitmap already marked as held by B.
        assert_eq!(
            a_sent_sequences,
            vec![4, 5],
            "A must not redundantly resend seq 6"
        );

        // Step 3: relay A's answer to B.
        relay(&mock_a, 0, &mock_b);

        tick_until(&node_b, || {
            node_b
                .database()
                .watermark_for(node_a.identity.node_id())
                .unwrap()
                == 6
        });

        // B now holds the complete run.
        assert_eq!(
            node_b
                .database()
                .watermark_for(node_a.identity.node_id())
                .unwrap(),
            6
        );
        for event in &a_events {
            assert!(node_b.database().has_event(&event.event_id).unwrap());
        }

        // No echo: B never retransmits A's events (its own written buffer
        // gained nothing beyond the one sync request from step 1), and no
        // second sync request was generated once the gap resolved (a later
        // contiguous event — already true here, since 4 and 5 arrived and
        // closed it — produces nothing further).
        b_offset = relay(&mock_b, b_offset, &mock_a); // drains, proves nothing new
        let _ = b_offset;
        assert_eq!(
            mock_b.written.lock().unwrap().len(),
            request_bytes.len(),
            "B must not have transmitted anything beyond its one original sync request"
        );

        // SAFETY: still under `_lock`.
        unsafe {
            std::env::remove_var(SECUREMESH_LORA_EVENT_TX_ENV);
        }
    }
}
