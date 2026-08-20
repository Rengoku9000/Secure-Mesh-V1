/**
 * Extracts a bounded OpenStreetMap basemap into local GeoJSON.
 *
 * # This is a provisioning tool, not part of the application
 *
 * SecureMesh never downloads map data. This script is run **once, by hand, by
 * an operator**, to produce the file the application then reads from disk. The
 * shipped binary contains no HTTP client on the map path and cannot fetch
 * anything, whatever this script does.
 *
 * Keeping the extraction here rather than doing it ad hoc means the dataset is
 * reproducible: the query, the region and the filters are all recorded, so
 * anyone can regenerate the same file and compare checksums.
 *
 * ```text
 * node scripts/extract-osm-basemap.mjs <south> <west> <north> <east>
 * ```
 *
 * Get the bounding box your node actually needs from:
 *
 * ```text
 * npm run map:provision -- <node-data-dir>
 * ```
 *
 * # Licence
 *
 * OpenStreetMap data is © OpenStreetMap contributors, available under the
 * Open Database Licence (ODbL). Attribution and share-alike obligations apply
 * to whoever distributes the result. The attribution is written into the output
 * file itself so it cannot be separated from the data.
 */

import { createHash } from "node:crypto";
import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const projectRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const outputPath = join(projectRoot, "map", "basemap.geojson");

/** Community Overpass instance. Used once, by hand, never by the application. */
const OVERPASS = "https://overpass-api.de/api/interpreter";

const ATTRIBUTION = "© OpenStreetMap contributors";
const LICENSE = "ODbL 1.0 (https://opendatacommons.org/licenses/odbl/)";

/**
 * What to extract.
 *
 * Deliberately not "everything". A tactical map needs orientation — where the
 * roads, water and settlements are — not every driveway and building outline.
 * Restricting to classified roads keeps the file small enough to project in one
 * pass without simplifying geometry, which would compromise correctness.
 */
function overpassQuery(south, west, north, east) {
  const bbox = `${south},${west},${north},${east}`;
  return `
[out:json][timeout:180];
(
  way["highway"~"^(motorway|trunk|primary|secondary|tertiary)$"](${bbox});
  way["highway"~"^(motorway_link|trunk_link|primary_link|secondary_link)$"](${bbox});
  way["railway"="rail"](${bbox});
  way["waterway"~"^(river|canal)$"](${bbox});
  way["natural"="water"](${bbox});
  way["landuse"="reservoir"](${bbox});
  way["boundary"="administrative"]["admin_level"~"^(4|5|6|7|8)$"](${bbox});
  node["place"~"^(city|town|village|suburb)$"](${bbox});
);
out geom;
`.trim();
}

/**
 * Which rendering layer an element belongs to.
 *
 * The renderer draws by `kind`, not by raw OSM tags, so the tag vocabulary
 * stays here and the map code stays readable. Returns `null` for anything not
 * worth drawing.
 */
function classify(tags = {}) {
  if (tags.natural === "water" || tags.landuse === "reservoir") return "water";
  if (tags.waterway) return "waterway";
  if (tags.boundary === "administrative") return "boundary";
  if (tags.railway === "rail") return "rail";

  if (tags.highway) {
    if (/^(motorway|trunk)/.test(tags.highway)) return "road-major";
    if (/^(primary)/.test(tags.highway)) return "road-primary";
    return "road-secondary";
  }

  if (tags.place) return "place";
  return null;
}

/** True when a way's geometry closes on itself, and so encloses an area. */
function isClosed(geometry) {
  if (geometry.length < 4) return false;
  const first = geometry[0];
  const last = geometry[geometry.length - 1];
  return first.lat === last.lat && first.lon === last.lon;
}

async function main() {
  const [south, west, north, east] = process.argv.slice(2).map(Number);

  if (![south, west, north, east].every(Number.isFinite)) {
    console.error("usage: node scripts/extract-osm-basemap.mjs <south> <west> <north> <east>");
    console.error("\nGet the box your node needs from:");
    console.error("  npm run map:provision -- <node-data-dir>");
    process.exit(2);
  }
  if (south >= north || west >= east) {
    console.error("the bounding box is inverted: expected south < north and west < east");
    process.exit(2);
  }

  const query = overpassQuery(south, west, north, east);
  console.log(`region : ${south}, ${west}  to  ${north}, ${east}`);
  console.log(`source : ${OVERPASS}`);
  console.log("\nrequesting… (one request, this machine only)\n");

  const response = await fetch(OVERPASS, {
    method: "POST",
    headers: {
      "Content-Type": "application/x-www-form-urlencoded",
      "User-Agent": "SecureMesh-basemap-provisioning/1.0",
    },
    body: new URLSearchParams({ data: query }),
  });

  if (!response.ok) {
    console.error(`Overpass returned ${response.status} ${response.statusText}`);
    console.error("Nothing was written. The application is unaffected.");
    process.exit(1);
  }

  const payload = await response.json();
  const elements = payload.elements ?? [];
  console.log(`received ${elements.length} OSM element(s)`);

  const features = [];
  const counts = {};

  for (const element of elements) {
    const kind = classify(element.tags);
    if (!kind) continue;

    const properties = { kind };
    // Names are carried only where they exist. Nothing is invented, and the
    // renderer decides which of them are worth showing.
    if (element.tags?.name) properties.name = element.tags.name;
    if (element.tags?.place) properties.place = element.tags.place;

    if (element.type === "node") {
      if (!Number.isFinite(element.lat) || !Number.isFinite(element.lon)) continue;
      features.push({
        type: "Feature",
        properties,
        geometry: { type: "Point", coordinates: [element.lon, element.lat] },
      });
    } else if (element.type === "way" && Array.isArray(element.geometry)) {
      const positions = element.geometry
        .filter((p) => Number.isFinite(p.lon) && Number.isFinite(p.lat))
        .map((p) => [Number(p.lon.toFixed(6)), Number(p.lat.toFixed(6))]);

      if (positions.length < 2) continue;

      const area = (kind === "water" || kind === "boundary") && isClosed(positions);
      features.push({
        type: "Feature",
        properties,
        geometry: area
          ? { type: "Polygon", coordinates: [positions] }
          : { type: "LineString", coordinates: positions },
      });
    } else {
      continue;
    }

    counts[kind] = (counts[kind] ?? 0) + 1;
  }

  if (features.length === 0) {
    console.error("\nNo usable features in that region. Nothing was written.");
    process.exit(1);
  }

  // Attribution lives inside the file, so it cannot be separated from the data
  // it applies to. GeoJSON permits foreign members on a FeatureCollection.
  const collection = {
    type: "FeatureCollection",
    attribution: ATTRIBUTION,
    license: LICENSE,
    source: "OpenStreetMap via Overpass API",
    extractedAt: new Date().toISOString(),
    bbox: [west, south, east, north],
    features,
  };

  const text = JSON.stringify(collection);
  mkdirSync(dirname(outputPath), { recursive: true });
  writeFileSync(outputPath, text);

  const sha256 = createHash("sha256").update(text).digest("hex");

  console.log("\nfeatures by kind:");
  for (const [kind, count] of Object.entries(counts).sort()) {
    console.log(`  ${kind.padEnd(16)} ${count}`);
  }
  console.log(`\nwritten : ${outputPath}`);
  console.log(`features: ${features.length}`);
  console.log(`size    : ${(text.length / (1024 * 1024)).toFixed(2)} MB`);
  console.log(`sha256  : ${sha256}`);
  console.log(`\n${ATTRIBUTION}`);
  console.log(`Licence: ${LICENSE}`);
  console.log("\nVerify with: npm run map:provision -- <node-data-dir>");
}

main().catch((error) => {
  console.error(`extraction failed: ${error.message}`);
  console.error("Nothing was written. The application is unaffected.");
  process.exit(1);
});
