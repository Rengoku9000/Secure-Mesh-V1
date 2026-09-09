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
  LocationFreshness,
  PeerLocationView,
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
 * Drawn only from a position the peer itself reported over the authenticated
 * mesh. Nothing is inferred from an IP address, a transport peer ID, mDNS, or
 * network topology — those say where a packet came from, not where a node is.
 *
 * An incident's coordinates are still never used to place its author. That is
 * where the peer was when it filed a report, possibly hours ago and kilometres
 * away; the incident is shown there because the incident is what happened
 * there.
 */
export interface PeerMarker {
  nodeId: string;
  nodeName: string;
  position: GeoPoint;
  accuracyMeters: number | null;
  source: IncidentLocationSource;
  freshness: LocationFreshness;
  /** When the peer measured it. */
  capturedAt: string;
  /** When this node received it. */
  receivedAt: string;
  ageSeconds: number;
  sequence: number;
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
 * Built from location heartbeats the peers signed and sent, joined to the peer
 * list only for a display name. A peer that has never reported a position gets
 * no marker — SecureMesh does not know where it is, and no amount of network
 * metadata would tell it.
 *
 * **Expired positions are excluded.** They are older than three heartbeat
 * intervals, so they say where a node *was*. They remain in the core's state
 * and are still reported in the peer list as a last known position; they are
 * simply not drawn as though they were current.
 */
export function peerMarkers(
  peers: Peer[],
  locations: PeerLocationView[],
): PeerMarker[] {
  const names = new Map(peers.map((peer) => [peer.nodeId, peer.nodeName]));
  const markers: PeerMarker[] = [];

  for (const location of locations) {
    if (location.freshness === "EXPIRED") {
      continue;
    }
    if (
      !Number.isFinite(location.latitude) ||
      !Number.isFinite(location.longitude)
    ) {
      continue;
    }

    markers.push({
      nodeId: location.nodeId,
      // Falls back to the node ID rather than inventing a name.
      nodeName: names.get(location.nodeId) ?? location.nodeId,
      position: {
        latitude: location.latitude,
        longitude: location.longitude,
      },
      accuracyMeters: location.accuracyMeters,
      source: location.locationSource,
      freshness: location.freshness,
      capturedAt: location.capturedAt,
      receivedAt: location.receivedAt,
      ageSeconds: location.ageSeconds,
      sequence: location.sequence,
    });
  }

  return markers;
}

/** How a peer's position reads in the legend and popup. */
export const FRESHNESS_LABEL: Record<LocationFreshness, string> = {
  CURRENT: "Current",
  STALE: "Stale",
  EXPIRED: "Expired",
};

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
