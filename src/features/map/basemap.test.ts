/**
 * Turning provisioned GeoJSON into drawable geometry.
 *
 * The file is operator-supplied, so these check that it is treated as data:
 * unusable input yields an empty map rather than an exception, no field is ever
 * interpreted as anything but geometry and names, and features arrive in an
 * order the renderer can draw straight through.
 */

import { test } from "node:test";
import assert from "node:assert/strict";
import { MAX_LABELS, projectBasemap, selectLabels } from "./basemap.ts";
import { project } from "./projection.ts";

/** Builds a FeatureCollection from `[kind, geometry, properties?]` triples. */
function collection(
  ...features: [string, unknown, Record<string, unknown>?][]
): string {
  return JSON.stringify({
    type: "FeatureCollection",
    features: features.map(([kind, geometry, extra]) => ({
      type: "Feature",
      properties: { kind, ...(extra ?? {}) },
      geometry,
    })),
  });
}

const LINE = {
  type: "LineString",
  coordinates: [
    [77.55, 13.12],
    [77.57, 13.14],
  ],
};

test("a road becomes an open path carrying its kind", () => {
  const { paths } = projectBasemap(collection(["road-major", LINE]));

  assert.equal(paths.length, 1);
  assert.equal(paths[0].kind, "road-major");
  assert.equal(paths[0].filled, false);
  assert.ok(paths[0].d.startsWith("M"));
  assert.ok(!paths[0].d.endsWith("Z"), "a road must not be closed");
});

test("a water polygon becomes a closed, filled path", () => {
  const { paths } = projectBasemap(
    collection([
      "water",
      {
        type: "Polygon",
        coordinates: [
          [
            [77.55, 13.12],
            [77.57, 13.12],
            [77.57, 13.14],
            [77.55, 13.12],
          ],
        ],
      },
    ]),
  );

  assert.equal(paths.length, 1);
  assert.equal(paths[0].kind, "water");
  assert.equal(paths[0].filled, true);
  assert.ok(paths[0].d.endsWith("Z"));
});

test("coordinates are read as longitude then latitude", () => {
  // GeoJSON orders positions [lon, lat]. Reversing them is the classic way to
  // put a map in the wrong hemisphere, and it would do so silently.
  const { paths } = projectBasemap(
    collection([
      "road-primary",
      {
        type: "LineString",
        coordinates: [
          [77.56533, 13.133599],
          [77.57, 13.14],
        ],
      },
    ]),
  );

  const expected = project({ latitude: 13.133599, longitude: 77.56533 });
  const [first] = paths[0].d.split(" L");
  const [x, y] = first.slice(1).split(" ").map(Number);

  assert.ok(Math.abs(x - expected.x) < 1e-6, `x ${x} vs ${expected.x}`);
  assert.ok(Math.abs(y - expected.y) < 1e-6, `y ${y} vs ${expected.y}`);
});

test("features are ordered back to front for the renderer", () => {
  // Sorted once here so the renderer emits one flat list. Water beneath roads,
  // major roads on top, or the network reads as a tangle.
  const { paths } = projectBasemap(
    collection(
      ["road-major", LINE],
      ["water", LINE],
      ["road-secondary", LINE],
      ["road-primary", LINE],
    ),
  );

  assert.deepEqual(
    paths.map((path) => path.kind),
    ["water", "road-secondary", "road-primary", "road-major"],
  );
});

test("multi-geometries expand into one path each", () => {
  const { paths } = projectBasemap(
    collection(
      [
        "road-secondary",
        {
          type: "MultiLineString",
          coordinates: [
            [
              [77.55, 13.12],
              [77.56, 13.13],
            ],
            [
              [77.57, 13.14],
              [77.58, 13.15],
            ],
          ],
        },
      ],
      [
        "water",
        {
          type: "MultiPolygon",
          coordinates: [
            [
              [
                [77.5, 13.1],
                [77.51, 13.1],
                [77.51, 13.11],
                [77.5, 13.1],
              ],
            ],
          ],
        },
      ],
    ),
  );

  assert.equal(paths.length, 3);
  assert.equal(paths.filter((path) => path.filled).length, 1);
});

test("nested geometry collections are followed", () => {
  const { paths } = projectBasemap(
    collection([
      "rail",
      { type: "GeometryCollection", geometries: [LINE] },
    ]),
  );

  assert.equal(paths.length, 1);
  assert.equal(paths[0].kind, "rail");
});

test("a feature with no recognised kind is dropped", () => {
  // A basemap from another source, without SecureMesh's classification, renders
  // as nothing rather than as unlabelled scribble the operator cannot read.
  const { paths } = projectBasemap(
    collection(["building", LINE], ["", LINE], [undefined as never, LINE]),
  );
  assert.equal(paths.length, 0);
});

test("a named place becomes a label, not a path", () => {
  const { paths, labels } = projectBasemap(
    collection([
      "place",
      { type: "Point", coordinates: [77.5963, 13.1007] },
      { name: "Yelahanka", place: "suburb" },
    ]),
  );

  assert.equal(paths.length, 0, "a place must not draw a mark on the map");
  assert.equal(labels.length, 1);
  assert.equal(labels[0].text, "Yelahanka");
  assert.equal(labels[0].place, "suburb");

  const expected = project({ latitude: 13.1007, longitude: 77.5963 });
  assert.ok(Math.abs(labels[0].x - expected.x) < 1e-9);
  assert.ok(Math.abs(labels[0].y - expected.y) < 1e-9);
});

test("an unnamed place produces nothing", () => {
  // Nothing is fabricated. A point with no name in the data gets no label.
  const { paths, labels } = projectBasemap(
    collection(["place", { type: "Point", coordinates: [77.59, 13.1] }]),
  );
  assert.equal(paths.length, 0);
  assert.equal(labels.length, 0);
});

test("labels are capped, keeping the most significant places", () => {
  // A tactical map is read under pressure. Two hundred overlapping village
  // names is less legible than none.
  const many = Array.from({ length: 60 }, (_, index) => ({
    x: index / 60,
    y: 0.5,
    text: `place-${index}`,
    place: index < 3 ? "town" : "village",
  }));

  const selected = selectLabels(many);

  assert.equal(selected.length, MAX_LABELS);
  assert.deepEqual(
    selected.slice(0, 3).map((label) => label.text),
    ["place-0", "place-1", "place-2"],
    "towns must outrank villages",
  );
});

test("a repeated place name is only labelled once", () => {
  const duplicated = [
    { x: 0.1, y: 0.5, text: "Yelahanka", place: "suburb" },
    { x: 0.2, y: 0.5, text: "Yelahanka", place: "village" },
  ];
  assert.equal(selectLabels(duplicated).length, 1);
});

test("unusable input produces an empty map rather than an exception", () => {
  // The core has already validated the file and told the operator what is
  // wrong. Throwing here would replace a working map with a blank panel and
  // add no information.
  for (const input of [
    "",
    "not json",
    "{}",
    '{"type":"FeatureCollection"}',
    '{"type":"FeatureCollection","features":"nope"}',
    '{"type":"FeatureCollection","features":[null]}',
    '{"type":"FeatureCollection","features":[{"geometry":null}]}',
  ]) {
    assert.deepEqual(projectBasemap(input), { paths: [], labels: [] }, `input: ${input}`);
  }
});

test("malformed positions are skipped, not drawn at zero", () => {
  const { paths } = projectBasemap(
    collection([
      "road-secondary",
      {
        type: "LineString",
        coordinates: [
          [77.55, 13.12],
          ["not", "a number"],
          [null, 5],
          [77.57, 13.14],
        ],
      },
    ]),
  );

  assert.equal(paths.length, 1);
  assert.equal(paths[0].d.split("L").length, 2, "expected exactly two vertices");
});

test("a degenerate ring is dropped", () => {
  // One vertex renders nothing while still costing a DOM node per feature on
  // every frame.
  const { paths } = projectBasemap(
    collection([
      "road-major",
      { type: "LineString", coordinates: [[77.55, 13.12]] },
    ]),
  );
  assert.equal(paths.length, 0);
});

test("deep nesting does not recurse away", () => {
  let geometry: unknown = LINE;
  for (let depth = 0; depth < 500; depth += 1) {
    geometry = { type: "GeometryCollection", geometries: [geometry] };
  }

  // Bounded rather than trusting the file. Not crashing is the property.
  assert.deepEqual(projectBasemap(collection(["rail", geometry])), {
    paths: [],
    labels: [],
  });
});

test("a realistic regional dataset projects once, quickly", () => {
  // The provisioned Bangalore extract is about 3300 features. This is that
  // size, paid once when the basemap loads and never again while panning.
  const features = Array.from({ length: 3300 }, (_, index) => {
    const kind =
      index % 5 === 0 ? "road-major" : index % 3 === 0 ? "water" : "road-secondary";
    return [
      kind,
      {
        type: "LineString",
        coordinates: [
          [77.5 + index * 0.00005, 13.1],
          [77.5 + index * 0.00005, 13.2],
        ],
      },
    ] as [string, unknown];
  });

  const started = performance.now();
  const { paths } = projectBasemap(collection(...features));
  const elapsed = performance.now() - started;

  assert.equal(paths.length, 3300);
  assert.ok(elapsed < 1000, `projecting took ${elapsed} ms`);
});
