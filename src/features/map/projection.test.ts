/**
 * The map's geometry.
 *
 * Run with `npm run test:map`. Node's built-in test runner executes these
 * directly — no test framework is installed, because the logic under test is
 * pure arithmetic and adding a runner to reach it would cost more than it
 * returns.
 *
 * These check the numbers that make the map *honest*: a marker in the right
 * place, and an accuracy circle that covers the ground it claims to.
 */

import { test } from "node:test";
import assert from "node:assert/strict";
import {
  clampZoom,
  fitBounds,
  formatDistance,
  fromScreen,
  graticuleStep,
  MAX_ZOOM,
  metresPerPixel,
  MIN_ZOOM,
  niceDistance,
  project,
  toScreen,
  unproject,
  worldSize,
} from "./projection.ts";

const CLOSE = 1e-9;

test("the origin projects to the centre of the world square", () => {
  const origin = project({ latitude: 0, longitude: 0 });
  assert.ok(Math.abs(origin.x - 0.5) < CLOSE);
  assert.ok(Math.abs(origin.y - 0.5) < CLOSE);
});

test("longitude maps linearly across the full range", () => {
  assert.ok(Math.abs(project({ latitude: 0, longitude: -180 }).x - 0) < CLOSE);
  assert.ok(Math.abs(project({ latitude: 0, longitude: 180 }).x - 1) < CLOSE);
  assert.ok(Math.abs(project({ latitude: 0, longitude: 90 }).x - 0.75) < CLOSE);
});

test("northern latitudes project above the equator", () => {
  // y grows downward, so further north must be a smaller y.
  const north = project({ latitude: 60, longitude: 0 });
  const equator = project({ latitude: 0, longitude: 0 });
  assert.ok(north.y < equator.y);
});

test("projection round-trips through its inverse", () => {
  for (const point of [
    { latitude: 13.133599, longitude: 77.56533 },
    { latitude: -33.8688, longitude: 151.2093 },
    { latitude: 64.1466, longitude: -21.9426 },
    { latitude: 0, longitude: 0 },
  ]) {
    const restored = unproject(project(point));
    assert.ok(Math.abs(restored.latitude - point.latitude) < 1e-7, "latitude");
    assert.ok(Math.abs(restored.longitude - point.longitude) < 1e-7, "longitude");
  }
});

test("latitudes past the Mercator limit are clamped, not sent to infinity", () => {
  const north = project({ latitude: 90, longitude: 0 });
  const south = project({ latitude: -90, longitude: 0 });

  assert.ok(Number.isFinite(north.y), "the pole must land somewhere drawable");
  assert.ok(Number.isFinite(south.y));

  // Clamped to the edges of the world square. The tolerance absorbs rounding
  // in the Mercator limit constant, not an error in the projection.
  assert.ok(Math.abs(north.y) < 1e-6, "north pole at " + north.y);
  assert.ok(Math.abs(south.y - 1) < 1e-6, "south pole at " + south.y);
});

test("ground resolution matches the known value at the equator", () => {
  // The standard Web Mercator figure: ~156543 m per pixel at zoom 0, equator.
  const atZoomZero = metresPerPixel(0, 0);
  assert.ok(Math.abs(atZoomZero - 156543.03392) < 0.01, `got ${atZoomZero}`);

  // Each zoom level halves it.
  assert.ok(Math.abs(metresPerPixel(0, 1) - atZoomZero / 2) < 0.01);
  assert.ok(Math.abs(metresPerPixel(0, 10) - atZoomZero / 1024) < 0.01);
});

test("ground resolution shrinks away from the equator", () => {
  // Mercator stretches with latitude, so the same pixel covers less ground.
  // Getting this backwards would draw accuracy circles at twice their true
  // size in temperate latitudes.
  const equator = metresPerPixel(0, 14);
  const sixty = metresPerPixel(60, 14);
  assert.ok(sixty < equator);
  assert.ok(Math.abs(sixty - equator * Math.cos((60 * Math.PI) / 180)) < 1e-6);
});

test("screen conversion round-trips", () => {
  const viewport = {
    centre: { latitude: 13.133599, longitude: 77.56533 },
    zoom: 15,
    width: 800,
    height: 500,
  };

  const point = { latitude: 13.1355, longitude: 77.5675 };
  const restored = fromScreen(toScreen(point, viewport), viewport);

  assert.ok(Math.abs(restored.latitude - point.latitude) < 1e-7);
  assert.ok(Math.abs(restored.longitude - point.longitude) < 1e-7);
});

test("the viewport centre lands in the middle of the surface", () => {
  const viewport = {
    centre: { latitude: 13.133599, longitude: 77.56533 },
    zoom: 15,
    width: 800,
    height: 500,
  };

  const screen = toScreen(viewport.centre, viewport);
  assert.ok(Math.abs(screen.x - 400) < 1e-6);
  assert.ok(Math.abs(screen.y - 250) < 1e-6);
});

test("fitting no points returns nothing rather than a default position", () => {
  // There is no honest place to centre a map of nothing.
  assert.equal(fitBounds([], 800, 500), null);
});

test("fitting one point centres on it at a usable zoom", () => {
  const point = { latitude: 13.133599, longitude: 77.56533 };
  const fitted = fitBounds([point], 800, 500);

  assert.ok(fitted);
  assert.ok(Math.abs(fitted.centre.latitude - point.latitude) < 1e-7);
  assert.ok(Math.abs(fitted.centre.longitude - point.longitude) < 1e-7);
  // A single point has no extent, so the zoom cannot be derived from it.
  assert.ok(fitted.zoom >= MIN_ZOOM && fitted.zoom <= MAX_ZOOM);
});

test("fitting several points contains all of them", () => {
  const points = [
    { latitude: 13.12, longitude: 77.55 },
    { latitude: 13.14, longitude: 77.57 },
    { latitude: 13.13, longitude: 77.56 },
  ];

  const fitted = fitBounds(points, 800, 500);
  assert.ok(fitted);

  const viewport = { ...fitted, width: 800, height: 500 };
  for (const point of points) {
    const screen = toScreen(point, viewport);
    assert.ok(screen.x >= 0 && screen.x <= 800, `x ${screen.x} off surface`);
    assert.ok(screen.y >= 0 && screen.y <= 500, `y ${screen.y} off surface`);
  }
});

test("fitting stays correct across a hundred incidents", () => {
  // The stated performance target. Correctness at that size matters as much as
  // speed: a fit that drops markers off screen is worse than a slow one.
  const points = Array.from({ length: 100 }, (_, index) => ({
    latitude: 13.1 + (index % 10) * 0.004,
    longitude: 77.55 + Math.floor(index / 10) * 0.004,
  }));

  const started = performance.now();
  const fitted = fitBounds(points, 900, 600);
  const elapsed = performance.now() - started;

  assert.ok(fitted);
  assert.ok(elapsed < 50, `fitting took ${elapsed} ms`);

  const viewport = { ...fitted, width: 900, height: 600 };
  for (const point of points) {
    const screen = toScreen(point, viewport);
    assert.ok(screen.x >= 0 && screen.x <= 900);
    assert.ok(screen.y >= 0 && screen.y <= 600);
  }
});

test("zoom is held inside the renderable range", () => {
  assert.equal(clampZoom(-5), MIN_ZOOM);
  assert.equal(clampZoom(99), MAX_ZOOM);
  assert.equal(clampZoom(12), 12);
});

test("the world doubles with every zoom level", () => {
  assert.equal(worldSize(0), 256);
  assert.equal(worldSize(1), 512);
  assert.equal(worldSize(10), 256 * 1024);
});

test("the scale bar reports round distances", () => {
  // A scale bar exists to be read by eye, so 143 m is worse than 100 m.
  assert.equal(niceDistance(143), 100);
  assert.equal(niceDistance(680), 500);
  assert.equal(niceDistance(2400), 2000);
  assert.equal(niceDistance(0), 1);
  assert.equal(formatDistance(500), "500 m");
  assert.equal(formatDistance(2000), "2 km");
});

test("the graticule coarsens as the map zooms out", () => {
  const close = graticuleStep(MAX_ZOOM);
  const far = graticuleStep(MIN_ZOOM);
  assert.ok(far > close, "a zoomed-out map needs wider grid spacing");
  assert.ok(close > 0);
});
