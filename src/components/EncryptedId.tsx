import { useState, type MouseEvent } from "react";
import { useIdPrivacy, encryptId } from "../lib/idPrivacy";
import { shortenId } from "../lib/format";

interface EncryptedIdProps {
  id: string;
  lead?: number;
  tail?: number;
  full?: boolean;
  type?: "incident" | "node";
  className?: string;
  showCopy?: boolean;
  showToggle?: boolean;
  visible?: boolean;
  onToggle?: () => void;
  style?: React.CSSProperties;
}

export function EncryptedId({
  id,
  lead = 6,
  tail = 4,
  full = false,
  type = "incident",
  className = "",
  showCopy = false,
  showToggle = true,
  visible,
  onToggle,
  style,
}: EncryptedIdProps) {
  const { isEncrypted, isIdRevealed, toggleRevealId } = useIdPrivacy();
  const [copied, setCopied] = useState(false);

  // Explicit prop overrides, otherwise individual reveal or global setting
  const isIndividualRevealed = isIdRevealed(id);
  const isVisible = visible !== undefined ? visible : (!isEncrypted || isIndividualRevealed);

  const displayText = isVisible
    ? full
      ? id
      : shortenId(id, lead, tail)
    : encryptId(id, lead, tail, full);

  const handleToggle = (e: MouseEvent) => {
    e.stopPropagation();
    if (onToggle) {
      onToggle();
    } else {
      toggleRevealId(id);
    }
  };

  const handleCopy = async (e: MouseEvent) => {
    e.stopPropagation();
    try {
      await navigator.clipboard.writeText(id);
      setCopied(true);
      setTimeout(() => setCopied(false), 1600);
    } catch {
      // Clipboard write error
    }
  };

  return (
    <span
      className={`encrypted-id ${
        isVisible ? "encrypted-id--visible" : "encrypted-id--encrypted"
      } ${className}`.trim()}
      style={style}
      title={
        isVisible
          ? `${type === "node" ? "Node ID" : "Incident ID"}: ${id}`
          : "ID is hidden (encrypted) · Click eye to show"
      }
    >
      <span className="encrypted-id__badge">
        {!isVisible ? (
          <svg
            className="encrypted-id__icon"
            width="11"
            height="11"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            strokeWidth="2.2"
            strokeLinecap="round"
            strokeLinejoin="round"
            aria-hidden="true"
          >
            <rect x="3" y="11" width="18" height="11" rx="2" ry="2" />
            <path d="M7 11V7a5 5 0 0 1 10 0v4" />
          </svg>
        ) : (
          <span className="encrypted-id__unlocked-dot" aria-hidden="true" />
        )}
        <span className="encrypted-id__text mono">{displayText}</span>
      </span>

      {showToggle && (
        <button
          type="button"
          className="encrypted-id__btn"
          onClick={handleToggle}
          title={isVisible ? "Hide ID (Encrypt)" : "Show ID (Decrypt)"}
          aria-label={isVisible ? "Hide ID" : "Show ID"}
        >
          {isVisible ? (
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
              <path d="M17.94 17.94A10.07 10.07 0 0 1 12 20c-7 0-11-8-11-8a18.45 18.45 0 0 1 5.06-5.94M9.9 4.24A9.12 9.12 0 0 1 12 4c7 0 11 8 11 8a18.5 18.5 0 0 1-2.16 3.19m-6.72-1.07a3 3 0 1 1-4.24-4.24" />
              <line x1="1" y1="1" x2="23" y2="23" />
            </svg>
          ) : (
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
              <path d="M1 12s4-8 11-8 11 8 11 8-4 8-11 8-11-8-11-8z" />
              <circle cx="12" cy="12" r="3" />
            </svg>
          )}
        </button>
      )}

      {showCopy && (
        <button
          type="button"
          className="encrypted-id__btn"
          onClick={handleCopy}
          title={copied ? "Copied to clipboard!" : "Copy full real ID"}
          aria-label={copied ? "Copied" : "Copy ID"}
        >
          {copied ? (
            <svg
              width="12"
              height="12"
              viewBox="0 0 24 24"
              fill="none"
              stroke="var(--state-ok)"
              strokeWidth="2.5"
              strokeLinecap="round"
              strokeLinejoin="round"
              aria-hidden="true"
            >
              <polyline points="20 6 9 17 4 12" />
            </svg>
          ) : (
            <svg
              width="12"
              height="12"
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              strokeWidth="2"
              strokeLinecap="round"
              strokeLinejoin="round"
              aria-hidden="true"
            >
              <rect x="9" y="9" width="13" height="13" rx="2" ry="2" />
              <path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1" />
            </svg>
          )}
        </button>
      )}
    </span>
  );
}
