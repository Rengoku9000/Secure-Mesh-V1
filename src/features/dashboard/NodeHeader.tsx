import { StatusDot } from "../../components/StatusDot";
import { ThemeSwitch } from "../../components/ThemeSwitch";
import type { NetworkStatus, PublicIdentity } from "../../types/core";

interface NodeHeaderProps {
  identity: PublicIdentity | null;
  network: NetworkStatus | null;
}

/**
 * The persistent header: which node this is, whether it is connected, and how
 * many peers it can see.
 *
 * The connectivity readout is deliberately prominent. An operator must be able
 * to tell at a glance that the node is running standalone, because that is the
 * normal and expected condition in the field, not an error.
 */
export function NodeHeader({ identity, network }: NodeHeaderProps) {
  const online = network?.online ?? false;

  return (
    <header className="node-header">
      <div className="node-header__inner">
        {/* The node name leads; the product name is context above it. An
            operator looking at three consoles needs to know which node this
            is before anything else. */}
        <div className="node-header__brand">
          <span className="node-header__mark">SecureMesh</span>
          <h1
            className="node-header__node"
            title={identity ? `Node ID: ${identity.nodeId}` : undefined}
          >
            {identity?.nodeName ?? "—"}
          </h1>
        </div>

        <div className="node-header__readouts">
          <div className="node-stat">
            <span className="node-stat__label">Link</span>
            <span className="node-stat__value">
              <StatusDot state={online ? "OPERATIONAL" : "INACTIVE"} />
              {online ? "ONLINE" : "OFFLINE"}
            </span>
          </div>

          <div className="node-stat">
            <span className="node-stat__label">Peers</span>
            <span className="node-stat__value">
              {network?.connectedPeers ?? 0}
              {network ? (
                <span className="node-stat__of">/ {network.knownPeers}</span>
              ) : null}
            </span>
          </div>

          <div className="node-stat">
            <span className="node-stat__label">Awaiting sync</span>
            <span className="node-stat__value">{network?.pendingSync ?? 0}</span>
          </div>
        </div>

        {/* Replaces the former full-width banner above the map. Same claim,
            no vertical cost. */}
        <span
          className="header-chip"
          title={
            network?.detail ??
            "All records are stored locally on this node and require no network connection."
          }
        >
          <span className="header-chip__glyph" aria-hidden="true">
            ◆
          </span>
          Offline-first
        </span>

        <ThemeSwitch />
      </div>
    </header>
  );
}
