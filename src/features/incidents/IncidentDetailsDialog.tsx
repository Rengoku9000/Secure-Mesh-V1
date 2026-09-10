import { useState, useEffect } from "react";
import { SeverityBadge } from "../../components/SeverityBadge";
import { SyncStatusBadge } from "../../components/SyncStatusBadge";
import { EncryptedId } from "../../components/EncryptedId";
import { formatAccuracy, formatTimestamp } from "../../lib/format";
import type { Incident, IncidentLocationSource } from "../../types/core";
import {
  DISPUTE_LABELS,
  type Dispute,
  type DisputeReason,
} from "../../lib/localAnnotations";

/**
 * What each recorded provenance means, in words that guide a decision.
 *
 * The record keeps a coarser vocabulary than the device layer reports, so these
 * three are the whole set. `UNKNOWN` covers a coordinate typed by hand, one the
 * platform would not attribute, an IP-derived guess, and an incident written
 * before provenance was recorded — all cases where the honest answer is that
 * the position is not attested.
 */
const SOURCE_LABEL: Record<IncidentLocationSource, string> = {
  GNSS: "Satellite (GNSS)",
  WIRELESS: "Wi-Fi / cellular estimate",
  UNKNOWN: "Not attested",
};

/**
 * Whether a source could have been obtained with no network reachable.
 *
 * Only satellite positioning qualifies. Stated here in the same terms the core
 * uses, because describing a Wi-Fi estimate as offline capability would be a
 * claim this project cannot support.
 */
const SOURCE_NOTE: Record<IncidentLocationSource, string> = {
  GNSS: "Obtained from satellites; needs no network.",
  WIRELESS:
    "Derived from nearby networks. The lookup itself is not offline, and precision is typically tens to hundreds of metres.",
  UNKNOWN:
    "Provenance was not recorded. Treat the coordinates as unverified.",
};

interface IncidentDetailsDialogProps {
  incident: Incident;
  onClose: () => void;
  onLocateOnMap?: (incident: Incident) => void;
  /** This node's own dispute against the report, if one was raised. */
  dispute?: Dispute | null;
  onFlag?: (incidentId: string, reason: DisputeReason, note?: string) => void;
  onClearFlag?: (incidentId: string) => void;
}

/**
 * The full record for one incident, including everything known about where its
 * coordinates came from.
 *
 * This view exists because the table cannot show provenance without becoming
 * unreadable, and provenance is the difference between a coordinate someone can
 * act on and one they cannot.
 */
export function IncidentDetailsDialog({
  incident,
  onClose,
  onLocateOnMap,
  dispute,
  onFlag,
  onClearFlag,
}: IncidentDetailsDialogProps) {
  const [flagOpen, setFlagOpen] = useState(false);
  const [flagReason, setFlagReason] = useState<DisputeReason>("INCORRECT");
  const [flagNote, setFlagNote] = useState("");
  const [showPlainIds, setShowPlainIds] = useState(false);

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        onClose();
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [onClose]);

  const located = incident.latitude !== null && incident.longitude !== null;

  return (
    <div
      className="dialog-backdrop"
      role="presentation"
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) {
          onClose();
        }
      }}
    >
      <div
        className="dialog"
        role="dialog"
        aria-modal="true"
        aria-labelledby="incident-details-title"
      >
        <header className="dialog__header">
          <h2 className="dialog__title" id="incident-details-title">
            Incident detail
          </h2>
          <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
            <button
              type="button"
              className={`dialog__visibility-pill ${
                showPlainIds
                  ? "dialog__visibility-pill--visible"
                  : "dialog__visibility-pill--encrypted"
              }`}
              onClick={() => setShowPlainIds(!showPlainIds)}
              title={
                showPlainIds
                  ? "Mask IDs for security (Encrypt)"
                  : "Reveal plain Incident & Node IDs (Decrypt)"
              }
              aria-label={showPlainIds ? "Mask IDs" : "Make IDs visible"}
            >
              {showPlainIds ? (
                <>
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
                    <path d="M1 12s4-8 11-8 11 8 11 8-4 8-11 8-11-8-11-8z" />
                    <circle cx="12" cy="12" r="3" />
                  </svg>
                  <span>IDs Visible</span>
                </>
              ) : (
                <>
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
                    <rect x="3" y="11" width="18" height="11" rx="2" ry="2" />
                    <path d="M7 11V7a5 5 0 0 1 10 0v4" />
                  </svg>
                  <span>Decrypt IDs</span>
                </>
              )}
            </button>

            <button
              type="button"
              className="dialog__close-btn"
              onClick={onClose}
              aria-label="Close dialog"
              title="Close (Esc)"
            >
              <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                <line x1="18" y1="6" x2="6" y2="18" />
                <line x1="6" y1="6" x2="18" y2="18" />
              </svg>
            </button>
          </div>
        </header>

        <div className="dialog__body">
          <dl className="detail-list">
            <div>
              <dt>Incident ID</dt>
              <dd className="mono">
                <EncryptedId
                  id={incident.id}
                  full
                  showCopy
                  visible={showPlainIds ? true : undefined}
                />
              </dd>
            </div>
            <div>
              <dt>Severity</dt>
              <dd>
                <SeverityBadge severity={incident.severity} />
              </dd>
            </div>
            <div>
              <dt>Description</dt>
              <dd>{incident.description}</dd>
            </div>
            <div>
              <dt>Recorded by</dt>
              <dd className="mono">
                <EncryptedId
                  id={incident.createdBy}
                  type="node"
                  full
                  showCopy
                  visible={showPlainIds ? true : undefined}
                />
              </dd>
            </div>
            <div>
              <dt>Recorded at</dt>
              <dd>{formatTimestamp(incident.createdAt)}</dd>
            </div>
            <div>
              <dt>Sync</dt>
              <dd>
                <SyncStatusBadge status={incident.syncStatus} />
              </dd>
            </div>
          </dl>

          <div className="location-capture">
            <div className="location-capture__header">
              <span className="field__label">Location</span>
              {located && (
                <span className="location-capture__badge">
                  {SOURCE_LABEL[incident.locationSource]}
                </span>
              )}
            </div>

            {!located ? (
              <div className="location-capture__state">
                <span className="location-capture__dot" />
                <span className="location-capture__detail">
                  No location was recorded for this incident.
                </span>
              </div>
            ) : (
              <>
                <dl className="location-capture__fix">
                  <div>
                    <dt>Latitude</dt>
                    <dd className="mono">{incident.latitude?.toFixed(6)}</dd>
                  </div>
                  <div>
                    <dt>Longitude</dt>
                    <dd className="mono">{incident.longitude?.toFixed(6)}</dd>
                  </div>
                  <div>
                    <dt>Accuracy</dt>
                    <dd className="mono">
                      {formatAccuracy(incident.accuracyMeters)}
                    </dd>
                  </div>
                  <div>
                    <dt>Source</dt>
                    <dd>{SOURCE_LABEL[incident.locationSource]}</dd>
                  </div>
                  <div>
                    <dt>Captured</dt>
                    <dd>
                      {incident.locationCapturedAt === null
                        ? "not recorded"
                        : formatTimestamp(incident.locationCapturedAt)}
                    </dd>
                  </div>
                </dl>
                <span className="field__hint">
                  {SOURCE_NOTE[incident.locationSource]}
                </span>

                {onLocateOnMap && (
                  <div style={{ marginTop: "14px", display: "flex", justifyContent: "flex-end" }}>
                    <button
                      type="button"
                      className="button button--primary button--compact"
                      onClick={() => {
                        onClose();
                        onLocateOnMap(incident);
                      }}
                      title="Locate this incident on the live map"
                    >
                      <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                        <polygon points="3 6 9 3 15 6 21 3 21 18 15 21 9 18 3 21" />
                      </svg>
                      <span>Locate on Live Map →</span>
                    </button>
                  </div>
                )}
              </>
            )}
          </div>

          {onFlag && (
            <div
              style={{
                marginTop: "20px",
                paddingTop: "16px",
                borderTop: "1px solid var(--border-subtle)",
              }}
            >
              {dispute ? (
                <div
                  style={{
                    padding: "10px 14px",
                    background: "var(--warning-bg, var(--danger-bg))",
                    border: "1px solid var(--warning-border, var(--danger-border))",
                    borderRadius: "var(--radius-sm)",
                  }}
                >
                  <div
                    style={{
                      display: "flex",
                      alignItems: "center",
                      justifyContent: "space-between",
                      gap: "12px",
                    }}
                  >
                    <span
                      style={{
                        fontSize: "12px",
                        fontWeight: 600,
                        color: "var(--warning-fg, var(--danger-fg))",
                      }}
                    >
                      Flagged on this node: {DISPUTE_LABELS[dispute.reason]}
                    </span>
                    {onClearFlag && (
                      <button
                        type="button"
                        className="button button--secondary button--compact"
                        onClick={() => onClearFlag(incident.id)}
                      >
                        Remove flag
                      </button>
                    )}
                  </div>
                  {dispute.note && (
                    <p
                      style={{
                        fontSize: "12px",
                        marginTop: "6px",
                        color: "var(--text-secondary)",
                      }}
                    >
                      {dispute.note}
                    </p>
                  )}
                  <p
                    style={{
                      fontSize: "11px",
                      marginTop: "6px",
                      color: "var(--text-tertiary, var(--text-secondary))",
                    }}
                  >
                    Local assessment only. The report itself is unaltered and is
                    still held and shared as recorded.
                  </p>
                </div>
              ) : !flagOpen ? (
                <div style={{ display: "flex", justifyContent: "flex-end" }}>
                  <button
                    type="button"
                    className="button button--danger-ghost button--compact"
                    onClick={() => setFlagOpen(true)}
                    title="Mark this report as inaccurate on this node"
                  >
                    <svg
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
                      <path d="M4 15s1-1 4-1 5 2 8 2 4-1 4-1V3s-1 1-4 1-5-2-8-2-4 1-4 1z" />
                      <line x1="4" y1="22" x2="4" y2="15" />
                    </svg>
                    <span>Flag as inaccurate</span>
                  </button>
                </div>
              ) : (
                <div>
                  <div className="field">
                    <label htmlFor="flag-reason" className="field__label">
                      Reason
                    </label>
                    <select
                      id="flag-reason"
                      className="input"
                      value={flagReason}
                      onChange={(e) =>
                        setFlagReason(e.target.value as DisputeReason)
                      }
                    >
                      {(
                        Object.keys(DISPUTE_LABELS) as DisputeReason[]
                      ).map((reason) => (
                        <option key={reason} value={reason}>
                          {DISPUTE_LABELS[reason]}
                        </option>
                      ))}
                    </select>
                  </div>

                  <div className="field">
                    <label htmlFor="flag-note" className="field__label">
                      Note <span style={{ fontWeight: 400 }}>(optional)</span>
                    </label>
                    <textarea
                      id="flag-note"
                      className="input"
                      rows={2}
                      value={flagNote}
                      maxLength={280}
                      placeholder="What is wrong with this report?"
                      onChange={(e) => setFlagNote(e.target.value)}
                    />
                  </div>

                  <p
                    style={{
                      fontSize: "11px",
                      color: "var(--text-secondary)",
                      lineHeight: 1.4,
                      marginBottom: "12px",
                    }}
                  >
                    The report is kept and continues to sync as recorded. This
                    flag is your node's own assessment and stays on this device.
                  </p>

                  <div
                    style={{
                      display: "flex",
                      justifyContent: "flex-end",
                      gap: "8px",
                    }}
                  >
                    <button
                      type="button"
                      className="button button--secondary button--compact"
                      onClick={() => {
                        setFlagOpen(false);
                        setFlagNote("");
                      }}
                    >
                      Cancel
                    </button>
                    <button
                      type="button"
                      className="button button--primary button--compact"
                      onClick={() => {
                        onFlag(incident.id, flagReason, flagNote);
                        setFlagOpen(false);
                        setFlagNote("");
                      }}
                    >
                      Save flag
                    </button>
                  </div>
                </div>
              )}
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
