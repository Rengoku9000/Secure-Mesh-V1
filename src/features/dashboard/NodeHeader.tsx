import { ThemeSwitch } from "../../components/ThemeSwitch";
import type { NetworkStatus, PublicIdentity } from "../../types/core";

interface NodeHeaderProps {
  identity: PublicIdentity | null;
  network: NetworkStatus | null;
  onOpenAi?: () => void;
}

/**
 * ixigo-inspired Live Status Hero Header.
 *
 * Uses the signature coral-crimson gradient, live transit-style status dot,
 * and bold scannable typography.
 */
export function NodeHeader({ identity, network, onOpenAi }: NodeHeaderProps) {
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
            <span>NODE</span>
          </div>

          <div className="ixigo-header__title-group">
            <h1
              className="ixigo-header__node-id"
              title={identity ? `Node ID: ${identity.nodeId}` : undefined}
            >
              {identity?.nodeName ?? "SM-NODE"}
            </h1>
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
              <svg
                className="ixigo-header__ai-sparkle"
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
              <span className="ixigo-header__ai-text">Ask AI</span>
              <kbd className="ixigo-header__ai-kbd mono">Ctrl K</kbd>
            </button>
          )}
          <ThemeSwitch />
        </div>
      </div>
    </header>
  );
}
