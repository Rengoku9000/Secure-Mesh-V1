/**
 * Turning SecureMesh state into things the map can draw.
 *
 * Pure functions, no DOM. The rules that matter operationally live here, where
 * they can be tested directly:
 *
 * - an incident with no coordinates produces **no marker**, rather than one at
 *   a default position;
 * - an incident with no recorded accuracy produces **no accuracy circle**,
 *   rather than a circle of some assumed radius;
 * - severity comes from the existing `Severity` values and the existing theme
 *   tokens, so the map cannot disagree with the incident table about how
 *   serious something is.
 */

import type {
  Incident,
  IncidentLocationSource,
  LocationSource as DeviceLocationSource,
  Peer,
  Severity,
} from "../../types/core";
import { formatAccuracy } from "../../lib/format.ts";
import type { GeoPoint } from "./projection.ts";
import { metresPerPixel } from "./projection.ts";

/** An incident placed on the map. */
export interface IncidentMarker {
  incidentId: string;
  position: GeoPoint;
  severity: Severity;
  /** Recorded accuracy radius in metres, or `null` when none was recorded. */
  accuracyMeters: number | null;
  /** The incident itself, so a click can open the existing details view. */
  incident: Incident;
}

/** The device this node is running on, once the operator has taken a fix. */
export interface NodeMarker {
  nodeName: string;
  position: GeoPoint;
  accuracyMeters: number | null;
}

/**
 * A peer node placed on the map.
 *
 * No peer marker can be produced today, and that is deliberate rather than
 * unfinished: `Peer` carries identity, trust, reachability and replication
 * state, and nothing anywhere in SecureMesh records where a peer *is*.
 *
 * The tempting shortcut — placing a peer at the coordinates of an incident it
 * authored — is wrong. That is where the peer was when it filed a report, which
 * may be hours old and kilometres away, and drawing it as the peer's position
 * would state something SecureMesh does not know. The incident is shown at
 * those coordinates because the incident is what happened there.
 *
 * The type and the layer exist so that the day a peer position becomes
 * authoritative, only the source of this array changes.
 */
export interface PeerMarker {
  nodeId: string;
  nodeName: string;
  position: GeoPoint;
  /** When the position was recorded, so staleness is visible. */
  recordedAt: string;
}

/** Which theme token paints a severity. Never a second severity scale. */
export const SEVERITY_TOKEN: Record<Severity, string> = {
  LOW: "var(--sev-low-fg)",
  MEDIUM: "var(--sev-medium-fg)",
  HIGH: "var(--sev-high-fg)",
  CRITICAL: "var(--sev-critical-fg)",
};

/** Marker radius in pixels, so more serious incidents read first. */
export const SEVERITY_RADIUS: Record<Severity, number> = {
  LOW: 5,
  MEDIUM: 6,
  HIGH: 7,
  CRITICAL: 8,
};

/** Whether an incident has a position that can be drawn. */
export function hasPosition(incident: Incident): boolean {
  return (
    incident.latitude !== null &&
    incident.longitude !== null &&
    Number.isFinite(incident.latitude) &&
    Number.isFinite(incident.longitude)
  );
}

/**
 * Markers for every incident that has coordinates.
 *
 * Incidents without a position are dropped rather than placed somewhere: an
 * incident with no location is a real and ordinary thing in SecureMesh, and it
 * belongs in the table, not at a guessed point on a map.
 */
export function incidentMarkers(incidents: Incident[]): IncidentMarker[] {
  const markers: IncidentMarker[] = [];

  for (const incident of incidents) {
    if (!hasPosition(incident)) {
      continue;
    }
    markers.push({
      incidentId: incident.id,
      position: {
        latitude: incident.latitude as number,
        longitude: incident.longitude as number,
      },
      severity: incident.severity,
      accuracyMeters: incident.accuracyMeters,
      incident,
    });
  }

  return markers;
}

/**
 * Markers for peers whose position SecureMesh holds authoritatively.
 *
 * Always empty, because no such record exists. Written as a function over the
 * peer list rather than a constant so the call site is already correct, and so
 * the emptiness is asserted by a test rather than assumed.
 */
export function peerMarkers(_peers: Peer[]): PeerMarker[] {
  return [];
}

/**
 * Radius of the accuracy circle in screen pixels, or `null` for no circle.
 *
 * `null` when no accuracy was recorded — the circle means "the true position is
 * somewhere in here", and there is no honest radius for a reading that never
 * carried one. Drawing a default would invent precision.
 *
 * Sub-pixel circles are also suppressed: a fix better than the current zoom can
 * resolve would draw a ring inside the marker, which reads as decoration rather
 * than as uncertainty.
 */
export function accuracyRadiusPixels(
  accuracyMeters: number | null,
  latitude: number,
  zoom: number,
): number | null {
  if (accuracyMeters === null || !Number.isFinite(accuracyMeters) || accuracyMeters <= 0) {
    return null;
  }

  const radius = accuracyMeters / metresPerPixel(latitude, zoom);
  return radius >= 1 ? radius : null;
}

/**
 * A value that changes only when something the map draws has changed.
 *
 * The dashboard re-reads every two seconds and hands back a fresh array each
 * time, so array identity says nothing about whether the map needs rebuilding.
 * Keying the marker memo on this instead means panning stays smooth and a poll
 * that changed nothing costs nothing.
 *
 * Deliberately excludes fields the map does not draw — description, timestamps,
 * analysis — so editing them elsewhere cannot force a redraw.
 */
export function markerSignature(incidents: Incident[]): string {
  const parts: string[] = [];

  for (const incident of incidents) {
    if (!hasPosition(incident)) {
      continue;
    }
    parts.push(
      `${incident.id}:${incident.latitude}:${incident.longitude}:${incident.severity}:${incident.accuracyMeters}:${incident.syncStatus}`,
    );
  }

  return parts.join("|");
}

/**
 * How a recorded incident position is described to an operator.
 *
 * Reads the vocabulary the record stores. A Wi-Fi-derived position is never
 * called GPS, because it is not one — the distinction is the whole reason the
 * source is recorded.
 */
export const SOURCE_LABEL: Record<IncidentLocationSource, string> = {
  GNSS: "GNSS (satellite)",
  WIRELESS: "Wireless",
  UNKNOWN: "Unknown",
};

/** The same, for a live reading straight from the platform provider. */
export const DEVICE_SOURCE_LABEL: Record<DeviceLocationSource, string> = {
  SATELLITE: "GNSS (satellite)",
  WIRELESS: "Wireless",
  IP_ADDRESS: "IP address estimate",
  UNKNOWN: "Unknown",
};

/** What the incident card shows, already resolved to display strings. */
export interface IncidentFacts {
  coordinates: string;
  accuracy: string;
  source: string;
  captured: string;
}

/**
 * The facts an incident card states about a position.
 *
 * Every absent value reads **"Unknown"**, never a zero, a dash or a plausible
 * default. An operator deciding whether to act on a coordinate has to be able
 * to tell "measured to ±5 m" from "nobody recorded how good this is", and a
 * blank field does not carry that difference.
 *
 * Returns `null` for an incident with no coordinates: it has no marker, so
 * there is nothing to describe.
 */
export function incidentFacts(incident: Incident): IncidentFacts | null {
  if (!hasPosition(incident)) {
    return null;
  }

  const latitude = incident.latitude as number;
  const longitude = incident.longitude as number;

  return {
    coordinates: `${latitude.toFixed(6)}, ${longitude.toFixed(6)}`,
    accuracy:
      incident.accuracyMeters === null || !Number.isFinite(incident.accuracyMeters)
        ? "Unknown"
        : formatAccuracy(incident.accuracyMeters),
    source: SOURCE_LABEL[incident.locationSource] ?? "Unknown",
    captured: incident.locationCapturedAt === null ? "Unknown" : incident.locationCapturedAt,
  };
}
