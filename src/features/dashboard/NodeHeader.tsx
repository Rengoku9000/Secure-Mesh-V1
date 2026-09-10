import { ThemeSwitch } from "../../components/ThemeSwitch";
import { QwenIcon } from "../../components/QwenIcon";
import type { NetworkStatus, PublicIdentity } from "../../types/core";

interface NodeHeaderProps {
  identity: PublicIdentity | null;
  network: NetworkStatus | null;
  onOpenAi?: () => void;
  onEditName?: () => void;
  /** Operator's local call sign, shown in place of the registered name. */
  callSign?: string | null;
}

/**
 * ixigo-inspired Live Status Hero Header.
 *
 * Uses the signature coral-crimson gradient, live transit-style status dot,
 * and bold scannable typography.
 */
export function NodeHeader({
  identity,
  network,
  onOpenAi,
  onEditName,
  callSign,
}: NodeHeaderProps) {
  const online = network?.online ?? false;
  const pendingSync = network?.pendingSync ?? 0;
  const connectedPeers = network?.connectedPeers ?? 0;

  return (
    <header className="ixigo-header">
      <div className="ixigo-header__inner">
        <div className="ixigo-header__left">
          <div className="ixigo-header__badge">
            <svg
              width="16"
              height="16"
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              strokeWidth="2.5"
              strokeLinecap="round"
              strokeLinejoin="round"
              aria-hidden="true"
            >
              <polygon points="13 2 3 14 12 14 11 22 21 10 12 10 13 2" />
            </svg>
            <span>MESH</span>
          </div>

          <div className="ixigo-header__title-group">
            <div style={{ display: "flex", alignItems: "center", gap: "6px" }}>
              <h1
                className="ixigo-header__node-id"
                title={
                  identity
                    ? `${identity.nodeName} · Node ID: ${identity.nodeId}`
                    : undefined
                }
              >
                {callSign ?? identity?.nodeName ?? "SM-NODE"}
              </h1>
              {onEditName && (
                <button
                  type="button"
                  onClick={onEditName}
                  className="button--ghost"
                  style={{
                    padding: "2px 5px",
                    minHeight: "unset",
                    height: "22px",
                    borderRadius: "4px",
                    display: "inline-flex",
                    alignItems: "center",
                    justifyContent: "center",
                    color: "var(--text-muted)",
                    cursor: "pointer",
                  }}
                  title="Edit node name / alias"
                  aria-label="Edit node name"
                >
                  <svg
                    width="12"
                    height="12"
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
                </button>
              )}
            </div>
            <div className="ixigo-header__substatus">
              <span className={`ixigo-live-dot ${online ? "ixigo-live-dot--active" : ""}`} />
              <span className="ixigo-live-label">
                {online ? "LIVE MESH" : "STANDALONE"}
              </span>
              <span className="ixigo-substatus-sep">•</span>
              <span>{connectedPeers} peer{connectedPeers === 1 ? "" : "s"}</span>
              {pendingSync > 0 && (
                <>
                  <span className="ixigo-substatus-sep">•</span>
                  <span className="ixigo-sync-alert">{pendingSync} sync</span>
                </>
              )}
            </div>
          </div>
        </div>

        <div className="ixigo-header__right">
          {onOpenAi && (
            <button
              type="button"
              className="ixigo-header__ai-pill"
              onClick={onOpenAi}
              title="Ask SecureMesh AI (Ctrl+K)"
              aria-label="Ask SecureMesh AI (Ctrl+K)"
            >
              <QwenIcon size={14} className="ixigo-header__ai-sparkle" />
              <span className="ixigo-header__ai-text">Ask AI</span>
            </button>
          )}
          <ThemeSwitch />
        </div>
      </div>
    </header>
  );
}
