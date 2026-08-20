import { useEffect } from "react";
import { SeverityBadge } from "../../components/SeverityBadge";
import { SyncStatusBadge } from "../../components/SyncStatusBadge";
import { formatAccuracy, formatTimestamp } from "../../lib/format";
import type { Incident, IncidentLocationSource } from "../../types/core";

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
}: IncidentDetailsDialogProps) {
  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        onClose();
      }
    };
    document.addEventListener("keydown", onKeyDown);
    return () => document.removeEventListener("keydown", onKeyDown);
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
          <button type="button" className="button button--ghost" onClick={onClose}>
            Close
          </button>
        </header>

        <div className="dialog__body">
          <dl className="detail-list">
            <div>
              <dt>Incident ID</dt>
              <dd className="mono">{incident.id}</dd>
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
              <dd className="mono">{incident.createdBy}</dd>
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
              </>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}
