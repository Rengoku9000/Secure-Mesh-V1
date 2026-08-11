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

/** Error codes the core can return. Mirrors `error::CoreError::code`. */
export type CoreErrorCode =
  | "STORAGE_ERROR"
  | "IDENTITY_ERROR"
  | "VALIDATION_ERROR"
  | "NOT_FOUND"
  | "INTERNAL_ERROR";
