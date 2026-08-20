/**
 * Web Mercator projection, and the arithmetic the tactical map needs.
 *
 * Free functions with no DOM and no React, so the map's geometry can be tested
 * directly rather than inferred from rendered output. Everything here is pure:
 * the same inputs always give the same numbers.
 *
 * # World coordinates
 *
 * A position projects to a `[0, 1]` square — 0 at the antimeridian and the top
 * of the projection, 1 at the other side. Keeping world space normalized and
 * independent of zoom is what lets the renderer project the basemap **once**
 * and then pan and zoom with a single SVG transform, instead of recomputing
 * every path on every frame.
 *
 * # Why Web Mercator
 *
 * Not because it is a good projection — it badly distorts area near the poles —
 * but because it is the projection every other map an operator has ever used
 * employs, and because it is conformal: a circle of constant ground radius
 * stays a circle on screen. That is what makes an honest accuracy circle
 * possible.
 */

/** A geographic position, in WGS 84 decimal degrees. */
export interface GeoPoint {
  latitude: number;
  longitude: number;
}

/** A position in normalized world space, both axes in `[0, 1]`. */
export interface WorldPoint {
  x: number;
  y: number;
}

/**
 * How far the map can be zoomed.
 *
 * The lower bound shows the whole world; the upper is about the point where a
 * screen pixel covers well under a metre, which is finer than any source
 * SecureMesh records can justify.
 */
export const MIN_ZOOM = 1;
export const MAX_ZOOM = 19;

/**
 * Latitude beyond which Web Mercator stretches without bound.
 *
 * The standard cutoff, which makes the projected world square. A position
 * further north or south than this is clamped rather than sent to infinity.
 */
const MAX_MERCATOR_LATITUDE = 85.05112877980659;

/** Earth's circumference at the equator, in metres. */
const EQUATORIAL_CIRCUMFERENCE = 40_075_016.686;

/** Side length of the world square at zoom 0, in pixels. */
export const TILE_SIZE = 256;

function clamp(value: number, low: number, high: number): number {
  return Math.min(high, Math.max(low, value));
}

/** Constrains a zoom level to the renderable range. */
export function clampZoom(zoom: number): number {
  return clamp(zoom, MIN_ZOOM, MAX_ZOOM);
}

/**
 * Projects a geographic position into normalized world space.
 *
 * Latitude is clamped to the Mercator limit rather than allowed to diverge: a
 * marker at the pole has to land somewhere finite, and the top edge of the map
 * is the honest place for it.
 */
export function project({ latitude, longitude }: GeoPoint): WorldPoint {
  const lat = clamp(latitude, -MAX_MERCATOR_LATITUDE, MAX_MERCATOR_LATITUDE);
  const radians = (lat * Math.PI) / 180;

  return {
    x: (longitude + 180) / 360,
    y: 0.5 - Math.log(Math.tan(Math.PI / 4 + radians / 2)) / (2 * Math.PI),
  };
}

/** Inverts {@link project}. Used to report what the operator is looking at. */
export function unproject({ x, y }: WorldPoint): GeoPoint {
  const radians = 2 * Math.atan(Math.exp((0.5 - y) * 2 * Math.PI)) - Math.PI / 2;

  return {
    latitude: (radians * 180) / Math.PI,
    longitude: x * 360 - 180,
  };
}

/**
 * Ground distance covered by one screen pixel, in metres.
 *
 * Latitude-dependent, because Mercator stretches horizontally away from the
 * equator: the same pixel spans far less ground in Iceland than in Kenya.
 * Getting this wrong would draw accuracy circles that overstate or understate
 * a fix by a factor of two or more at temperate latitudes.
 */
export function metresPerPixel(latitude: number, zoom: number): number {
  const lat = clamp(latitude, -MAX_MERCATOR_LATITUDE, MAX_MERCATOR_LATITUDE);
  const radians = (lat * Math.PI) / 180;

  return (
    (EQUATORIAL_CIRCUMFERENCE * Math.cos(radians)) / (TILE_SIZE * Math.pow(2, zoom))
  );
}

/** The side length of the whole world, in pixels, at a zoom level. */
export function worldSize(zoom: number): number {
  return TILE_SIZE * Math.pow(2, zoom);
}

/** What the map is currently looking at. */
export interface Viewport {
  centre: GeoPoint;
  zoom: number;
  /** Size of the drawing surface, in CSS pixels. */
  width: number;
  height: number;
}

/** Converts a geographic position to a pixel offset within the viewport. */
export function toScreen(point: GeoPoint, viewport: Viewport): WorldPoint {
  const size = worldSize(viewport.zoom);
  const world = project(point);
  const centre = project(viewport.centre);

  return {
    x: (world.x - centre.x) * size + viewport.width / 2,
    y: (world.y - centre.y) * size + viewport.height / 2,
  };
}

/** Converts a pixel offset within the viewport back to a position. */
export function fromScreen(pixel: WorldPoint, viewport: Viewport): GeoPoint {
  const size = worldSize(viewport.zoom);
  const centre = project(viewport.centre);

  return unproject({
    x: (pixel.x - viewport.width / 2) / size + centre.x,
    y: (pixel.y - viewport.height / 2) / size + centre.y,
  });
}

/**
 * The centre and zoom that bring every supplied point into view.
 *
 * Returns `null` for an empty set rather than a default position: there is no
 * honest place to put a map of nothing, and inventing one would show an
 * operator a location no data supports.
 *
 * A single point cannot imply a zoom — a set of one has no extent — so
 * `singleZoom` is used for that case instead of zooming to infinity.
 */
export function fitBounds(
  points: GeoPoint[],
  width: number,
  height: number,
  options: { padding?: number; singleZoom?: number } = {},
): { centre: GeoPoint; zoom: number } | null {
  if (points.length === 0) {
    return null;
  }

  const padding = options.padding ?? 48;
  const singleZoom = options.singleZoom ?? 15;

  const projected = points.map(project);
  const minX = Math.min(...projected.map((p) => p.x));
  const maxX = Math.max(...projected.map((p) => p.x));
  const minY = Math.min(...projected.map((p) => p.y));
  const maxY = Math.max(...projected.map((p) => p.y));

  const centre = unproject({ x: (minX + maxX) / 2, y: (minY + maxY) / 2 });

  const spanX = maxX - minX;
  const spanY = maxY - minY;

  // Every point in the same place, to floating-point precision.
  if (spanX <= Number.EPSILON && spanY <= Number.EPSILON) {
    return { centre, zoom: clampZoom(singleZoom) };
  }

  const usableWidth = Math.max(1, width - padding * 2);
  const usableHeight = Math.max(1, height - padding * 2);

  // The tighter of the two axes decides: fitting the looser one would push the
  // other off screen.
  const zoomX = spanX > 0 ? Math.log2(usableWidth / (spanX * TILE_SIZE)) : MAX_ZOOM;
  const zoomY = spanY > 0 ? Math.log2(usableHeight / (spanY * TILE_SIZE)) : MAX_ZOOM;

  return { centre, zoom: clampZoom(Math.min(zoomX, zoomY)) };
}

/**
 * A round distance close to the requested width, for the scale bar.
 *
 * Scale bars read 100 m or 500 m, never 143 m, because the point is to let
 * someone estimate a distance by eye rather than to be exact.
 */
export function niceDistance(metres: number): number {
  if (metres <= 0) {
    return 1;
  }
  const magnitude = Math.pow(10, Math.floor(Math.log10(metres)));
  const normalised = metres / magnitude;

  const step = normalised >= 5 ? 5 : normalised >= 2 ? 2 : 1;
  return step * magnitude;
}

/** Formats a ground distance for the scale bar. */
export function formatDistance(metres: number): string {
  return metres >= 1000 ? `${metres / 1000} km` : `${metres} m`;
}

/**
 * Spacing between graticule lines, in degrees, for a zoom level.
 *
 * Picked from a fixed ladder so the grid changes at predictable zooms instead
 * of drifting, and so labels stay at readable precision.
 */
export function graticuleStep(zoom: number): number {
  const ladder = [
    30, 15, 10, 5, 2, 1, 0.5, 0.2, 0.1, 0.05, 0.02, 0.01, 0.005, 0.002, 0.001,
    0.0005, 0.0002, 0.0001,
  ];
  const index = clamp(Math.floor(zoom) - MIN_ZOOM, 0, ladder.length - 1);
  return ladder[index];
}
