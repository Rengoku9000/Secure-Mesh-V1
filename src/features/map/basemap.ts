/**
 * Turning a provisioned GeoJSON basemap into drawable SVG geometry.
 *
 * Projected into normalized `[0, 1]` world coordinates **once**, when the
 * basemap loads. Panning and zooming then move a single SVG transform instead
 * of reprojecting every path, which is what keeps a few thousand road segments
 * responsive on a drag.
 *
 * Nothing here interprets a URL, a style reference or a tile template. A
 * basemap is geometry and a handful of names; every other field is ignored.
 * That is why a hostile or malformed data file cannot cause a request — the
 * renderer has no way to make one.
 */

import { project } from "./projection.ts";

/**
 * What a feature is, in terms the renderer draws.
 *
 * The extraction script maps OSM's tag vocabulary onto these, so tag knowledge
 * stays in provisioning and the map code stays readable. An unrecognised kind
 * is dropped rather than guessed at.
 */
export type FeatureKind =
  | "water"
  | "waterway"
  | "boundary"
  | "rail"
  | "road-major"
  | "road-primary"
  | "road-secondary"
  | "place";

/**
 * Painter's order, back to front.
 *
 * Water first so roads cross it rather than disappear beneath it; major roads
 * last so they read above the secondary network. Incident and node markers are
 * drawn in separate layers above all of this and always win.
 */
const DRAW_ORDER: FeatureKind[] = [
  "water",
  "boundary",
  "waterway",
  "rail",
  "road-secondary",
  "road-primary",
  "road-major",
];

/** One drawable path, in normalized world coordinates. */
export interface BasemapPath {
  /** SVG path data with both axes in `[0, 1]`. */
  d: string;
  kind: FeatureKind;
  /** Areas are filled; everything else is stroked only. */
  filled: boolean;
}

/** A place name, positioned in normalized world coordinates. */
export interface BasemapLabel {
  x: number;
  y: number;
  text: string;
  /** OSM place class, used to decide which labels survive the cap. */
  place: string;
}

export interface ProjectedBasemap {
  paths: BasemapPath[];
  labels: BasemapLabel[];
}

/**
 * How many place labels are kept.
 *
 * A tactical map is read under pressure. Every village in a 20 km box would be
 * two hundred labels of overlapping text, which is less legible than none.
 */
export const MAX_LABELS = 14;

/** Which places matter most when the cap bites. */
const PLACE_RANK: Record<string, number> = {
  city: 0,
  town: 1,
  suburb: 2,
  village: 3,
};

/** Deepest coordinate nesting followed, so a crafted file cannot recurse away. */
const MAX_DEPTH = 8;

/** Decimal places kept in path data — about a centimetre of ground. */
const PRECISION = 7;

function isPosition(value: unknown): value is number[] {
  return (
    Array.isArray(value) &&
    value.length >= 2 &&
    typeof value[0] === "number" &&
    typeof value[1] === "number" &&
    Number.isFinite(value[0]) &&
    Number.isFinite(value[1])
  );
}

/** Projects a ring or line of positions into SVG path data. */
function ring(positions: unknown, close: boolean): string {
  if (!Array.isArray(positions)) {
    return "";
  }

  const commands: string[] = [];
  for (const position of positions) {
    if (!isPosition(position)) {
      continue;
    }
    // GeoJSON is [longitude, latitude]. Reversing these is the classic way to
    // put a map in the wrong hemisphere, so it is worth being explicit.
    const { x, y } = project({ latitude: position[1], longitude: position[0] });
    commands.push(
      `${commands.length === 0 ? "M" : "L"}${x.toFixed(PRECISION)} ${y.toFixed(PRECISION)}`,
    );
  }

  if (commands.length < 2) {
    return "";
  }
  return close ? `${commands.join(" ")} Z` : commands.join(" ");
}

function isFeatureKind(value: unknown): value is FeatureKind {
  return (
    typeof value === "string" &&
    (DRAW_ORDER as string[]).concat("place").includes(value)
  );
}

function walk(
  geometry: unknown,
  kind: FeatureKind,
  into: ProjectedBasemap,
  name: string | undefined,
  place: string | undefined,
  depth: number,
): void {
  if (depth > MAX_DEPTH || typeof geometry !== "object" || geometry === null) {
    return;
  }

  const node = geometry as {
    type?: unknown;
    coordinates?: unknown;
    geometries?: unknown;
  };

  if (Array.isArray(node.geometries)) {
    for (const nested of node.geometries) {
      walk(nested, kind, into, name, place, depth + 1);
    }
    return;
  }

  const push = (d: string, filled: boolean) => {
    if (d.length > 0) into.paths.push({ d, kind, filled });
  };

  switch (node.type) {
    case "Point":
    case "MultiPoint": {
      // Only named places become anything. An unnamed point carries no
      // information a marker could convey, and drawing it would compete with
      // incident markers for attention while meaning something else entirely.
      if (kind !== "place" || !name) break;
      const positions =
        node.type === "Point" ? [node.coordinates] : (node.coordinates as unknown[]);
      for (const position of positions ?? []) {
        if (!isPosition(position)) continue;
        const { x, y } = project({ latitude: position[1], longitude: position[0] });
        into.labels.push({ x, y, text: name, place: place ?? "village" });
      }
      break;
    }

    case "LineString":
      push(ring(node.coordinates, false), false);
      break;

    case "MultiLineString":
      if (Array.isArray(node.coordinates)) {
        for (const line of node.coordinates) push(ring(line, false), false);
      }
      break;

    case "Polygon":
      if (Array.isArray(node.coordinates)) {
        for (const line of node.coordinates) push(ring(line, true), true);
      }
      break;

    case "MultiPolygon":
      if (Array.isArray(node.coordinates)) {
        for (const polygon of node.coordinates) {
          if (!Array.isArray(polygon)) continue;
          for (const line of polygon) push(ring(line, true), true);
        }
      }
      break;

    default:
      break;
  }
}

/**
 * Converts a GeoJSON `FeatureCollection` into world-space geometry.
 *
 * Returns empty geometry rather than throwing for anything unusable. The core
 * has already validated the file and reported its state to the operator; by the
 * time it reaches the renderer, failing loudly a second time would replace a
 * working map with a blank panel and add no information.
 *
 * Features with no recognised `kind` are dropped. A basemap from another
 * source, without SecureMesh's classification, therefore renders as nothing at
 * all rather than as unlabelled scribble.
 */
export function projectBasemap(geojson: string): ProjectedBasemap {
  const empty: ProjectedBasemap = { paths: [], labels: [] };

  let parsed: unknown;
  try {
    parsed = JSON.parse(geojson);
  } catch {
    return empty;
  }

  const features = (parsed as { features?: unknown })?.features;
  if (!Array.isArray(features)) {
    return empty;
  }

  const collected: ProjectedBasemap = { paths: [], labels: [] };

  for (const feature of features) {
    const properties = (feature as { properties?: Record<string, unknown> })?.properties;
    const kind = properties?.kind;
    if (!isFeatureKind(kind)) {
      continue;
    }

    const name = typeof properties?.name === "string" ? properties.name : undefined;
    const place = typeof properties?.place === "string" ? properties.place : undefined;

    walk((feature as { geometry?: unknown })?.geometry, kind, collected, name, place, 0);
  }

  // Sorted into painter's order once, here, so the renderer can emit one flat
  // list without grouping on every frame.
  const order = new Map(DRAW_ORDER.map((kind, index) => [kind, index]));
  collected.paths.sort(
    (a, b) => (order.get(a.kind) ?? 0) - (order.get(b.kind) ?? 0),
  );

  collected.labels = selectLabels(collected.labels);
  return collected;
}

/**
 * Keeps the most significant place labels, up to {@link MAX_LABELS}.
 *
 * Ranked by place class rather than by position, so a town survives and a
 * hamlet does not — which is the same judgement a paper map makes. Duplicates
 * are dropped: OSM often carries the same name as several nodes.
 */
export function selectLabels(labels: BasemapLabel[]): BasemapLabel[] {
  const seen = new Set<string>();
  const unique: BasemapLabel[] = [];

  for (const label of labels) {
    if (seen.has(label.text)) continue;
    seen.add(label.text);
    unique.push(label);
  }

  unique.sort(
    (a, b) => (PLACE_RANK[a.place] ?? 9) - (PLACE_RANK[b.place] ?? 9),
  );

  return unique.slice(0, MAX_LABELS);
}
