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

/**
 * Whether a record is searchable by local AI. Mirrors `ai::indexer::IndexState`.
 *
 * Independent of {@link SyncStatus}: an incident can be shared with every peer
 * and still not be indexed here, because a vector is derived local state that
 * each node builds for itself rather than something the mesh carries.
 */
export type IndexState = "NOT_INDEXED" | "INDEXING" | "INDEXED" | "INDEX_FAILED";

/**
 * Whether this device will report a position. Mirrors
 * `location::LocationPermission`.
 */
export type LocationPermission =
  | "NOT_REQUESTED"
  | "GRANTED"
  | "DENIED"
  | "UNAVAILABLE";

/**
 * How the platform arrived at a position. Mirrors `location::LocationSource`.
 *
 * Shown to the operator because the difference matters: a satellite fix is
 * metres and works with no network, whereas an IP-derived one is city-level and
 * required the OS to reach the Internet.
 */
export type LocationSource = "SATELLITE" | "WIRELESS" | "IP_ADDRESS" | "UNKNOWN";

/**
 * How an incident record describes where its coordinates came from. Mirrors
 * `domain::incident::LocationSource`.
 *
 * Deliberately coarser than {@link LocationSource}: the record keeps the one
 * distinction a reader can act on — off a satellite, or not. The translation
 * from the platform vocabulary is done in Rust, and the UI never performs it,
 * so there is no second copy of the rule to drift.
 */
export type IncidentLocationSource = "GNSS" | "WIRELESS" | "UNKNOWN";

/** One position fix, exactly as the platform reported it. */
export interface DeviceLocation {
  latitude: number;
  longitude: number;
  /** Radius of uncertainty in metres, when the platform supplies one. */
  accuracyMeters: number | null;
  altitudeMeters: number | null;
  headingDegrees: number | null;
  speedMps: number | null;
  source: LocationSource;
  capturedAt: string;
}

/** Mirrors `ai::indexer::IncidentIndexState`. */
export interface IncidentIndexState {
  incidentId: string;
  state: IndexState;
}

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
  location: ComponentStatus;
  /**
   * Whether offline geographic data is installed. Deliberately unrelated to
   * network reachability — the map never uses the network either way.
   */
  map: ComponentStatus;
  tee: ComponentStatus;
}

/**
 * How much of a peer's reported position to trust, given its age.
 * Mirrors `domain::peer_location::LocationFreshness`.
 */
export type LocationFreshness = "CURRENT" | "STALE" | "EXPIRED";

/**
 * A position a peer reported over the mesh. Mirrors
 * `domain::peer_location::PeerLocationView`.
 *
 * Ephemeral operational state: held in memory by the core, never written to the
 * event log, and never replicated onward. `nodeId` is the **authenticated**
 * sender — the heartbeat body carries no identifier a peer could spoof.
 */
export interface PeerLocationView {
  nodeId: string;
  latitude: number;
  longitude: number;
  /** Reported accuracy radius. `null` means none was reported, never zero. */
  accuracyMeters: number | null;
  /** How the peer obtained it. A wireless fix is never relabelled as GNSS. */
  locationSource: IncidentLocationSource;
  /** When the peer measured it — not when this node received it. */
  capturedAt: string;
  /** When this node received it. Distinct from `capturedAt`. */
  receivedAt: string;
  /** The origin's monotonic counter, which decides which update is newer. */
  sequence: number;
  freshness: LocationFreshness;
  /** Seconds since receipt, for "updated N min ago". */
  ageSeconds: number;
}

/** Geographic extent of a basemap. Mirrors `map::BoundingBox`. */
export interface BoundingBox {
  minLatitude: number;
  minLongitude: number;
  maxLatitude: number;
  maxLongitude: number;
}

/**
 * A provisioned offline basemap, described without its geometry.
 *
 * Mirrors `map::Basemap`. Separate from the GeoJSON itself so the dashboard can
 * poll status without dragging megabytes of coastline across IPC.
 */
export interface Basemap {
  name: string;
  path: string;
  bytes: number;
  featureCount: number;
  bounds: BoundingBox;
  /** SHA-256 of the file, so an operator can confirm what is installed. */
  sha256: string;
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
  /**
   * Reported accuracy radius in metres. `null` means the reading carried no
   * figure — never that it was exact.
   */
  accuracyMeters: number | null;
  /** Provenance of the coordinates. `UNKNOWN` for hand-entered positions. */
  locationSource: IncidentLocationSource;
  /**
   * When the position was measured, as distinct from `createdAt`, which is when
   * the incident was filed.
   */
  locationCapturedAt: string | null;
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
  /**
   * Supplied only alongside coordinates. The core refuses provenance with no
   * position to describe, because it would imply a measurement never taken.
   *
   * The source is sent as the platform reported it; Rust maps it onto the
   * record's vocabulary.
   */
  accuracyMeters: number | null;
  locationSource: LocationSource | null;
  locationCapturedAt: string | null;
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
  /**
   * Always `null` on analyses produced from Phase 7 onwards: the model states
   * no calibrated confidence, so `RawAnalysis::validate` discards whatever it
   * gives. Retained because rows stored before that change may still hold a
   * clamped value. Never rendered as a number — see
   * `features/intelligence/confidence.ts`.
   */
  confidence: number | null;
  modelId: string;
  latencyMs: number;
  generatedAt: string;
}

/**
 * One field where the model and the deterministic rule layer disagree.
 * Mirrors `ai::consistency::Disagreement`.
 */
export interface Disagreement {
  /** The analysis field in question. */
  field: string;
  /** What the model said. */
  modelResult: string;
  /** What rules over the same report text derived independently. */
  deterministicEvidence: string;
  /** Why this is surfaced, in operator-facing terms. */
  reason: string;
}

/**
 * What the deterministic layer makes of one analysis.
 * Mirrors `ai::consistency::ConsistencyReport`.
 *
 * An empty `disagreements` list means the two agree on everything that *can*
 * be checked — not that the analysis is correct. `uncheckedFields` names what
 * had no deterministic source at all.
 */
export interface ConsistencyReport {
  disagreements: Disagreement[];
  uncheckedFields: string[];
  needsOperatorReview: boolean;
}

/**
 * An analysis together with the evidence about it.
 * Mirrors `ai::consistency::AnalysisOutcome`.
 *
 * The two are separate on purpose: `analysis` is exactly what the model
 * produced, and `consistency` is what the rules make of it. The rule layer is
 * evidence for an operator, never an automatic replacement for the model.
 */
export interface AnalysisOutcome {
  analysis: IncidentAnalysis;
  consistency: ConsistencyReport;
}

// --- Laptop-side insight (derived, local, never replicated) ---

/** Finer-grained incident type. Mirrors `ai::nlp::Hazard`. */
export type Hazard =
  | "FIRE"
  | "EXPLOSION"
  | "MEDICAL_EMERGENCY"
  | "ACCIDENT"
  | "STRUCTURAL_DAMAGE"
  | "FLOOD"
  | "LANDSLIDE"
  | "TRAPPED_PERSONS"
  | "MISSING_PERSONS"
  | "ROAD_BLOCKAGE"
  | "POWER_FAILURE"
  | "INFRASTRUCTURE_FAILURE"
  | "COMMUNICATION_FAILURE"
  | "EARTHQUAKE"
  | "SEVERE_WEATHER"
  | "EVACUATION"
  | "RESOURCE_SHORTAGE"
  | "HAZARDOUS_MATERIAL";

export interface HazardMention {
  hazard: Hazard;
  cue: string;
  negated: boolean;
  resolved: boolean;
  headline: boolean;
}

export type PersonStatus =
  | "DECEASED"
  | "TRAPPED"
  | "MISSING"
  | "INJURED"
  | "AT_RISK"
  | "DISPLACED"
  | "AFFECTED";

export interface PeopleMention {
  count: number | null;
  approximate: boolean;
  status: PersonStatus;
  subset: boolean;
  text: string;
}

export interface PeopleSummary {
  deceased: number | null;
  trapped: number | null;
  missing: number | null;
  injured: number | null;
  displaced: number | null;
  affected: number | null;
  atRisk: number | null;
  approximate: boolean;
  unquantified: boolean;
}

export interface RouteMention {
  text: string;
  blocked: boolean;
}

export interface QuantityMention {
  value: number;
  unit: string;
  approximate: boolean;
}

export interface SeverityFactor {
  label: string;
  weight: number;
}

export interface SeverityAssessment {
  level: Severity;
  score: number;
  reason: string;
  factors: SeverityFactor[];
}

/** What the rule layer found in one report. Mirrors `ai::nlp::TextExtraction`. */
export interface TextExtraction {
  version: string;
  category: IncidentCategory;
  categoryConfidence: number;
  categoryScores: { category: IncidentCategory; score: number }[];
  hazards: HazardMention[];
  people: PeopleMention[];
  peopleSummary: PeopleSummary;
  locations: string[];
  routes: RouteMention[];
  structures: string[];
  organizations: string[];
  times: string[];
  quantities: QuantityMention[];
  urgencyCues: string[];
  severity: SeverityAssessment;
}

export type Relation = "DUPLICATE" | "POSSIBLE_DUPLICATE" | "RELATED";
export type MatchMethod = "SEMANTIC" | "LEXICAL";
export type CategoryMethod = "LEXICAL" | "SEMANTIC" | "NONE";

export interface RelatedIncident {
  incidentId: string;
  excerpt: string;
  operatorSeverity: Severity;
  createdAt: string;
  similarity: number;
  relation: Relation;
  method: MatchMethod;
  sameCategory: boolean;
  sharedHazards: Hazard[];
  distanceKm: number | null;
  hoursApart: number;
}

/**
 * Everything derived about one incident on this device. Mirrors
 * `ai::insight::IncidentInsight`. Computed on demand, never stored or sent.
 */
export interface IncidentInsight {
  incidentId: string;
  category: IncidentCategory;
  categoryMethod: CategoryMethod;
  semanticCategory: { category: IncidentCategory; similarity: number } | null;
  extraction: TextExtraction;
  modelAgrees: boolean | null;
  related: RelatedIncident[];
  severityDiffersFromOperator: boolean;
  semanticAvailable: boolean;
  elapsedMs: number;
}

export interface BriefPriority {
  incidentId: string;
  excerpt: string;
  category: IncidentCategory;
  derivedSeverity: Severity;
  score: number;
  reason: string;
  operatorSeverity: Severity;
  createdAt: string;
}

/** Mirrors `ai::insight::SituationBrief`. */
export interface SituationBrief {
  incidentsConsidered: number;
  categories: { category: IncidentCategory; count: number }[];
  derivedSeverity: { level: Severity; count: number }[];
  people: {
    deceased: number;
    trapped: number;
    missing: number;
    injured: number;
    atRisk: number;
    reportsWithPeople: number;
    approximate: boolean;
  };
  blockedRoutes: string[];
  priorities: BriefPriority[];
  duplicateGroups: string[][];
  semanticAvailable: boolean;
  summary: string | null;
  summaryModel: string | null;
  summarySupport: number | null;
  summaryNote: string | null;
  elapsedMs: number;
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

/**
 * What a retrieved passage is to a reader. Mirrors
 * `storage::intelligence::PassageSource`.
 *
 * Distinct from the embedding kind, which only records which table a vector
 * points into. This is the distinction an operator acts on: standing guidance
 * carries different weight from an unverified field report, and an answer that
 * draws on both must not present them identically.
 */
export type PassageSource =
  | "OPERATIONAL_KNOWLEDGE"
  | "LIVE_INCIDENT"
  | "IMPORTED_DOCUMENT";

/** A passage an answer was built from. Mirrors `ai::rag::AnswerSource`. */
export interface AnswerSource {
  marker: string;
  kind: "KNOWLEDGE_CHUNK" | "INCIDENT";
  /** Whether this citation is guidance, a live incident, or an import. */
  source: PassageSource;
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
  /**
   * Share of the answer's content words found in the passages it cites.
   *
   * An answer that cites real passages and then draws on the model's training
   * scores near zero here, which is what turns it into a refusal. Shown so an
   * operator can see how closely an answer tracks its sources rather than
   * taking `grounded` on trust.
   */
  answerSupport: number;
  modelId: string;
  retrievalMs: number;
  generationMs: number;
}

/** What an operational knowledge install did. Mirrors `knowledge_pack::InstallReport`. */
export interface InstallReport {
  documentsInstalled: number;
  /** Reported separately from installed, so a repeat install visibly does nothing. */
  documentsAlreadyPresent: number;
  chunksCreated: number;
}

/** Counts of local knowledge. Mirrors `ai::KnowledgeBaseSummary`. */
export interface KnowledgeBaseSummary {
  operationalDocuments: number;
  importedDocuments: number;
  /** Incidents holding a vector, and therefore actually searchable. */
  liveIncidentsIndexed: number;
  /** All incidents held. A gap from the indexed count means indexing is
   *  catching up — normal, not an error. */
  liveIncidentsTotal: number;
  chunks: number;
  vectors: number;
  packDocumentsAvailable: number;
  packInstalled: boolean;
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
