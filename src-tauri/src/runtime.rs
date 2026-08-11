//! The assembled SecureMesh node.
//!
//! [`NodeRuntime`] owns the node's identity and its local database and exposes
//! the operations the UI needs. All decision-making lives here rather than in
//! React: the frontend renders what the runtime reports and never derives
//! status of its own.
//!
//! The runtime is deliberately free of Tauri types so it can be constructed
//! and driven directly from integration tests.

use crate::domain::event::{EventKind, IncidentCreatedPayload, IncidentObservationPayload};
use crate::domain::trust::{Capability, PeerRole, TrustEvent, TrustState};
use crate::domain::{Incident, MeshEvent, NewIncident, Observation, SyncStatus};
use crate::error::{CoreError, CoreResult};
use crate::identity::keystore::FileKeyStore;
use crate::identity::{NodeIdentity, PublicIdentity};
use crate::networking::{MeshTransport, PeerDescriptor};
use crate::security::{audit, AuditEvent, AuditOutcome};
use crate::storage::Database;
use crate::sync::{SyncEngine, SyncReport};
use serde::Serialize;
use std::path::Path;
use std::sync::Mutex;
use uuid::Uuid;

/// File names created inside the node's data directory.
///
/// Public so that a harness can construct the node's identity before the
/// runtime exists — attaching a transport requires knowing the node ID, which
/// is derived from the key.
pub const DATABASE_FILE: &str = "securemesh.sqlite";
pub const KEYSTORE_FILE: &str = "node_identity.json";

/// A running SecureMesh node.
pub struct NodeRuntime {
    identity: NodeIdentity,
    database: Database,
    /// Serialises local event creation.
    ///
    /// Allocating a sequence number and storing the event are two separate
    /// database operations. Without this guard, two concurrent local writes
    /// could read the same next sequence number and the second would land as a
    /// self-equivocation — the node accusing itself of forking its own log.
    local_append: Mutex<()>,
    /// The mesh, when one is attached. `None` means this node runs standalone,
    /// which is a fully supported mode rather than a failure.
    mesh: Option<Mutex<SyncEngine<Box<dyn MeshTransport>>>>,
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

        let database = Database::open(data_dir.join(DATABASE_FILE))?;
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
        };
        runtime.backfill_legacy_incidents()?;
        Ok(runtime)
    }

    /// Processes everything the mesh has delivered since the last call.
    ///
    /// Returns an empty report for a standalone node, so callers need no
    /// special case for running without a network.
    pub fn sync_tick(&self) -> CoreResult<SyncReport> {
        let Some(mesh) = &self.mesh else {
            return Ok(SyncReport::default());
        };
        let mut engine = mesh
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        engine.tick(&self.database, &self.identity)
    }

    /// How many connected peers are authorized, and how many are connected.
    pub fn authorized_peer_count(&self) -> CoreResult<(usize, usize)> {
        let Some(mesh) = &self.mesh else {
            return Ok((0, 0));
        };
        let engine = mesh
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
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
        let engine = mesh
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
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

    /// Whether a mesh transport is attached to this node.
    pub fn mesh_attached(&self) -> bool {
        self.mesh.is_some()
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
        self.database.approve_peer(&self.identity, node_id, note)
    }

    /// Refuses a peer that has never been trusted. Requires
    /// [`Capability::PeerEnroll`].
    pub fn reject_peer(&self, node_id: &str, note: Option<&str>) -> CoreResult<TrustState> {
        self.require_local_capability(Capability::PeerEnroll)?;
        self.database.reject_peer(&self.identity, node_id, note)
    }

    /// Withdraws authorization from a peer. Requires
    /// [`Capability::PeerRevoke`].
    pub fn revoke_peer(&self, node_id: &str, note: Option<&str>) -> CoreResult<TrustState> {
        self.require_local_capability(Capability::PeerRevoke)?;
        self.database.revoke_peer(&self.identity, node_id, note)
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
                },
            )?;
            self.database.backfill_incident_event(&event, &incident.id)?;
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

    /// Signs and applies an event authored by this node.
    fn append_local_event(
        &self,
        kind: EventKind,
        payload: impl Serialize,
    ) -> CoreResult<MeshEvent> {
        let _guard = self.lock_local_append();

        let sequence = self.database.next_local_sequence(self.identity.node_id())?;
        let event = MeshEvent::create(&self.identity, sequence, kind, payload)?;
        self.database
            .apply_event(&event, self.identity.node_id(), None)?;
        Ok(event)
    }

    /// The operator-facing node name, for startup logging and diagnostics.
    ///
    /// Separate from [`public_identity`](Self::public_identity) so that reading
    /// the name for a log line does not record an identity-disclosure audit
    /// event; that event should mean "the UI asked for the identity".
    pub fn node_name(&self) -> &str {
        self.identity.node_name()
    }

    /// The node's public identity. Never includes private key material.
    pub fn public_identity(&self) -> PublicIdentity {
        audit(
            AuditEvent::PublicIdentityDisclosed,
            AuditOutcome::Success,
            &format!("node={}", self.identity.node_name()),
        );
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
            },
        )?;

        audit(
            AuditEvent::IncidentCreated,
            AuditOutcome::Success,
            &format!("id={} event={}", validated.id, event.event_id),
        );

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
        let database = match (self.database.health_check(), self.database.count_incidents()) {
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
                (true, 0) => ComponentStatus::inactive(
                    "Listening",
                    "Encrypted QUIC mesh is running. No peers discovered yet — this is normal \
                     when operating alone."
                        .to_string(),
                ),
                (true, count) => ComponentStatus::operational(
                    "Connected",
                    format!(
                        "{count} authenticated peer(s) over encrypted QUIC. Discovery is local; \
                         no server is involved."
                    ),
                ),
            },
            // Phase 3. No model is bundled and no inference runtime is loaded.
            ai: ComponentStatus::inactive(
                "Not installed",
                "Local inference arrives in Phase 3. No model is present on this node."
                    .to_string(),
            ),
            // Phase 5. Claiming otherwise on a normal OS process would be false.
            tee: ComponentStatus::inactive(
                "Not available",
                "No trusted execution environment is in use. Keys are held in software."
                    .to_string(),
            ),
        }
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
        &self.database
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

        let incident = node.create_incident(input("Power line down", "HIGH")).unwrap();
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
        node.create_incident(input("check for leaks", "LOW")).unwrap();

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
            node.create_incident(input("before restart", "LOW")).unwrap();
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

        let incident = node.create_incident(input("awaiting a peer", "LOW")).unwrap();
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
            handle.join().unwrap().expect("concurrent create should succeed");
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

        let incident = node.create_incident(input("Initial report", "MEDIUM")).unwrap();
        node.add_observation(&incident.id, "Water level rising").unwrap();

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
            conn.execute("UPDATE incidents SET origin_event_id = NULL", []).unwrap();
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
}
