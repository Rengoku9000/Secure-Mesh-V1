import { useCallback, useEffect, useState } from "react";
import { Alert } from "../components/Alert";
import { Panel } from "../components/Panel";
import { NodeHeader } from "../features/dashboard/NodeHeader";
import { NodeIdentityPanel } from "../features/dashboard/NodeIdentityPanel";
import { SystemStatusPanel } from "../features/dashboard/SystemStatusPanel";
import { CreateIncidentDialog } from "../features/incidents/CreateIncidentDialog";
import { IncidentDetailsDialog } from "../features/incidents/IncidentDetailsDialog";
import { IncidentTable } from "../features/incidents/IncidentTable";
import { TacticalMap } from "../features/map/TacticalMap";
import { AskSecureMesh } from "../features/intelligence/AskSecureMesh";
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
import type {
  Incident,
  IncidentIndexState,
  IntelligenceStatus,
  LocalAuthority,
  NetworkStatus,
  Peer,
  PublicIdentity,
  SystemStatus,
  PeerLocationView,
} from "../types/core";

/**
 * How often the dashboard re-reads mesh state.
 *
 * Peers appear and disappear on their own, so a static view would go stale
 * without the operator touching anything.
 */
const REFRESH_INTERVAL_MS = 2000;

/**
 * State that changes, and state that does not.
 *
 * The node's identity is fixed for the life of the process — it is loaded from
 * the keystore once at startup and never mutated — so re-reading it on every
 * tick was pure churn. Mesh state genuinely changes underneath the operator and
 * has to be re-read; identity is fetched once and shared by every component
 * that needs it.
 */

/**
 * The single screen of the Phase 1 application.
 *
 * All state comes from the Rust core; this component orchestrates fetching and
 * rendering but makes no operational decisions of its own.
 */
export function DashboardPage() {
  const [identity, setIdentity] = useState<PublicIdentity | null>(null);
  const [systemStatus, setSystemStatus] = useState<SystemStatus | null>(null);
  const [network, setNetwork] = useState<NetworkStatus | null>(null);
  const [peers, setPeers] = useState<Peer[]>([]);
  // Ephemeral: what peers have reported about themselves, held by the core in
  // memory. Never persisted here, never replicated onward.
  const [peerLocations, setPeerLocations] = useState<PeerLocationView[]>([]);
  const [authority, setAuthority] = useState<LocalAuthority | null>(null);
  const [intelligence, setIntelligence] = useState<IntelligenceStatus | null>(null);
  const [incidents, setIncidents] = useState<Incident[]>([]);
  const [indexStates, setIndexStates] = useState<IncidentIndexState[]>([]);

  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [dialogOpen, setDialogOpen] = useState(false);
  // One selection, shared by the map popup, the table highlight and the map
  // camera. Whether the details *dialog* is open is a different question —
  // clicking a marker shows its card, and only "View incident" opens the full
  // record — so that is tracked separately rather than by overloading
  // selection with two meanings.
  const [selected, setSelected] = useState<Incident | null>(null);
  const [detailsOpen, setDetailsOpen] = useState(false);

  const refresh = useCallback(async () => {
    try {
      // Fetched together so the header, status panel, peer list and timeline
      // always describe the same moment. Identity is absent deliberately: it
      // cannot change, so polling it would only add load.
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

  // Identity is read once. It is fixed for the life of the process, so a second
  // read could only ever return the same answer.
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

    // Peers connect and disconnect without the operator doing anything, so the
    // view has to keep re-reading rather than rendering once.
    const timer = window.setInterval(() => void refresh(), REFRESH_INTERVAL_MS);
    return () => window.clearInterval(timer);
  }, [refresh]);

  const pendingCount = network?.pendingSync ?? 0;

  return (
    <div className="app-shell">
      <NodeHeader identity={identity} network={network} />

      <main className="app-main">
        {error && <Alert title="Core unavailable" message={error} />}

        {/*
          Operations layout. The primary column carries the map and the
          incident timeline; the rail carries peer, network and node state.
          Below 1280px the rail moves beneath the primary column rather than
          competing with it for width — see `.ops-layout` in app.css.

          This is a nesting change only. Every child receives exactly the props
          it did before, and no handler, state value or IPC call is altered.
        */}
        <div className="ops-layout">
          <div className="ops-primary">
            {/* The map is the fastest way to read where things are, and it
                shares the dashboard's single selection state with the timeline
                below rather than keeping its own. */}
            <div className="dashboard-map">
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
                  setDetailsOpen(true);
                }}
              />
            </div>

            <Panel
              title="Recent incidents"
              subtitle={
                incidents.length === 0
                  ? "Stored locally on this node"
                  : `${incidents.length} record(s) · ${pendingCount} awaiting sync`
              }
              flush
              actions={
                <button
                  type="button"
                  className="button button--primary"
                  onClick={() => setDialogOpen(true)}
                >
                  Create incident
                </button>
              }
            >
              <IncidentTable
                incidents={incidents}
                loading={loading}
                indexStates={indexStates}
                onSelect={(incident) => {
                  setSelected(incident);
                  setDetailsOpen(true);
                }}
              />
            </Panel>
          </div>

          {/* Operational state: who we can reach, whether the subsystems are
              healthy, and which node this is. Priorities 2-4, so they sit
              beside the map rather than below the timeline. */}
          <aside className="ops-rail" aria-label="Node and network status">
            <PeerPanel
              peers={peers}
              network={network}
              authority={authority}
              onChanged={() => void refresh()}
            />
            <SystemStatusPanel status={systemStatus} />
            <NodeIdentityPanel identity={identity} />
          </aside>
        </div>

        {/* Supporting capability, deliberately last. With no model provisioned
            these panels report themselves unavailable and the console above is
            unaffected. */}
        <section className="ops-secondary" aria-label="Local intelligence">
          <AskSecureMesh status={intelligence} />
          <IntelligencePanel status={intelligence} />
          <KnowledgeBasePanel
            status={intelligence}
            onChanged={() => void refresh()}
          />
        </section>
      </main>

      <footer className="app-footer">
        <span className="app-footer__item">
          <span className="app-footer__label">Storage</span>
          <span>Local SQLite on this node</span>
        </span>
        <span className="app-footer__item">
          <span className="app-footer__label">Transport</span>
          <span>libp2p QUIC, TLS 1.3 · mDNS discovery on the local link</span>
        </span>
        <span className="app-footer__item">
          <span className="app-footer__label">Network</span>
          <span>
            {network?.detail ??
              "All records are stored locally and require no network connection."}
          </span>
        </span>
      </footer>

      {dialogOpen && (
        <CreateIncidentDialog
          onClose={() => setDialogOpen(false)}
          onCreated={() => {
            // Re-read from the core rather than appending locally, so the
            // timeline and the counts always reflect what was persisted.
            void refresh();
          }}
        />
      )}

      {selected && detailsOpen && (
        <IncidentDetailsDialog
          // Re-read from the current list so the dialog follows a refresh
          // rather than showing a snapshot taken when the row was clicked.
          incident={incidents.find((item) => item.id === selected.id) ?? selected}
          // Closing the record leaves it selected, so the map keeps its
          // highlight and popup where the operator left them.
          onClose={() => setDetailsOpen(false)}
        />
      )}
    </div>
  );
}
