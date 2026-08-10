import { Panel } from "../../components/Panel";
import { formatTimestamp, shortenId } from "../../lib/format";
import type { PublicIdentity } from "../../types/core";

/**
 * The node's cryptographic identity.
 *
 * Only public material is displayed, because only public material crosses the
 * IPC boundary — the private key never leaves the Rust identity module. The
 * "key storage" row reports the backend truthfully so an operator can tell
 * whether the key is held in software or in security hardware.
 */
export function NodeIdentityPanel({ identity }: { identity: PublicIdentity | null }) {
  if (!identity) {
    return (
      <Panel title="Node identity">
        <div className="key-value" aria-busy="true">
          <div className="skeleton" style={{ height: 32 }} />
          <div className="skeleton" style={{ height: 32 }} />
        </div>
      </Panel>
    );
  }

  return (
    <Panel title="Node identity" subtitle={identity.algorithm}>
      <dl className="key-value">
        <div className="key-value__row">
          <dt className="key-value__key">Node name</dt>
          <dd className="key-value__value">{identity.nodeName}</dd>
        </div>
        <div className="key-value__row">
          <dt className="key-value__key">Node ID</dt>
          <dd className="key-value__value" title={identity.nodeId}>
            {shortenId(identity.nodeId, 16, 8)}
          </dd>
        </div>
        <div className="key-value__row">
          <dt className="key-value__key">Public key</dt>
          <dd className="key-value__value" title={identity.publicKey}>
            {shortenId(identity.publicKey, 16, 8)}
          </dd>
        </div>
        <div className="key-value__row">
          <dt className="key-value__key">Key storage</dt>
          <dd className="key-value__value">
            {identity.keyBackend}
            {identity.hardwareBacked ? " (hardware-backed)" : " (software)"}
          </dd>
        </div>
        <div className="key-value__row">
          <dt className="key-value__key">Created</dt>
          <dd className="key-value__value">{formatTimestamp(identity.createdAt)}</dd>
        </div>
      </dl>
    </Panel>
  );
}
