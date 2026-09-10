import { useState } from "react";
import { Panel } from "../../components/Panel";
import { StatusDot } from "../../components/StatusDot";
import { EncryptedId } from "../../components/EncryptedId";
import { formatRelative } from "../../lib/format";
import { approvePeer, CoreError, rejectPeer, revokePeer } from "../../lib/ipc";
import type {
  LocalAuthority,
  NetworkStatus,
  Peer,
  TrustState,
} from "../../types/core";

interface PeerPanelProps {
  peers: Peer[];
  network: NetworkStatus | null;
  authority: LocalAuthority | null;
  /** Re-reads state from the core after a decision. */
  onChanged: () => void;
}

/** How each authorization state is presented. */
const TRUST_PRESENTATION: Record<
  TrustState,
  { label: string; meaning: string; className: string }
> = {
  UNKNOWN: {
    label: "UNKNOWN",
    meaning: "Not enrolled — no data exchanged",
    className: "trust-badge--unknown",
  },
  PENDING: {
    label: "PENDING",
    meaning: "Enrollment required",
    className: "trust-badge--pending",
  },
  TRUSTED: {
    label: "TRUSTED",
    meaning: "Incident sync",
    className: "trust-badge--trusted",
  },
  REVOKED: {
    label: "REVOKED",
    meaning: "Access denied",
    className: "trust-badge--revoked",
  },
};

interface PeerRowProps {
  peer: Peer;
  authority: LocalAuthority | null;
  busy: boolean;
  onDecision: (peer: Peer, action: "approve" | "reject" | "revoke") => void;
}

function PeerRow({ peer, authority, busy, onDecision }: PeerRowProps) {
  const presentation = TRUST_PRESENTATION[peer.trustState];
  const connected = peer.connectionState === "CONNECTED";

  // Which controls to *draw*. Not a security decision: the Rust core re-checks
  // the same capabilities on every call, so a frontend that rendered these
  // anyway would still be refused.
  const canEnroll = authority?.canEnroll ?? false;
  const canRevoke = authority?.canRevoke ?? false;
  const awaitingDecision = peer.trustState === "PENDING" || peer.trustState === "UNKNOWN";

  return (
    <li className="peer-row">
      <div className="peer-row__identity">
        <span className="peer-row__name" title={`Node ID: ${peer.nodeId}`}>
          {peer.nodeName}
        </span>
        <EncryptedId id={peer.nodeId} lead={8} tail={4} type="node" />
      </div>

      <div className="peer-row__badges">
        <span className={`trust-badge ${presentation.className}`}>
          {presentation.label}
        </span>
        <span className="peer-row__state">
          <StatusDot state={connected ? "OPERATIONAL" : "INACTIVE"} />
          {peer.connectionState}
        </span>
      </div>

      <div className="peer-row__meta">
        {peer.lastSeen ? `Seen ${formatRelative(peer.lastSeen)}` : "Never seen"}
        {peer.role === "ADMIN" && <span className="peer-row__role"> · ADMIN</span>}
      </div>

      <div className="peer-row__sync">
        {peer.equivocating ? (
          <span className="peer-row__warning">Conflicting history</span>
        ) : peer.trustState === "TRUSTED" ? (
          <span>
            {peer.pendingEvents === 0 ? "Synced" : `${peer.pendingEvents} pending`}
          </span>
        ) : (
          <span className="text-muted">{presentation.meaning}</span>
        )}
      </div>

      {(canEnroll || canRevoke) && (
        <div className="peer-row__actions">
          {awaitingDecision && canEnroll && (
            <>
              <button
                type="button"
                className="button button--primary button--compact"
                disabled={busy}
                onClick={() => onDecision(peer, "approve")}
              >
                Approve
              </button>
              <button
                type="button"
                className="button button--secondary button--compact"
                disabled={busy}
                onClick={() => onDecision(peer, "reject")}
              >
                Reject
              </button>
            </>
          )}

          {peer.trustState === "TRUSTED" && canRevoke && (
            <button
              type="button"
              className="button button--secondary button--compact"
              disabled={busy}
              onClick={() => onDecision(peer, "revoke")}
            >
              Revoke
            </button>
          )}

          {peer.trustState === "REVOKED" && canEnroll && (
            <button
              type="button"
              className="button button--secondary button--compact"
              disabled={busy}
              onClick={() => onDecision(peer, "approve")}
            >
              Reinstate
            </button>
          )}
        </div>
      )}
    </li>
  );
}

/**
 * The mesh: who this node can see, whether each one is authorized, and how far
 * replication has got.
 *
 * An empty list is a normal operating state, not a fault. So is a peer sitting
 * at PENDING — a connected node that has not been enrolled is exactly what the
 * authorization layer is for.
 */
export function PeerPanel({ peers, network, authority, onChanged }: PeerPanelProps) {
  const [busyNodeId, setBusyNodeId] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const connectedCount = peers.filter((p) => p.connectionState === "CONNECTED").length;
  const pendingCount = peers.filter((p) => p.trustState === "PENDING").length;

  async function handleDecision(peer: Peer, action: "approve" | "reject" | "revoke") {
    setBusyNodeId(peer.nodeId);
    setError(null);
    try {
      if (action === "approve") {
        await approvePeer(peer.nodeId);
      } else if (action === "reject") {
        await rejectPeer(peer.nodeId);
      } else {
        await revokePeer(peer.nodeId);
      }
      onChanged();
    } catch (raw) {
      const coreError = raw as CoreError;
      setError(coreError.message ?? "The decision could not be recorded.");
    } finally {
      setBusyNodeId(null);
    }
  }

  return (
    <Panel
      title="Peers"
      subtitle={`${connectedCount} connected · ${peers.length} known`}
    >
      {network && (
        <div className="network-summary">
          <div className="network-summary__row">
            <span className="network-summary__label">Status</span>
            <span className="network-summary__value">
              <StatusDot state={network.online ? "OPERATIONAL" : "INACTIVE"} />
              {network.online ? "ONLINE" : "OFFLINE"}
            </span>
          </div>
          <div className="network-summary__row">
            <span className="network-summary__label">Transport</span>
            <span className="network-summary__value mono">
              {network.transport.toUpperCase()}
            </span>
          </div>
          <div className="network-summary__row">
            <span className="network-summary__label">Your role</span>
            <span className="network-summary__value">{authority?.role ?? "—"}</span>
          </div>
        </div>
      )}

      {pendingCount > 0 && (
        <div className="enrollment-notice" role="status">
          <strong>{pendingCount}</strong> peer{pendingCount === 1 ? "" : "s"} awaiting
          enrollment. Nothing is shared with them until approved.
        </div>
      )}

      {error && (
        <div className="alert" role="alert">
          <div className="alert__body">{error}</div>
        </div>
      )}

      {peers.length === 0 ? (
        <p className="peer-empty">
          No peers discovered. This node operates independently and keeps every
          record locally; peers on the same network are found automatically, and
          exchange nothing until you enroll them.
        </p>
      ) : (
        <ul className="peer-list">
          {peers.map((peer) => (
            <PeerRow
              key={peer.nodeId}
              peer={peer}
              authority={authority}
              busy={busyNodeId === peer.nodeId}
              onDecision={handleDecision}
            />
          ))}
        </ul>
      )}
    </Panel>
  );
}
