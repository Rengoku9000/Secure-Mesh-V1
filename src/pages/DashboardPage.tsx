import { useCallback, useEffect, useMemo, useState } from "react";
import { Alert } from "../components/Alert";
import { BottomNav, type NavTab } from "../components/BottomNav";
import { Panel } from "../components/Panel";
import { EncryptedId } from "../components/EncryptedId";
import { NodeHeader } from "../features/dashboard/NodeHeader";
import { NodeIdentityPanel } from "../features/dashboard/NodeIdentityPanel";
import { SystemStatusPanel } from "../features/dashboard/SystemStatusPanel";
import { CreateIncidentDialog } from "../features/incidents/CreateIncidentDialog";
import { IncidentCard } from "../features/incidents/IncidentCard";
import { IncidentDetailsDialog } from "../features/incidents/IncidentDetailsDialog";
import { TacticalMap } from "../features/map/TacticalMap";
import { AskAiDrawer } from "../features/intelligence/AskAiDrawer";
import { IntelligencePanel } from "../features/intelligence/IntelligencePanel";
import { KnowledgeBasePanel } from "../features/intelligence/KnowledgeBasePanel";
import { PeerPanel } from "../features/network/PeerPanel";
import {
  CoreError,
  getIncidentIndexStates,
  getIncidents,
  getLocalAuthority,
  getIntelligenceStatus,
  getNetworkStatus,
  getNodeIdentity,
  getPeerLocations,
  getPeers,
  getSystemStatus,
} from "../lib/ipc";
import { formatRelative } from "../lib/format";
import { useLocalAnnotations, type DisputeReason } from "../lib/localAnnotations";
import type {
  Incident,
  IncidentIndexState,
  IndexState,
  IntelligenceStatus,
  LocalAuthority,
  NetworkStatus,
  Peer,
  PublicIdentity,
  SystemStatus,
  PeerLocationView,
  Severity,
} from "../types/core";

const REFRESH_INTERVAL_MS = 2000;

export function DashboardPage() {
  const [identity, setIdentity] = useState<PublicIdentity | null>(null);
  const [systemStatus, setSystemStatus] = useState<SystemStatus | null>(null);
  const [network, setNetwork] = useState<NetworkStatus | null>(null);
  const [peers, setPeers] = useState<Peer[]>([]);
  const [peerLocations, setPeerLocations] = useState<PeerLocationView[]>([]);
  const [authority, setAuthority] = useState<LocalAuthority | null>(null);
  const [intelligence, setIntelligence] = useState<IntelligenceStatus | null>(null);
  const [incidents, setIncidents] = useState<Incident[]>([]);
  const [indexStates, setIndexStates] = useState<IncidentIndexState[]>([]);

  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [dialogOpen, setDialogOpen] = useState(false);
  const [selected, setSelected] = useState<Incident | null>(null);
  const [detailsOpen, setDetailsOpen] = useState(false);
  const [aiDrawerOpen, setAiDrawerOpen] = useState(false);

  // Edit node name dialog state
  const [editNameOpen, setEditNameOpen] = useState(false);
  const [newName, setNewName] = useState("");
  const [nameError, setNameError] = useState<string | null>(null);

  const { callSign, setCallSign, disputeFor, flagIncident, clearFlag } =
    useLocalAnnotations();

  // Global Ctrl+K / Cmd+K listener to toggle Ask SecureMesh AI drawer from anywhere
  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "k") {
        event.preventDefault();
        setAiDrawerOpen((prev) => !prev);
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, []);

  // Active view tab for mobile / tablet layout
  const [activeTab, setActiveTab] = useState<NavTab>("map");

  // Filter state for incidents feed
  const [severityFilter, setSeverityFilter] = useState<Severity | "ALL">("ALL");
  const [searchQuery, setSearchQuery] = useState("");

  const refresh = useCallback(async () => {
    try {
      const [
        nextStatus,
        nextNetwork,
        nextPeers,
        nextPeerLocations,
        nextIncidents,
        nextIndexStates,
        nextAuthority,
        nextIntelligence,
      ] = await Promise.all([
        getSystemStatus(),
        getNetworkStatus(),
        getPeers(),
        getPeerLocations(),
        getIncidents(),
        getIncidentIndexStates(),
        getLocalAuthority(),
        getIntelligenceStatus(),
      ]);

      setSystemStatus(nextStatus);
      setNetwork(nextNetwork);
      setPeers(nextPeers);
      setPeerLocations(nextPeerLocations);
      setIncidents(nextIncidents);
      setIndexStates(nextIndexStates);
      setAuthority(nextAuthority);
      setIntelligence(nextIntelligence);
      setError(null);
    } catch (raw) {
      const coreError = raw as CoreError;
      setError(coreError.message ?? "The SecureMesh core did not respond.");
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    let cancelled = false;

    void getNodeIdentity()
      .then((value) => {
        if (!cancelled) setIdentity(value);
      })
      .catch((raw: CoreError) => {
        if (!cancelled) {
          setError(raw.message ?? "The SecureMesh core did not respond.");
        }
      });

    return () => {
      cancelled = true;
    };
  }, []);

  useEffect(() => {
    void refresh();
    const timer = window.setInterval(() => void refresh(), REFRESH_INTERVAL_MS);
    return () => window.clearInterval(timer);
  }, [refresh]);

  const indexStateMap = useMemo(
    () => new Map<string, IndexState>(indexStates.map((e) => [e.incidentId, e.state])),
    [indexStates],
  );

  const filteredIncidents = useMemo(() => {
    return incidents.filter((incident) => {
      if (severityFilter !== "ALL" && incident.severity !== severityFilter) {
        return false;
      }
      if (searchQuery.trim() !== "") {
        const query = searchQuery.toLowerCase();
        const matchesDesc = incident.description.toLowerCase().includes(query);
        const matchesId = incident.id.toLowerCase().includes(query);
        if (!matchesDesc && !matchesId) return false;
      }
      return true;
    });
  }, [incidents, severityFilter, searchQuery]);

  const counts = useMemo(() => {
    const res: Record<Severity | "ALL", number> = {
      ALL: incidents.length,
      CRITICAL: 0,
      HIGH: 0,
      MEDIUM: 0,
      LOW: 0,
    };
    for (const inc of incidents) {
      if (res[inc.severity] !== undefined) {
        res[inc.severity]++;
      }
    }
    return res;
  }, [incidents]);

  const pendingCount = network?.pendingSync ?? 0;
  const unapprovedPeersCount = peers.filter((p) => p.trustState === "PENDING").length;

  const handleLocateOnMap = useCallback((incident: Incident) => {
    setSelected(incident);
    setDetailsOpen(false);
    setActiveTab("map");
  }, []);

  // The call sign sits beside the node's real name rather than replacing it:
  // `identity.nodeName` is what peers see and stays exactly as the keystore
  // recorded it.
  const handleOpenEditName = useCallback(() => {
    setNewName(callSign ?? "");
    setNameError(null);
    setEditNameOpen(true);
  }, [callSign]);

  const handleSaveName = useCallback(() => {
    const trimmed = newName.trim();
    if (trimmed.length > 32) {
      setNameError("Call sign cannot exceed 32 characters");
      return;
    }
    // Empty clears the call sign and falls back to the node's own name.
    setNameError(null);
    setCallSign(trimmed === "" ? null : trimmed);
    setEditNameOpen(false);
  }, [newName, setCallSign]);

  // Reports are evidence. One that turns out to be wrong is marked with this
  // node's own assessment and kept, rather than deleted.
  const handleFlagIncident = useCallback(
    (id: string, reason: DisputeReason, note?: string) => {
      flagIncident(id, reason, note);
    },
    [flagIncident],
  );

  return (
    <div className="mobile-app-shell">
      <NodeHeader
        identity={identity}
        network={network}
        onOpenAi={() => setAiDrawerOpen(true)}
        onEditName={handleOpenEditName}
        callSign={callSign}
      />

      <main className="mobile-main">
        {error && <Alert title="Core unavailable" message={error} />}

        {/* 1. MAP VIEW (Always mounted so Canvas projection and Pan/Zoom are preserved) */}
        <div
          className={`mobile-view mobile-view--map ${activeTab === "map" ? "mobile-view--active" : "mobile-view--hidden"}`}
        >
          <div className="mobile-map-container">
            <TacticalMap
              incidents={incidents}
              peers={peers}
              peerLocations={peerLocations}
              identity={identity}
              indexStates={indexStates}
              status={systemStatus?.map}
              selected={selected}
              onSelect={setSelected}
              onOpenDetails={(incident) => {
                setSelected(incident);
                setActiveTab("incidents");
                setDetailsOpen(false);
              }}
            />
          </div>

          {/* Tactical Overview Strip directly below the map */}
          <div className="map-tactical-strip">
            <div className="map-stat-chip">
              <span className="map-stat-chip__icon">
                <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                  <path d="m21.73 18-8-14a2 2 0 0 0-3.48 0l-8 14A2 2 0 0 0 4 21h16a2 2 0 0 0 1.73-3Z" />
                  <line x1="12" y1="9" x2="12" y2="13" />
                  <line x1="12" y1="17" x2="12.01" y2="17" />
                </svg>
              </span>
              <div className="map-stat-chip__content">
                <span className="map-stat-chip__val">{incidents.length}</span>
                <span className="map-stat-chip__lbl">Incidents</span>
              </div>
            </div>
            <div className="map-stat-chip">
              <span className="map-stat-chip__icon">
                <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                  <circle cx="18" cy="5" r="3" />
                  <circle cx="6" cy="12" r="3" />
                  <circle cx="18" cy="19" r="3" />
                  <line x1="8.59" y1="13.51" x2="15.42" y2="17.49" />
                  <line x1="15.41" y1="6.51" x2="8.59" y2="10.49" />
                </svg>
              </span>
              <div className="map-stat-chip__content">
                <span className="map-stat-chip__val">{peers.length}</span>
                <span className="map-stat-chip__lbl">Mesh Nodes</span>
              </div>
            </div>
            <div className="map-stat-chip">
              <span className="map-stat-chip__icon">
                <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                  <path d="M5 12.55a11 11 0 0 1 14.08 0" />
                  <path d="M1.42 9a16 16 0 0 1 21.16 0" />
                  <path d="M8.53 16.11a6 6 0 0 1 6.95 0" />
                  <line x1="12" y1="20" x2="12.01" y2="20" />
                </svg>
              </span>
              <div className="map-stat-chip__content">
                <span className="map-stat-chip__val">{network?.connectedPeers ?? 0}</span>
                <span className="map-stat-chip__lbl">Connected</span>
              </div>
            </div>
          </div>

          {/* Incident Quick Details Card when selected */}
          {selected ? (
            <div className="map-selected-card">
              <div className="map-selected-card__header">
                <div className="map-selected-card__badge-row">
                  <span className={`badge badge--severity badge--${selected.severity.toLowerCase()}`}>
                    {selected.severity}
                  </span>
                  <span className="map-selected-card__id">
                    <EncryptedId id={selected.id} lead={6} tail={4} />
                  </span>
                  <span className="map-selected-card__time">
                    {formatRelative(selected.createdAt)}
                  </span>
                </div>
                <button
                  type="button"
                  className="map-selected-card__close"
                  onClick={() => setSelected(null)}
                  aria-label="Dismiss preview"
                >
                  ✕
                </button>
              </div>
              <p className="map-selected-card__desc">{selected.description}</p>
              <div className="map-selected-card__footer">
                <span className="map-selected-card__geo">
                  {selected.latitude != null && selected.longitude != null ? (
                    <>
                      <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true" style={{ verticalAlign: "-1px", marginRight: "4px" }}>
                        <path d="M21 10c0 7-9 13-9 13s-9-6-9-13a9 9 0 0 1 18 0z" />
                        <circle cx="12" cy="10" r="3" />
                      </svg>
                      {selected.latitude.toFixed(5)}, {selected.longitude.toFixed(5)}
                    </>
                  ) : (
                    "No GPS fix"
                  )}
                </span>
                <div className="map-selected-card__actions">
                  <button
                    type="button"
                    className="button button--primary button--compact"
                    onClick={() => {
                      setActiveTab("incidents");
                      setDetailsOpen(true);
                    }}
                  >
                    View Record →
                  </button>
                </div>
              </div>
            </div>
          ) : (
            <div className="map-recent-strip">
              <div className="map-recent-strip__header">
                <span className="map-recent-strip__title">Quick Incident Pointers</span>
                <span className="map-recent-strip__subtitle">Tap to locate on map</span>
              </div>
              <div className="map-recent-strip__list">
                {incidents.slice(0, 3).map((inc) => (
                  <button
                    key={inc.id}
                    type="button"
                    className="map-recent-item"
                    onClick={() => setSelected(inc)}
                  >
                    <span className={`badge badge--severity badge--${inc.severity.toLowerCase()}`}>
                      {inc.severity}
                    </span>
                    <span className="map-recent-item__desc">{inc.description}</span>
                    <span className="map-recent-item__time">{formatRelative(inc.createdAt)}</span>
                  </button>
                ))}
                {incidents.length === 0 && (
                  <div className="map-recent-empty">No incidents reported in local mesh</div>
                )}
              </div>
            </div>
          )}
        </div>

        {/* 2. INCIDENTS VIEW */}
        {activeTab === "incidents" && (
          <div className="mobile-view mobile-view--scrollable">
            <div className="mobile-section-header">
              <div className="mobile-section-header__title-row">
                <h2 className="mobile-section-title">Incident Feed</h2>
                <span className="mobile-section-subtitle">
                  {incidents.length} stored · {pendingCount} awaiting sync
                </span>
              </div>

              {/* Severity filter pills with count badges */}
              <div className="filter-chips" role="toolbar" aria-label="Filter incidents by severity">
                {(["ALL", "CRITICAL", "HIGH", "MEDIUM", "LOW"] as const).map((sev) => (
                  <button
                    key={sev}
                    type="button"
                    className={`filter-chip ${severityFilter === sev ? "filter-chip--active" : ""}`}
                    onClick={() => setSeverityFilter(sev)}
                  >
                    <span>{sev}</span>
                    <span className="filter-chip__count">{counts[sev]}</span>
                  </button>
                ))}
              </div>

              {/* Search input */}
              <div className="mobile-search-bar">
                <div className="mobile-search-wrapper">
                  <svg
                    width="15"
                    height="15"
                    viewBox="0 0 24 24"
                    fill="none"
                    stroke="currentColor"
                    strokeWidth="2.2"
                    className="mobile-search-icon"
                    aria-hidden="true"
                  >
                    <circle cx="11" cy="11" r="8" />
                    <line x1="21" y1="21" x2="16.65" y2="16.65" />
                  </svg>
                  <input
                    type="search"
                    className="input mobile-search-input"
                    placeholder="Search description, station, or ID..."
                    value={searchQuery}
                    onChange={(e) => setSearchQuery(e.target.value)}
                    aria-label="Search incidents"
                  />
                </div>
                {searchQuery && (
                  <button
                    type="button"
                    className="button button--ghost button--compact"
                    onClick={() => setSearchQuery("")}
                  >
                    Clear
                  </button>
                )}
              </div>
            </div>

            {loading ? (
              <div className="loading-stack" aria-busy="true">
                {[0, 1, 2].map((i) => (
                  <div key={i} className="skeleton skeleton--card" />
                ))}
              </div>
            ) : filteredIncidents.length === 0 ? (
              <div className="empty-state">
                <p className="empty-state__title">
                  {incidents.length === 0 ? "No incidents recorded" : "No matching incidents"}
                </p>
                <p className="empty-state__hint">
                  {incidents.length === 0
                    ? "Tap '+ Report' to create an incident. Stored locally without Internet."
                    : "Try adjusting your search or severity filter."}
                </p>
              </div>
            ) : (
              <div className="incident-card-list">
                {filteredIncidents.map((incident) => (
                  <IncidentCard
                    key={incident.id}
                    incident={incident}
                    indexState={indexStateMap.get(incident.id)}
                    dispute={disputeFor(incident.id)}
                    isSelected={selected?.id === incident.id}
                    onSelect={(inc) => {
                      setSelected(inc);
                      setDetailsOpen(true);
                    }}
                    onLocateOnMap={handleLocateOnMap}
                  />
                ))}
              </div>
            )}
          </div>
        )}

        {/* 3. MESH NETWORK VIEW */}
        {activeTab === "mesh" && (
          <div className="mobile-view mobile-view--scrollable">
            <div className="mobile-section-header">
              <h2 className="mobile-section-title">Mesh Network</h2>
              <span className="mobile-section-subtitle">
                {peers.length} discovered · {unapprovedPeersCount} pending approval
              </span>
            </div>

            <div className="mobile-panel-stack">
              <PeerPanel
                peers={peers}
                network={network}
                authority={authority}
                onChanged={() => void refresh()}
              />
            </div>
          </div>
        )}

        {/* 4. LOCAL AI / INTELLIGENCE VIEW */}
        {activeTab === "intel" && (
          <div className="mobile-view mobile-view--scrollable">
            <div className="mobile-section-header">
              <div style={{ display: "flex", justifyContent: "space-between", alignItems: "flex-start", width: "100%" }}>
                <div>
                  <h2 className="mobile-section-title">Field Intelligence & Protocols</h2>
                  <span className="mobile-section-subtitle">On-device edge inference, vectors & manuals</span>
                </div>
                <button
                  type="button"
                  className="button button--primary button--compact"
                  onClick={() => setAiDrawerOpen(true)}
                  title="Ask SecureMesh AI (Ctrl+K)"
                  style={{ gap: "6px" }}
                >
                  <svg
                    width="14"
                    height="14"
                    viewBox="0 0 24 24"
                    fill="none"
                    stroke="currentColor"
                    strokeWidth="2.2"
                    strokeLinecap="round"
                    strokeLinejoin="round"
                    aria-hidden="true"
                  >
                    <path d="M12 2v4M12 18v4M4.93 4.93l2.83 2.83M16.24 16.24l2.83 2.83" />
                    <circle cx="12" cy="12" r="3" />
                  </svg>
                  <span>Query AI (Ctrl+K)</span>
                </button>
              </div>
            </div>

            <div className="mobile-panel-stack">
              <IntelligencePanel status={intelligence} />
              <KnowledgeBasePanel
                status={intelligence}
                onChanged={() => void refresh()}
              />
            </div>
          </div>
        )}

        {/* 5. NODE & SYSTEM VIEW */}
        {activeTab === "node" && (
          <div className="mobile-view mobile-view--scrollable">
            <div className="mobile-section-header">
              <h2 className="mobile-section-title">Node Diagnostics</h2>
              <span className="mobile-section-subtitle">Local storage & cryptographic identity</span>
            </div>

            <div className="mobile-panel-stack">
              <NodeIdentityPanel
                identity={identity}
                onEditName={handleOpenEditName}
                callSign={callSign}
              />
              <SystemStatusPanel status={systemStatus} />

              <Panel title="Edge Transport & Protocol">
                <div className="key-value">
                  <div className="key-value__row">
                    <span className="key-value__key">Transport</span>
                    <span className="key-value__value">libp2p QUIC · TLS 1.3 encrypted</span>
                  </div>
                  <div className="key-value__row">
                    <span className="key-value__key">Discovery</span>
                    <span className="key-value__value">Local link mDNS broadcast</span>
                  </div>
                  <div className="key-value__row">
                    <span className="key-value__key">Storage</span>
                    <span className="key-value__value">SQLite 3 embedded on this device</span>
                  </div>
                  <div className="key-value__row">
                    <span className="key-value__key">Platform</span>
                    <span className="key-value__value">Tauri 2 · Rust Core (Zero Cloud)</span>
                  </div>
                </div>
              </Panel>
            </div>
          </div>
        )}

        {/* Tactical Floating Action Button (FAB) for 1-thumb quick incident creation */}
        {(activeTab === "map" || activeTab === "incidents") && (
          <button
            type="button"
            className="tactical-fab"
            onClick={() => setDialogOpen(true)}
            aria-label="Report new incident"
            title="Create Incident (1-tap)"
          >
            <svg
              width="24"
              height="24"
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              strokeWidth="2.5"
              strokeLinecap="round"
              strokeLinejoin="round"
              aria-hidden="true"
            >
              <line x1="12" y1="5" x2="12" y2="19" />
              <line x1="5" y1="12" x2="19" y2="12" />
            </svg>
            <span className="tactical-fab__label">Report</span>
          </button>
        )}
      </main>

      <BottomNav
        activeTab={activeTab}
        onChange={setActiveTab}
        pendingSyncCount={pendingCount}
        unapprovedPeersCount={unapprovedPeersCount}
      />

      {/* Touch-Friendly Dialogs */}
      {dialogOpen && (
        <CreateIncidentDialog
          onClose={() => setDialogOpen(false)}
          onCreated={() => {
            void refresh();
          }}
        />
      )}

      {selected && detailsOpen && (
        <IncidentDetailsDialog
          incident={incidents.find((item) => item.id === selected.id) ?? selected}
          onClose={() => setDetailsOpen(false)}
          onLocateOnMap={handleLocateOnMap}
          dispute={disputeFor(selected.id)}
          onFlag={handleFlagIncident}
          onClearFlag={clearFlag}
          intelligence={intelligence}
          onOpenIncident={(incidentId) => {
            const next = incidents.find((item) => item.id === incidentId);
            if (next) setSelected(next);
          }}
        />
      )}

      {/* Edit Node Display Name / Alias Dialog */}
      {editNameOpen && (
        <div
          className="dialog-backdrop"
          role="dialog"
          aria-modal="true"
          aria-labelledby="edit-node-name-title"
          onClick={() => setEditNameOpen(false)}
        >
          <div
            className="dialog"
            style={{ maxWidth: "420px" }}
            onClick={(e) => e.stopPropagation()}
          >
            <header className="dialog__header">
              <h2 id="edit-node-name-title" className="dialog__title">
                Set Local Call Sign
              </h2>
              <button
                type="button"
                className="dialog__close-btn"
                onClick={() => setEditNameOpen(false)}
                aria-label="Close"
              >
                ✕
              </button>
            </header>

            <div className="dialog__body">
              <p style={{ fontSize: "13px", color: "var(--text-secondary)", marginBottom: "14px", lineHeight: "1.4" }}>
                A call sign for your own reference on this device. The node's
                registered name and its cryptographic Node ID are unchanged, and
                peers continue to see the node exactly as before. Leave blank to
                clear it.
              </p>

              <form
                onSubmit={(e) => {
                  e.preventDefault();
                  handleSaveName();
                }}
              >
                <div className="field">
                  <label htmlFor="node-name-input" className="field__label">
                    Call sign
                  </label>
                  <span className="field__hint">
                    Registered as {identity?.nodeName ?? "unknown"}
                  </span>
                  <input
                    id="node-name-input"
                    type="text"
                    className="input"
                    value={newName}
                    maxLength={32}
                    onChange={(e) => setNewName(e.target.value)}
                    placeholder="e.g. ALPHA-BASE-01"
                    autoFocus
                  />
                  {nameError && (
                    <span style={{ color: "var(--danger-fg)", fontSize: "12px", marginTop: "4px" }}>
                      {nameError}
                    </span>
                  )}
                </div>

                <div style={{ display: "flex", justifyContent: "flex-end", gap: "8px", marginTop: "20px" }}>
                  <button
                    type="button"
                    className="button button--secondary"
                    onClick={() => setEditNameOpen(false)}
                  >
                    Cancel
                  </button>
                  <button type="submit" className="button button--primary">
                    {newName.trim() ? "Save call sign" : "Clear call sign"}
                  </button>
                </div>
              </form>
            </div>
          </div>
        </div>
      )}

      {/* Global Slide-Over Tactical AI Drawer */}
      <AskAiDrawer
        isOpen={aiDrawerOpen}
        onClose={() => setAiDrawerOpen(false)}
        status={intelligence}
        onNavigateToIntel={() => {
          setActiveTab("intel");
        }}
      />
    </div>
  );
}
