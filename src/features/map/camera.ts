/**
 * Where the map is looking.
 *
 * # Camera is not data
 *
 * Everything here computes a centre and a zoom. Nothing in this module reads or
 * returns an incident, a selection or a position record, which is the point:
 * moving the camera can never change what exists. Markers are always built from
 * the full incident list, and the viewport decides what is *visible*, never what
 * is *rendered*.
 *
 * That separation is why "My location" cannot lose an incident. It could only
 * ever put one off screen.
 */

import {
  clampZoom,
  MAX_ZOOM,
  TILE_SIZE,
  project,
  toScreen,
  type GeoPoint,
  type Viewport,
} from "./projection.ts";

/**
 * Zoom "My location" settles on when nothing else is nearby.
 *
 * About a quarter of a kilometre across on a typical panel — close enough to
 * read a street, wide enough to keep the surroundings.
 */
export const SELF_ZOOM = 16;

/**
 * How far an incident can be and still be treated as worth keeping in frame.
 *
 * A judgement, not a fact: within this, an operator almost certainly wants to
 * see the incident and their own position together. Beyond it, framing both
 * would zoom out so far that neither is legible, and "take me to where I am"
 * would stop meaning anything.
 */
export const NEARBY_KM = 25;

/** Padding kept around the outermost point when framing, in pixels. */
const PADDING = 56;

const EARTH_RADIUS_KM = 6371.0088;

/** Great-circle distance between two positions, in kilometres. */
export function distanceKm(a: GeoPoint, b: GeoPoint): number {
  const toRadians = (degrees: number) => (degrees * Math.PI) / 180;

  const dLat = toRadians(b.latitude - a.latitude);
  const dLon = toRadians(b.longitude - a.longitude);
  const lat1 = toRadians(a.latitude);
  const lat2 = toRadians(b.latitude);

  const h =
    Math.sin(dLat / 2) ** 2 +
    Math.cos(lat1) * Math.cos(lat2) * Math.sin(dLon / 2) ** 2;

  return 2 * EARTH_RADIUS_KM * Math.asin(Math.min(1, Math.sqrt(h)));
}

/**
 * The closest zoom at which every point is still on screen, **with the camera
 * held at `centre`**.
 *
 * Different from `fitBounds`, which is free to move the centre. Here the centre
 * is fixed — the operator asked to be taken to their own position, not to the
 * middle of everything — so the visible span has to be twice the distance from
 * the centre to the furthest point.
 *
 * Returns [`MAX_ZOOM`] when there is nothing to include, so a caller can apply
 * its own default rather than being handed an arbitrary one.
 */
export function zoomToInclude(
  centre: GeoPoint,
  points: GeoPoint[],
  width: number,
  height: number,
  padding: number = PADDING,
): number {
  if (points.length === 0) {
    return MAX_ZOOM;
  }

  const origin = project(centre);
  let maxDx = 0;
  let maxDy = 0;

  for (const point of points) {
    const projected = project(point);
    maxDx = Math.max(maxDx, Math.abs(projected.x - origin.x));
    maxDy = Math.max(maxDy, Math.abs(projected.y - origin.y));
  }

  const usableWidth = Math.max(1, width - padding * 2);
  const usableHeight = Math.max(1, height - padding * 2);

  // Every point coincides with the centre; no zoom is implied by the geometry.
  if (maxDx <= Number.EPSILON && maxDy <= Number.EPSILON) {
    return MAX_ZOOM;
  }

  const zoomX =
    maxDx > 0 ? Math.log2(usableWidth / (2 * maxDx * TILE_SIZE)) : MAX_ZOOM;
  const zoomY =
    maxDy > 0 ? Math.log2(usableHeight / (2 * maxDy * TILE_SIZE)) : MAX_ZOOM;

  return clampZoom(Math.min(zoomX, zoomY));
}

/**
 * The camera for "My location".
 *
 * Always centres on the operator — that is what the button says it does. The
 * zoom is the only judgement: close in when there is nothing else to see, wide
 * enough to keep company when there is.
 *
 * Never zooms *in* past {@link SELF_ZOOM}. The bug this replaces forced a fixed
 * close zoom regardless of what else was on the map, which pushed every other
 * incident out of frame and made it look as though they had been dropped.
 *
 * Incidents beyond {@link NEARBY_KM} are deliberately not framed. They are
 * still rendered, still in the list, and still reachable through "Fit
 * incidents" or "Fit all" — the caller reports how many are off screen so the
 * operator is told rather than left to wonder.
 */
export function frameOnSelf(
  here: GeoPoint,
  incidents: GeoPoint[],
  width: number,
  height: number,
  options: { selfZoom?: number; nearbyKm?: number } = {},
): { centre: GeoPoint; zoom: number } {
  const selfZoom = options.selfZoom ?? SELF_ZOOM;
  const nearbyKm = options.nearbyKm ?? NEARBY_KM;

  const nearby = incidents.filter(
    (incident) => distanceKm(here, incident) <= nearbyKm,
  );

  if (nearby.length === 0) {
    return { centre: here, zoom: clampZoom(selfZoom) };
  }

  return {
    centre: here,
    zoom: Math.min(clampZoom(selfZoom), zoomToInclude(here, nearby, width, height)),
  };
}

/**
 * How many of these points fall outside the viewport.
 *
 * Used only to tell the operator that something exists off screen. It counts;
 * it never filters. Nothing anywhere uses this to decide what to draw.
 */
export function offscreenCount(points: GeoPoint[], viewport: Viewport): number {
  let outside = 0;

  for (const point of points) {
    const screen = toScreen(point, viewport);
    if (
      screen.x < 0 ||
      screen.y < 0 ||
      screen.x > viewport.width ||
      screen.y > viewport.height
    ) {
      outside += 1;
    }
  }

  return outside;
}
