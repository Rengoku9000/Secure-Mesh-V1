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
        <div className="node-header__brand">
          <h1 className="node-header__title">SecureMesh</h1>
          <span className="node-header__tagline">
            Confidential edge node · offline-first
          </span>
        </div>

        <div className="node-stat">
          <span className="node-stat__label">Node</span>
          <span
            className="node-stat__value"
            title={identity ? `Node ID: ${identity.nodeId}` : undefined}
          >
            {identity?.nodeName ?? "—"}
          </span>
        </div>

        <div className="node-stat">
          <span className="node-stat__label">Status</span>
          <span className="node-stat__value">
            <StatusDot state={online ? "OPERATIONAL" : "INACTIVE"} />
            {online ? "ONLINE" : "OFFLINE"}
          </span>
        </div>

        <div className="node-stat">
          <span className="node-stat__label">Peers</span>
          <span className="node-stat__value">{network?.connectedPeers ?? 0}</span>
        </div>

        <ThemeSwitch />
      </div>
    </header>
  );
}
