/**
 * Where the map looks, and what that can never change.
 *
 * The bug these guard against: "My location" forced a close fixed zoom, which
 * pushed every other incident out of frame. The incidents were still there —
 * still in the list, still in the table, still rendered — but from the
 * operator's seat they had vanished, which is the same thing as being lost.
 *
 * So two families of assertion here. One says the camera frames sensibly. The
 * other says the camera cannot touch data at all.
 */

import { test } from "node:test";
import assert from "node:assert/strict";
import {
  distanceKm,
  frameOnSelf,
  NEARBY_KM,
  offscreenCount,
  SELF_ZOOM,
  zoomToInclude,
} from "./camera.ts";
import { incidentMarkers, markerSignature } from "./markers.ts";
import { fitBounds, MAX_ZOOM, MIN_ZOOM, toScreen } from "./projection.ts";
import type { Incident } from "../../types/core";

const WIDTH = 900;
const HEIGHT = 480;

/** The position this machine actually reports. */
const HERE = { latitude: 13.133599, longitude: 77.56533 };

/** About 2 km north-east of HERE. */
const NEAR = { latitude: 13.1465, longitude: 77.5785 };

/** About 12 km away — still worth framing together. */
const MIDDLING = { latitude: 13.2, longitude: 77.63 };

/** Another continent. */
const FAR = { latitude: 48.8584, longitude: 2.2945 };

function incident(overrides: Partial<Incident> = {}): Incident {
  return {
    id: "11111111-1111-4111-8111-111111111111",
    createdBy: "a7f32c9e00000000000000000000000000000000000000000000000000000000",
    description: "landslide across the access road",
    severity: "HIGH",
    latitude: HERE.latitude,
    longitude: HERE.longitude,
    accuracyMeters: 4.5,
    locationSource: "GNSS",
    locationCapturedAt: "2026-08-20T10:00:00.000Z",
    createdAt: "2026-08-20T10:01:00.000Z",
    updatedAt: "2026-08-20T10:01:00.000Z",
    syncStatus: "PENDING",
    ...overrides,
  };
}

/** Whether a position lands inside the viewport. */
function visible(point: { latitude: number; longitude: number }, camera: { centre: typeof HERE; zoom: number }) {
  const screen = toScreen(point, { ...camera, width: WIDTH, height: HEIGHT });
  return screen.x >= 0 && screen.x <= WIDTH && screen.y >= 0 && screen.y <= HEIGHT;
}

// ---------------------------------------------------------------------------
// Distance
// ---------------------------------------------------------------------------

test("distance is measured on the globe, not in degrees", () => {
  assert.equal(Math.round(distanceKm(HERE, HERE)), 0);

  // Roughly 2 km, and certainly not the ~0.018 a naive degree difference gives.
  const near = distanceKm(HERE, NEAR);
  assert.ok(near > 1 && near < 3, `expected ~2 km, got ${near}`);

  // Bengaluru to Paris is around 7 800 km.
  const far = distanceKm(HERE, FAR);
  assert.ok(far > 7000 && far < 8500, `expected ~7800 km, got ${far}`);
});

// ---------------------------------------------------------------------------
// Zoom that holds the centre
// ---------------------------------------------------------------------------

test("zoomToInclude keeps every point on screen with the centre held", () => {
  const zoom = zoomToInclude(HERE, [NEAR, MIDDLING], WIDTH, HEIGHT);
  const camera = { centre: HERE, zoom };

  assert.ok(visible(NEAR, camera), "the near incident is off screen");
  assert.ok(visible(MIDDLING, camera), "the middling incident is off screen");
  // And the operator is still exactly in the middle.
  assert.ok(visible(HERE, camera));
});

test("zoomToInclude reports the closest zoom that still fits", () => {
  const zoom = zoomToInclude(HERE, [MIDDLING], WIDTH, HEIGHT);

  // One step closer must push it off, or the answer was not the closest fit.
  assert.ok(!visible(MIDDLING, { centre: HERE, zoom: zoom + 1 }));
});

test("zoomToInclude yields the maximum when there is nothing to include", () => {
  // No geometry implies no zoom, so the caller applies its own default rather
  // than being handed an arbitrary one.
  assert.equal(zoomToInclude(HERE, [], WIDTH, HEIGHT), MAX_ZOOM);
  assert.equal(zoomToInclude(HERE, [HERE], WIDTH, HEIGHT), MAX_ZOOM);
});

test("zoomToInclude stays inside the renderable range", () => {
  const zoom = zoomToInclude(HERE, [FAR], WIDTH, HEIGHT);
  assert.ok(zoom >= MIN_ZOOM && zoom <= MAX_ZOOM);
});

// ---------------------------------------------------------------------------
// My location
// ---------------------------------------------------------------------------

test("My location centres on the operator", () => {
  const camera = frameOnSelf(HERE, [NEAR], WIDTH, HEIGHT);
  assert.equal(camera.centre.latitude, HERE.latitude);
  assert.equal(camera.centre.longitude, HERE.longitude);
});

test("My location keeps nearby incidents in frame", () => {
  // The regression this exists for: a fixed close zoom pushed every other
  // incident off screen, and it looked as though they had been dropped.
  const camera = frameOnSelf(HERE, [NEAR, MIDDLING], WIDTH, HEIGHT);

  assert.ok(visible(NEAR, camera), "a 2 km incident must stay visible");
  assert.ok(visible(MIDDLING, camera), "a 12 km incident must stay visible");
});

test("My location closes in when there is nothing else nearby", () => {
  const camera = frameOnSelf(HERE, [], WIDTH, HEIGHT);
  assert.equal(camera.zoom, SELF_ZOOM);
});

test("My location never zooms in past its own limit", () => {
  // Even a single incident a metre away must not zoom to street furniture.
  const almostHere = { latitude: HERE.latitude + 0.00001, longitude: HERE.longitude };
  const camera = frameOnSelf(HERE, [almostHere], WIDTH, HEIGHT);
  assert.ok(camera.zoom <= SELF_ZOOM);
});

test("My location does not zoom out to the whole world for a distant incident", () => {
  // "Take me to where I am" has to keep meaning that. A far incident is not
  // framed — it is still rendered, and Fit all is the way to reach it.
  const camera = frameOnSelf(HERE, [FAR], WIDTH, HEIGHT);

  assert.equal(camera.zoom, SELF_ZOOM);
  assert.equal(camera.centre.latitude, HERE.latitude);
});

test("the nearby threshold is what decides framing", () => {
  const justInside = frameOnSelf(HERE, [MIDDLING], WIDTH, HEIGHT, {
    nearbyKm: NEARBY_KM,
  });
  const justOutside = frameOnSelf(HERE, [MIDDLING], WIDTH, HEIGHT, {
    nearbyKm: 1,
  });

  assert.ok(justInside.zoom < SELF_ZOOM, "a nearby incident should widen the view");
  assert.equal(justOutside.zoom, SELF_ZOOM, "a distant one should not");
});

// ---------------------------------------------------------------------------
// Fit incidents, and Fit all
// ---------------------------------------------------------------------------

test("Fit incidents frames every located incident", () => {
  const points = [HERE, NEAR, MIDDLING];
  const fitted = fitBounds(points, WIDTH, HEIGHT);
  assert.ok(fitted);

  for (const point of points) {
    assert.ok(visible(point, fitted), `${JSON.stringify(point)} is off screen`);
  }
});

test("Fit incidents works with exactly one incident", () => {
  const fitted = fitBounds([NEAR], WIDTH, HEIGHT);
  assert.ok(fitted);
  assert.ok(Math.abs(fitted.centre.latitude - NEAR.latitude) < 1e-7);
  assert.ok(visible(NEAR, fitted));
});

test("Fit incidents does nothing when there is nothing to fit", () => {
  // Gracefully, and without inventing a place to look at.
  assert.equal(fitBounds([], WIDTH, HEIGHT), null);
});

test("Fit incidents ignores incidents that have no coordinates", () => {
  // The marker layer drops them, so they never reach the camera at all.
  const positions = incidentMarkers([
    incident({ id: "a" }),
    incident({ id: "b", latitude: null, longitude: null }),
    incident({ id: "c", latitude: NEAR.latitude, longitude: NEAR.longitude }),
  ]).map((marker) => marker.position);

  assert.equal(positions.length, 2);
  const fitted = fitBounds(positions, WIDTH, HEIGHT);
  assert.ok(fitted);
  for (const point of positions) {
    assert.ok(visible(point, fitted));
  }
});

test("Fit all frames the operator together with every incident", () => {
  const points = [NEAR, MIDDLING, HERE];
  const fitted = fitBounds(points, WIDTH, HEIGHT);
  assert.ok(fitted);

  assert.ok(visible(HERE, fitted), "the node must be in frame");
  assert.ok(visible(NEAR, fitted));
  assert.ok(visible(MIDDLING, fitted));
});

test("Fit all works when no position has been taken", () => {
  // Only incidents, then — the node is included only when it actually exists.
  const fitted = fitBounds([NEAR, MIDDLING], WIDTH, HEIGHT);
  assert.ok(fitted);
  assert.ok(visible(NEAR, fitted));
});

test("Fit all works when there is a position but no incidents", () => {
  const fitted = fitBounds([HERE], WIDTH, HEIGHT);
  assert.ok(fitted);
  assert.ok(visible(HERE, fitted));
});

test("Fit all with nothing at all does nothing", () => {
  assert.equal(fitBounds([], WIDTH, HEIGHT), null);
});

// ---------------------------------------------------------------------------
// The camera cannot change data
// ---------------------------------------------------------------------------

test("framing returns a camera and nothing else", () => {
  // Structural: every function here takes positions and returns a centre and a
  // zoom. There is no incident, no selection and no location record in any
  // signature, so no camera action has anything to mutate.
  const camera = frameOnSelf(HERE, [NEAR], WIDTH, HEIGHT);
  assert.deepEqual(Object.keys(camera).sort(), ["centre", "zoom"]);
});

test("framing does not disturb the positions it was given", () => {
  const incidents = [
    { latitude: NEAR.latitude, longitude: NEAR.longitude },
    { latitude: MIDDLING.latitude, longitude: MIDDLING.longitude },
  ];
  const before = JSON.stringify(incidents);

  frameOnSelf(HERE, incidents, WIDTH, HEIGHT);
  zoomToInclude(HERE, incidents, WIDTH, HEIGHT);
  fitBounds(incidents, WIDTH, HEIGHT);
  offscreenCount(incidents, { centre: HERE, zoom: 18, width: WIDTH, height: HEIGHT });

  assert.equal(JSON.stringify(incidents), before, "a coordinate was mutated");
});

test("the marker set is identical before and after any camera action", () => {
  // The heart of the complaint: markers come from the incident list, never
  // from the viewport. Moving the camera cannot remove one.
  const incidents = [
    incident({ id: "a" }),
    incident({ id: "b", latitude: NEAR.latitude, longitude: NEAR.longitude }),
    incident({ id: "c", latitude: FAR.latitude, longitude: FAR.longitude }),
  ];

  const before = markerSignature(incidents);
  const positions = incidentMarkers(incidents).map((marker) => marker.position);

  frameOnSelf(HERE, positions, WIDTH, HEIGHT);
  fitBounds(positions, WIDTH, HEIGHT);

  assert.equal(markerSignature(incidents), before);
  assert.equal(incidentMarkers(incidents).length, 3);
});

test("an incident far outside the view is still a marker", () => {
  // Zoomed tight on the operator, with an incident on another continent.
  const incidents = [incident({ id: "far", latitude: FAR.latitude, longitude: FAR.longitude })];
  const markers = incidentMarkers(incidents);

  const camera = frameOnSelf(HERE, markers.map((m) => m.position), WIDTH, HEIGHT);
  const viewport = { ...camera, width: WIDTH, height: HEIGHT };

  // Off screen, and still present. Those are different things.
  assert.equal(offscreenCount(markers.map((m) => m.position), viewport), 1);
  assert.equal(markers.length, 1);
});

// ---------------------------------------------------------------------------
// Reporting what is off screen
// ---------------------------------------------------------------------------

test("offscreenCount counts, and only counts", () => {
  const camera = frameOnSelf(HERE, [], WIDTH, HEIGHT);
  const viewport = { ...camera, width: WIDTH, height: HEIGHT };

  assert.equal(offscreenCount([HERE], viewport), 0);
  assert.equal(offscreenCount([FAR], viewport), 1);
  assert.equal(offscreenCount([HERE, FAR, FAR], viewport), 2);
  assert.equal(offscreenCount([], viewport), 0);
});

test("nothing is off screen once everything has been fitted", () => {
  const points = [HERE, NEAR, MIDDLING];
  const fitted = fitBounds(points, WIDTH, HEIGHT);
  assert.ok(fitted);

  assert.equal(
    offscreenCount(points, { ...fitted, width: WIDTH, height: HEIGHT }),
    0,
  );
});
