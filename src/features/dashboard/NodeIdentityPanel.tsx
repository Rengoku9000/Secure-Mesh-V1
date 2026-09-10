import { Panel } from "../../components/Panel";
import { EncryptedId } from "../../components/EncryptedId";
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
interface NodeIdentityPanelProps {
  identity: PublicIdentity | null;
  onEditName?: () => void;
  /** Operator's local call sign for this node, if one is set. */
  callSign?: string | null;
}

export function NodeIdentityPanel({
  identity,
  onEditName,
  callSign,
}: NodeIdentityPanelProps) {
  if (!identity) {
    return (
      <Panel title="Node identity">
        <div className="key-value" aria-busy="true">
          <div className="skeleton skeleton--line" />
          <div className="skeleton skeleton--line" />
        </div>
      </Panel>
    );
  }

  return (
    <Panel
      title="Node identity"
      subtitle={identity.algorithm}
      actions={
        onEditName && (
          <button
            type="button"
            onClick={onEditName}
            className="button button--secondary button--compact"
            title="Edit node name / alias"
          >
            <svg
              width="13"
              height="13"
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              strokeWidth="2.2"
              strokeLinecap="round"
              strokeLinejoin="round"
              aria-hidden="true"
            >
              <path d="M17 3a2.85 2.83 0 1 1 4 4L7.5 20.5 2 22l1.5-5.5Z" />
            </svg>
            <span>Edit Name</span>
          </button>
        )
      }
    >
      <dl className="key-value">
        <div className="key-value__row">
          <dt className="key-value__key">Node name</dt>
          <dd className="key-value__value" style={{ display: "flex", alignItems: "center", gap: "8px" }}>
            <strong>{identity.nodeName}</strong>
          </dd>
        </div>
        {callSign && (
          <div className="key-value__row">
            <dt className="key-value__key">Call sign</dt>
            <dd className="key-value__value">
              <strong>{callSign}</strong>{" "}
              <span style={{ color: "var(--text-secondary)", fontWeight: 400 }}>
                (this device only)
              </span>
            </dd>
          </div>
        )}
        <div className="key-value__row">
          <dt className="key-value__key">Node ID</dt>
          <dd className="key-value__value">
            <EncryptedId id={identity.nodeId} lead={16} tail={8} type="node" showCopy />
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
