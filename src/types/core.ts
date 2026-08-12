/**
 * TypeScript mirrors of the types the Rust core serialises across the Tauri
 * IPC boundary.
 *
 * These are hand-maintained. If a `#[derive(Serialize)]` struct in
 * `src-tauri/src` changes shape, the matching type here must change with it —
 * the compiler cannot check across the language boundary.
 *
 * Note what is absent: there is no field anywhere in this file for a private
 * key, because no command returns one.
 */

/** Operational urgency of an incident. Mirrors `domain::incident::Severity`. */
export type Severity = "LOW" | "MEDIUM" | "HIGH" | "CRITICAL";

export const SEVERITIES: readonly Severity[] = [
  "LOW",
  "MEDIUM",
  "HIGH",
  "CRITICAL",
] as const;

/** Whether a record has reached any peer. Mirrors `domain::incident::SyncStatus`. */
export type SyncStatus = "PENDING" | "SYNCING" | "SYNCED" | "FAILED";

/** Mirrors `runtime::ComponentState`. */
export type ComponentState = "OPERATIONAL" | "DEGRADED" | "INACTIVE";

/** Mirrors `identity::PublicIdentity` — the safe half of the node identity. */
export interface PublicIdentity {
  nodeId: string;
  nodeName: string;
  /** Hex-encoded Ed25519 public key. Public by design. */
  publicKey: string;
  algorithm: string;
  createdAt: string;
  /** Which keystore backend holds the private key. */
  keyBackend: string;
  /** True only when a hardware security module protects the key. */
  hardwareBacked: boolean;
}

/** Mirrors `runtime::ComponentStatus`. */
export interface ComponentStatus {
  state: ComponentState;
  label: string;
  detail: string;
}

/** Mirrors `runtime::SystemStatus`. */
export interface SystemStatus {
  database: ComponentStatus;
  identity: ComponentStatus;
  network: ComponentStatus;
  ai: ComponentStatus;
  tee: ComponentStatus;
}

/** Mirrors `runtime::NetworkStatus`. */
export interface NetworkStatus {
  online: boolean;
  connectedPeers: number;
  knownPeers: number;
  pendingSync: number;
  transport: string;
  detail: string;
}

/** Mirrors `domain::incident::Incident`. */
export interface Incident {
  id: string;
  createdBy: string;
  description: string;
  severity: Severity;
  latitude: number | null;
  longitude: number | null;
  createdAt: string;
  updatedAt: string;
  syncStatus: SyncStatus;
}

/** Mirrors `domain::incident::NewIncident` — untrusted input, validated in Rust. */
export interface NewIncident {
  description: string;
  severity: Severity;
  latitude: number | null;
  longitude: number | null;
}

/** Reachability of a peer. Mirrors `domain::peer::ConnectionState`. */
export type ConnectionState = "DISCONNECTED" | "CONNECTING" | "CONNECTED";

/**
 * Whether a peer is *authorized*. Mirrors `domain::trust::TrustState`.
 *
 * Independent of `ConnectionState`: a peer can be CONNECTED and REVOKED at the
 * same time — the session exists, but nothing is authorized to flow through it.
 */
export type TrustState = "UNKNOWN" | "PENDING" | "TRUSTED" | "REVOKED";

/** Mirrors `domain::trust::PeerRole`. */
export type PeerRole = "NODE" | "ADMIN";

/** Mirrors `domain::trust::Capability`. */
export type Capability =
  | "INCIDENT_CREATE"
  | "INCIDENT_READ"
  | "INCIDENT_SYNC"
  | "PEER_DISCOVER"
  | "PEER_ENROLL"
  | "PEER_REVOKE";

/** Mirrors `domain::trust::TrustEventKind`. */
export type TrustEventKind =
  | "peer.enrollment.requested"
  | "peer.enrollment.approved"
  | "peer.enrollment.rejected"
  | "peer.revoked"
  | "peer.reinstated";

/** An entry in the local trust audit log. Mirrors `domain::trust::TrustEvent`. */
export interface TrustEvent {
  id: string;
  /** Local monotonic ordering — not wall-clock time. */
  sequence: number;
  nodeId: string;
  kind: TrustEventKind;
  fromState: TrustState | null;
  toState: TrustState;
  actorNode: string;
  occurredAt: string;
  detail: string | null;
}

/**
 * What the local operator may do. Mirrors `commands::trust::LocalAuthority`.
 *
 * Used to decide which controls to *render*. It is not the security control:
 * the Rust core re-checks the same capabilities on every call, so a frontend
 * that ignored these flags would still be refused.
 */
export interface LocalAuthority {
  nodeId: string;
  nodeName: string;
  role: PeerRole;
  capabilities: Capability[];
  canEnroll: boolean;
  canRevoke: boolean;
}

/**
 * A peer node. Mirrors `domain::peer::Peer`.
 *
 * Public identity material only — there is no command that returns any
 * private key, this node's or a peer's.
 */
export interface Peer {
  nodeId: string;
  nodeName: string;
  /** Hex Ed25519 public key, verified during the transport handshake. */
  publicKey: string;
  transportPeerId: string | null;
  connectionState: ConnectionState;
  lastSeen: string | null;
  protocolVersion: number | null;
  capabilities: string[];
  /** True once this peer has been caught signing two different events at the
   *  same sequence number. Replication from it stops advancing. */
  equivocating: boolean;
  /** Events held locally that this peer has not acknowledged. */
  pendingEvents: number;
  firstSeen: string;

  // --- Authorization ---
  trustState: TrustState;
  role: PeerRole;
  grantedCapabilities: Capability[];
  enrolledAt: string | null;
  enrolledBy: string | null;
  revokedAt: string | null;
  revokedBy: string | null;
  trustNotes: string | null;
}

/** A note appended to an incident. Mirrors `domain::incident::Observation`. */
export interface Observation {
  id: string;
  incidentId: string;
  authorNode: string;
  note: string;
  createdAt: string;
}

// --- Local intelligence (Phase 3) ---

/** Category a local model assigned. Mirrors `domain::IncidentCategory`. */
export type IncidentCategory =
  | "INFRASTRUCTURE"
  | "FLOODING"
  | "FIRE"
  | "MEDICAL"
  | "EVACUATION"
  | "POWER"
  | "COMMUNICATIONS"
  | "RESOURCE_SHORTAGE"
  | "EARTHQUAKE"
  | "SEVERE_WEATHER"
  | "ROAD_BLOCKAGE"
  | "OTHER";

/** Mirrors `domain::AccessStatus`. */
export type AccessStatus = "OPEN" | "RESTRICTED" | "BLOCKED" | "UNKNOWN";

/**
 * Derived intelligence about an incident. Mirrors `domain::IncidentAnalysis`.
 *
 * Derived, not authoritative: the incident and its signed event remain the
 * source of truth, and `severity` here is the *model's* opinion, deliberately
 * separate from the severity an operator set.
 */
export interface IncidentAnalysis {
  incidentId: string;
  category: IncidentCategory;
  severity: Severity;
  summary: string;
  asset: string | null;
  cause: string | null;
  accessStatus: AccessStatus;
  entities: string[];
  affectedResources: string[];
  locationHint: string | null;
  /** Model-stated confidence, clamped to 0..1. Absent when it gave none. */
  confidence: number | null;
  modelId: string;
  latencyMs: number;
  generatedAt: string;
}

/** Mirrors `ai::IntelligenceStatus`. */
export interface IntelligenceStatus {
  state: "READY" | "LOADING" | "UNAVAILABLE";
  detail: string;
  modelName: string | null;
  modelId: string | null;
  quantisation: string | null;
  /** Always "LOCAL". */
  inference: string;
  /** Always "NONE". */
  networkDependency: string;
  embeddingModel: string | null;
  analysesStored: number;
  documentsIndexed: number;
  chunksIndexed: number;
  vectorsStored: number;
}

/** A passage an answer was built from. Mirrors `ai::rag::AnswerSource`. */
export interface AnswerSource {
  marker: string;
  kind: "KNOWLEDGE_CHUNK" | "INCIDENT";
  subjectId: string;
  title: string;
  score: number;
  excerpt: string;
  /** Whether the model actually cited this, as opposed to merely being offered it. */
  cited: boolean;
}

/** A grounded answer. Mirrors `ai::rag::GroundedAnswer`. */
export interface GroundedAnswer {
  question: string;
  answer: string;
  sources: AnswerSource[];
  /** True when the answer cites at least one supplied passage. */
  grounded: boolean;
  /** True when the corpus could not answer and the refusal was returned. */
  refused: boolean;
  /**
   * Source numbers the model returned that did not exist. They were dropped
   * before reaching here; a non-zero count is a reason to distrust the answer.
   */
  droppedCitations: number;
  modelId: string;
  retrievalMs: number;
  generationMs: number;
}

/** Mirrors `ai::IndexReport`. */
export interface IndexReport {
  chunksEmbedded: number;
  incidentsEmbedded: number;
  failures: number;
}

/** Mirrors `storage::intelligence::KnowledgeDocument`. */
export interface KnowledgeDocument {
  id: string;
  title: string;
  source: string;
  sourceType: string;
  contentHash: string;
  importedAt: string;
  chunkCount: number;
}

/** Error codes the core can return. Mirrors `error::CoreError::code`. */
export type CoreErrorCode =
  | "STORAGE_ERROR"
  | "IDENTITY_ERROR"
  | "VALIDATION_ERROR"
  | "NOT_FOUND"
  | "INTERNAL_ERROR";
