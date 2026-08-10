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
