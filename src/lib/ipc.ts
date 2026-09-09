/**
 * The single place the frontend talks to the Rust core.
 *
 * Every call goes through Tauri IPC to a local process. There is no HTTP
 * client in this application and no external endpoint to configure — a
 * SecureMesh node has nothing to reach out to.
 *
 * Components import the functions below rather than calling `invoke`
 * directly, so command names and argument shapes exist in exactly one place.
 */

import { invoke } from "@tauri-apps/api/core";
import type {
  CoreErrorCode,
  DeviceLocation,
  GroundedAnswer,
  Incident,
  IncidentAnalysis,
  IndexReport,
  IntelligenceStatus,
  LocationPermission,
  IncidentIndexState,
  KnowledgeDocument,
  LocalAuthority,
  NetworkStatus,
  NewIncident,
  Observation,
  Peer,
  PublicIdentity,
  SystemStatus,
  TrustEvent,
  TrustState,
  InstallReport,
  KnowledgeBaseSummary,
  Basemap,
  PeerLocationView,
} from "../types/core";

/**
 * An error raised by the Rust core, carrying the stable code the core
 * assigned. Components branch on `code`, never on message text.
 */
export class CoreError extends Error {
  readonly code: CoreErrorCode | "IPC_ERROR";

  constructor(code: CoreErrorCode | "IPC_ERROR", message: string) {
    super(message);
    this.name = "CoreError";
    this.code = code;
  }

  /** True when the operator can fix this by changing their input. */
  get isValidation(): boolean {
    return this.code === "VALIDATION_ERROR";
  }
}

/**
 * Normalises whatever `invoke` rejected with into a `CoreError`.
 *
 * The core returns `{ code, message }`, but a transport-level failure can
 * reject with a bare string or an `Error`, so every shape is handled rather
 * than assumed.
 */
function toCoreError(raw: unknown): CoreError {
  if (raw instanceof CoreError) {
    return raw;
  }

  if (typeof raw === "object" && raw !== null && "code" in raw && "message" in raw) {
    const { code, message } = raw as { code: unknown; message: unknown };
    if (typeof code === "string" && typeof message === "string") {
      return new CoreError(code as CoreErrorCode, message);
    }
  }

  if (typeof raw === "string") {
    return new CoreError("IPC_ERROR", raw);
  }

  if (raw instanceof Error) {
    return new CoreError("IPC_ERROR", raw.message);
  }

  return new CoreError("IPC_ERROR", "The SecureMesh core returned an unexpected error.");
}

async function call<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  try {
    return await invoke<T>(command, args);
  } catch (raw) {
    throw toCoreError(raw);
  }
}

/** This node's public identity. Never includes private key material. */
export function getNodeIdentity(): Promise<PublicIdentity> {
  return call<PublicIdentity>("get_node_identity");
}

/** Health of every subsystem, for the status panel. */
export function getSystemStatus(): Promise<SystemStatus> {
  return call<SystemStatus>("get_system_status");
}

/** Mesh connectivity and the backlog awaiting propagation. */
export function getNetworkStatus(): Promise<NetworkStatus> {
  return call<NetworkStatus>("get_network_status");
}

/**
 * Creates an incident.
 *
 * The input is validated in Rust, not here. Any client-side checking exists
 * only to give faster feedback; it is never the enforcement point.
 */
export function createIncident(input: NewIncident): Promise<Incident> {
  return call<Incident>("create_incident", { input });
}

/** Recent incidents, newest first. The core clamps `limit`. */
export function getIncidents(limit?: number): Promise<Incident[]> {
  return call<Incident[]>("get_incidents", { limit: limit ?? null });
}

/** A single incident by ID. Rejects with `NOT_FOUND` if it does not exist. */
export function getIncident(id: string): Promise<Incident> {
  return call<Incident>("get_incident", { id });
}

/** Known peers, with live connection state and per-peer sync backlog. */
export function getPeers(): Promise<Peer[]> {
  return call<Peer[]>("get_peers");
}

/**
 * What the local operator is permitted to do.
 *
 * Used only to decide which controls to render. The Rust core enforces the
 * same capabilities on every call below, so hiding a button is presentation,
 * never protection.
 */
export function getLocalAuthority(): Promise<LocalAuthority> {
  return call<LocalAuthority>("get_local_authority");
}

/** Authorizes a peer, or reinstates a revoked one. */
export function approvePeer(nodeId: string, note?: string): Promise<TrustState> {
  return call<TrustState>("approve_peer", { nodeId, note: note ?? null });
}

/** Refuses a peer that has never been authorized. */
export function rejectPeer(nodeId: string, note?: string): Promise<TrustState> {
  return call<TrustState>("reject_peer", { nodeId, note: note ?? null });
}

/** Withdraws authorization from a peer. */
export function revokePeer(nodeId: string, note?: string): Promise<TrustState> {
  return call<TrustState>("revoke_peer", { nodeId, note: note ?? null });
}

// --- Local intelligence (Phase 3) ---
//
// Every call here goes to a model running on this device. There is no key to
// configure and no request that leaves the machine.

/** Model, readiness, and index sizes for the Intelligence panel. */
export function getIntelligenceStatus(): Promise<IntelligenceStatus> {
  return call<IntelligenceStatus>("get_intelligence_status");
}

/**
 * Analyses one incident with the local model.
 *
 * Slow — seconds on CPU — so this is always an explicit operator action, never
 * automatic. Analysis must not sit on the path of incident capture.
 */
export function analyseIncident(incidentId: string): Promise<IncidentAnalysis> {
  return call<IncidentAnalysis>("analyse_incident", { incidentId });
}

/**
 * The stored analysis for an incident.
 *
 * Resolves to `null` rather than rejecting when no model is provisioned, so the
 * incident view renders identically on a node without AI.
 */
export function getIncidentAnalysis(
  incidentId: string,
): Promise<IncidentAnalysis | null> {
  return call<IncidentAnalysis | null>("get_incident_analysis", { incidentId });
}

/** Answers a question from this node's own records. */
export function askSecureMesh(
  question: string,
  topK?: number,
): Promise<GroundedAnswer> {
  return call<GroundedAnswer>("ask_securemesh", {
    question,
    topK: topK ?? null,
  });
}

/**
 * Embeds anything not yet indexed.
 *
 * Indexing is automatic — the Rust core triggers a pass after a local write and
 * after replication applies events — so this is a manual retry, not the
 * mechanism. The UI must never be the thing that remembers to index.
 */
export function indexIntelligence(): Promise<IndexReport> {
  return call<IndexReport>("index_intelligence");
}

/** Where each incident stands in the local vector index. */
export function getIncidentIndexStates(): Promise<IncidentIndexState[]> {
  return call<IncidentIndexState[]>("get_incident_index_states");
}

// --- Device location ---
//
// The position comes from the operating system on this device. There is no
// geocoding service, no map tile server and no API key. Reading a position
// writes nothing: it becomes part of the record only when the operator submits
// it with an incident.

/** Whether this device will report a position. Never prompts. */
export function getLocationPermission(): Promise<LocationPermission> {
  return call<LocationPermission>("get_location_permission");
}

/**
 * Asks the platform for location access.
 *
 * May show a system prompt, so call it only from an explicit operator action.
 */
export function requestLocationPermission(): Promise<LocationPermission> {
  return call<LocationPermission>("request_location_permission");
}

/**
 * Takes one position fix.
 *
 * Rejects rather than resolving to a placeholder when the platform cannot
 * answer — an invented coordinate is indistinguishable from a real one.
 */
export function getCurrentLocation(): Promise<DeviceLocation> {
  return call<DeviceLocation>("get_current_location");
}

/** Documents in the local knowledge base. */
export function getKnowledgeDocuments(): Promise<KnowledgeDocument[]> {
  return call<KnowledgeDocument[]>("get_knowledge_documents");
}

/**
 * Positions authorized peers have reported over the mesh.
 *
 * Read-only and cheap — it returns what the core already holds in memory.
 * Reading it takes no fix, contacts no peer, and writes nothing.
 */
export function getPeerLocations(): Promise<PeerLocationView[]> {
  return call<PeerLocationView[]>("get_peer_locations");
}

/**
 * The provisioned basemap description, or `null` when none is installed.
 *
 * Cheap: reports what is installed without returning its geometry.
 */
export function getMapBasemap(): Promise<Basemap | null> {
  return call<Basemap | null>("get_map_basemap");
}

/**
 * The basemap geometry.
 *
 * Read once when the map mounts, never on the refresh cycle. Reads a local
 * file; there is no tile server and no request leaves the machine.
 */
export function getMapGeojson(): Promise<string> {
  return call<string>("get_map_geojson");
}

/** Counts of local knowledge. Read-only; valid on a node with no model. */
export function getKnowledgeSummary(): Promise<KnowledgeBaseSummary> {
  return call<KnowledgeBaseSummary>("get_knowledge_summary");
}

/**
 * Installs the operational knowledge pack compiled into the binary.
 *
 * Nothing is downloaded and no file is read — the documents ship inside the
 * executable. Idempotent: a second call reports everything already present.
 */
export function installOperationalKnowledge(): Promise<InstallReport> {
  return call<InstallReport>("install_operational_knowledge");
}

/** The local trust audit log, newest first. */
export function getTrustAuditLog(nodeId?: string, limit?: number): Promise<TrustEvent[]> {
  return call<TrustEvent[]>("get_trust_audit_log", {
    nodeId: nodeId ?? null,
    limit: limit ?? null,
  });
}

/** Observations appended to an incident, from this node or any peer. */
export function getObservations(incidentId: string): Promise<Observation[]> {
  return call<Observation[]>("get_observations", { incidentId });
}

/**
 * Appends an observation to an incident.
 *
 * Phase 2 records developments by appending rather than editing, so two nodes
 * updating the same incident while partitioned cannot lose each other's work.
 */
export function addObservation(incidentId: string, note: string): Promise<void> {
  return call<void>("add_observation", { incidentId, note });
}
