import { useEffect, useRef } from "react";
import { IndexStateBadge } from "../../components/IndexStateBadge";
import { SeverityBadge } from "../../components/SeverityBadge";
import { SyncStatusBadge } from "../../components/SyncStatusBadge";
import { formatAccuracy, formatLocation, formatRelative, shortenId } from "../../lib/format";
import type { Incident, IndexState } from "../../types/core";

interface IncidentCardProps {
  incident: Incident;
  indexState?: IndexState;
  isSelected?: boolean;
  onSelect: (incident: Incident) => void;
  onLocateOnMap?: (incident: Incident) => void;
}

/**
 * Transit/PNR-inspired incident card modeled after ixigo's booking & status cards.
 * High contrast, clean route/coordinate pills, distinct status tags, and live map redirect.
 */
export function IncidentCard({
  incident,
  indexState,
  isSelected = false,
  onSelect,
  onLocateOnMap,
}: IncidentCardProps) {
  const hasCoords = incident.latitude !== null && incident.longitude !== null;
  const cardRef = useRef<HTMLElement>(null);

  // Smoothly scroll into view if highlighted/selected from map or elsewhere
  useEffect(() => {
    if (isSelected && cardRef.current) {
      cardRef.current.scrollIntoView({ behavior: "smooth", block: "center" });
    }
  }, [isSelected]);

  return (
    <article
      ref={cardRef}
      className={`ixigo-transit-card${isSelected ? " ixigo-transit-card--selected" : ""}`}
      tabIndex={0}
      role="button"
      onClick={() => onSelect(incident)}
      onKeyDown={(e) => {
        if (e.key === "Enter" || e.key === " ") {
          e.preventDefault();
          onSelect(incident);
        }
      }}
      aria-label={`Incident ${shortenId(incident.id, 8, 4)}: ${incident.description}`}
    >
      {/* Top Status Strip */}
      <div className="ixigo-transit-card__top">
        <div className="ixigo-transit-card__id-badge">
          <span className="ixigo-transit-card__hash">#</span>
          <span>{shortenId(incident.id, 6, 4)}</span>
        </div>

        <SeverityBadge severity={incident.severity} />

        {isSelected && (
          <span className="ixigo-transit-card__focus-badge">
            <span className="ixigo-transit-card__focus-dot" />
            Selected on Map
          </span>
        )}

        <div className="ixigo-transit-card__sync-wrapper">
          <SyncStatusBadge status={incident.syncStatus} />
          <time className="ixigo-transit-card__time" dateTime={incident.createdAt}>
            {formatRelative(incident.createdAt)}
          </time>
        </div>
      </div>

      {/* Main Headline */}
      <h3 className="ixigo-transit-card__title">{incident.description}</h3>

      {/* Transit / Coordinate Route Strip */}
      <div className="ixigo-transit-card__waypoint-strip">
        <div className="ixigo-transit-card__pin-icon" aria-hidden="true">
          <svg
            width="14"
            height="14"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            strokeWidth="2.5"
            strokeLinecap="round"
            strokeLinejoin="round"
          >
            <path d="M21 10c0 7-9 13-9 13s-9-6-9-13a9 9 0 0 1 18 0z" />
            <circle cx="12" cy="10" r="3" />
          </svg>
        </div>

        {hasCoords ? (
          <div className="ixigo-transit-card__location-info">
            <span className="ixigo-transit-card__coords">
              {formatLocation(incident.latitude, incident.longitude)}
            </span>
            {incident.accuracyMeters !== null && (
              <span className="ixigo-transit-card__accuracy">
                {formatAccuracy(incident.accuracyMeters)}
              </span>
            )}
          </div>
        ) : (
          <span className="ixigo-transit-card__no-location">
            Position not pinned (offline entry)
          </span>
        )}

        {indexState && (
          <div className="ixigo-transit-card__ai-tag">
            <IndexStateBadge state={indexState} />
          </div>
        )}
      </div>

      {/* Footer CTA & Direct Live Map Button */}
      <div className="ixigo-transit-card__footer">
        <div style={{ display: "flex", alignItems: "center", gap: "6px" }}>
          <span className="ixigo-transit-card__cta-text">View Full Details</span>
          <span className="ixigo-transit-card__cta-arrow" aria-hidden="true">
            →
          </span>
        </div>

        {hasCoords && onLocateOnMap && (
          <button
            type="button"
            className="ixigo-transit-card__map-btn"
            title="Redirect and locate this incident on the Live Tactical Map"
            onClick={(e) => {
              e.stopPropagation();
              onLocateOnMap(incident);
            }}
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
              <polygon points="3 6 9 3 15 6 21 3 21 18 15 21 9 18 3 21" />
            </svg>
            <span>Live Map</span>
          </button>
        )}
      </div>
    </article>
  );
}
