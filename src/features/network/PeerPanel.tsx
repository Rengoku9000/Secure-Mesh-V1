import { Panel } from "../../components/Panel";
import { StatusDot } from "../../components/StatusDot";
import { formatRelative, shortenId } from "../../lib/format";
import type { NetworkStatus, Peer } from "../../types/core";

interface PeerPanelProps {
  peers: Peer[];
  network: NetworkStatus | null;
}

function PeerRow({ peer }: { peer: Peer }) {
  const connected = peer.connectionState === "CONNECTED";

  return (
    <li className="peer-row">
      <div className="peer-row__identity">
        <span className="peer-row__name" title={`Node ID: ${peer.nodeId}`}>
          {peer.nodeName}
        </span>
        <span className="peer-row__id mono">{shortenId(peer.nodeId, 8, 4)}</span>
      </div>

      <div className="peer-row__state">
        <StatusDot state={connected ? "OPERATIONAL" : "INACTIVE"} />
        <span>{peer.connectionState}</span>
      </div>

      <div className="peer-row__meta">
        {peer.lastSeen ? `Seen ${formatRelative(peer.lastSeen)}` : "Never seen"}
      </div>

      <div className="peer-row__sync">
        {peer.equivocating ? (
          // A node that signed two different events at one sequence number.
          // Surfaced prominently: records already received are kept, but
          // replication from it has stopped.
          <span className="peer-row__warning">Conflicting history</span>
        ) : peer.pendingEvents === 0 ? (
          <span>Synced</span>
        ) : (
          <span>{peer.pendingEvents} pending</span>
        )}
      </div>
    </li>
  );
}

/**
 * The mesh: who this node can see, and how far replication has got with each.
 *
 * An empty list is a normal operating state, not a fault — a SecureMesh node
 * is designed to work alone — so the empty message says so plainly rather than
 * reading like an error.
 */
export function PeerPanel({ peers, network }: PeerPanelProps) {
  const connectedCount = peers.filter((p) => p.connectionState === "CONNECTED").length;

  return (
    <Panel
      title="Network"
      subtitle={
        network
          ? `${connectedCount} connected · ${peers.length} known`
          : undefined
      }
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
            <span className="network-summary__label">Awaiting sync</span>
            <span className="network-summary__value">{network.pendingSync}</span>
          </div>
        </div>
      )}

      {peers.length === 0 ? (
        <p className="peer-empty">
          No peers discovered. This node operates independently and keeps every
          record locally; peers on the same network are found automatically.
        </p>
      ) : (
        <ul className="peer-list">
          {peers.map((peer) => (
            <PeerRow key={peer.nodeId} peer={peer} />
          ))}
        </ul>
      )}
    </Panel>
  );
}
