import { useCallback, useEffect, useState } from "react";
import { Alert } from "../components/Alert";
import { Panel } from "../components/Panel";
import { NodeHeader } from "../features/dashboard/NodeHeader";
import { NodeIdentityPanel } from "../features/dashboard/NodeIdentityPanel";
import { SystemStatusPanel } from "../features/dashboard/SystemStatusPanel";
import { CreateIncidentDialog } from "../features/incidents/CreateIncidentDialog";
import { IncidentTable } from "../features/incidents/IncidentTable";
import { AskSecureMesh } from "../features/intelligence/AskSecureMesh";
import { IntelligencePanel } from "../features/intelligence/IntelligencePanel";
import { PeerPanel } from "../features/network/PeerPanel";
import {
  CoreError,
  getIncidents,
  getLocalAuthority,
  getIntelligenceStatus,
  getNetworkStatus,
  getNodeIdentity,
  getPeers,
  getSystemStatus,
} from "../lib/ipc";
import type {
  Incident,
  IntelligenceStatus,
  LocalAuthority,
  NetworkStatus,
  Peer,
  PublicIdentity,
  SystemStatus,
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
  const [authority, setAuthority] = useState<LocalAuthority | null>(null);
  const [intelligence, setIntelligence] = useState<IntelligenceStatus | null>(null);
  const [incidents, setIncidents] = useState<Incident[]>([]);

  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [dialogOpen, setDialogOpen] = useState(false);

  const refresh = useCallback(async () => {
    try {
      // Fetched together so the header, status panel, peer list and timeline
      // always describe the same moment. Identity is absent deliberately: it
      // cannot change, so polling it would only add load.
      const [
        nextStatus,
        nextNetwork,
        nextPeers,
        nextIncidents,
        nextAuthority,
        nextIntelligence,
      ] = await Promise.all([
        getSystemStatus(),
        getNetworkStatus(),
        getPeers(),
        getIncidents(),
        getLocalAuthority(),
        getIntelligenceStatus(),
      ]);

      setSystemStatus(nextStatus);
      setNetwork(nextNetwork);
      setPeers(nextPeers);
      setIncidents(nextIncidents);
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

        <div className="offline-banner">
          <span className="offline-banner__marker">Offline-first</span>
          <span>
            {network?.detail ??
              "All records are stored locally on this node and require no network connection."}
          </span>
        </div>

        <div className="dashboard-grid">
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
            <IncidentTable incidents={incidents} loading={loading} />
          </Panel>

          <AskSecureMesh status={intelligence} />

          <div style={{ display: "grid", gap: "var(--space-5)" }}>
            <PeerPanel
              peers={peers}
              network={network}
              authority={authority}
              onChanged={() => void refresh()}
            />
            <IntelligencePanel status={intelligence} />
            <SystemStatusPanel status={systemStatus} />
            <NodeIdentityPanel identity={identity} />
          </div>
        </div>
      </main>

      <footer className="app-footer">
        SecureMesh Phase 2 · Peer-to-peer over encrypted QUIC, discovered
        locally. No cloud services, no server. Local AI and TEE are not yet
        implemented.
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
    </div>
  );
}
