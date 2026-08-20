/**
 * The offline guarantee, checked against the source rather than asserted.
 *
 * A runtime observation can only show that this machine happened not to make a
 * request. Scanning the map layer shows that no code path exists to make one —
 * which is the claim SecureMesh actually makes about its map.
 *
 * This is the same technique `src-tauri/tests/ai_boundary.rs` uses for the AI
 * subsystem, for the same reason.
 */

import { test } from "node:test";
import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const MAP_DIRECTORY = dirname(fileURLToPath(import.meta.url));

/** Every source file in the map layer, excluding the tests themselves. */
function implementationFiles(): { name: string; code: string }[] {
  return readdirSync(MAP_DIRECTORY)
    .filter((name) => /\.(ts|tsx)$/.test(name))
    .filter((name) => !name.endsWith(".test.ts"))
    .map((name) => ({
      name,
      code: stripComments(readFileSync(join(MAP_DIRECTORY, name), "utf8")),
    }));
}

/**
 * Removes comments before scanning.
 *
 * The map's own documentation explains at length that it contacts no tile
 * server and uses no API key, so a scan that included prose would fail on files
 * that are entirely correct.
 */
function stripComments(source: string): string {
  return source
    .replace(/\/\*[\s\S]*?\*\//g, "")
    .split("\n")
    .filter((line) => !line.trim().startsWith("//"))
    .join("\n");
}

test("the map layer contains no online map provider", () => {
  const providers = [
    "mapbox",
    "googleapis",
    "google.com",
    "openstreetmap.org",
    "tile.openstreetmap",
    "arcgis",
    "bing.com",
    "here.com",
    "maptiler",
    "stadiamaps",
    "cartocdn",
  ];

  for (const { name, code } of implementationFiles()) {
    const lowered = code.toLowerCase();
    for (const provider of providers) {
      assert.ok(
        !lowered.includes(provider),
        `${name} references the online map provider ${provider}`,
      );
    }
  }
});

test("the map layer requests nothing over the network", () => {
  // No transport, so no tile request, style request, glyph request or geocode
  // is possible from here regardless of what any data file might contain.
  const transports = [
    "http://",
    "https://",
    "fetch(",
    "XMLHttpRequest",
    "WebSocket",
    "EventSource",
    "navigator.sendBeacon",
    "importScripts",
  ];

  for (const { name, code } of implementationFiles()) {
    for (const transport of transports) {
      assert.ok(!code.includes(transport), `${name} uses ${transport}`);
    }
  }
});

test("the map layer carries no API key or token", () => {
  for (const { name, code } of implementationFiles()) {
    const lowered = code.toLowerCase();
    for (const secret of ["api_key", "apikey", "access_token", "accesstoken"]) {
      assert.ok(!lowered.includes(secret), `${name} references ${secret}`);
    }
  }
});

test("the map never tracks position continuously", () => {
  // Position is a snapshot the operator asks for. A watch would build a
  // movement history of the person holding the device, which SecureMesh does
  // not collect and has no way to protect.
  for (const { name, code } of implementationFiles()) {
    for (const tracking of [
      "watchPosition",
      "navigator.geolocation",
      "setInterval",
    ]) {
      assert.ok(!code.includes(tracking), `${name} uses ${tracking}`);
    }
  }
});

test("the map reaches location only through the SecureMesh core", () => {
  // The existing WindowsLocationProvider stays responsible for obtaining a
  // position. React asking the platform directly would be a second GPS
  // implementation with none of the honesty checks the first one has.
  for (const { name, code } of implementationFiles()) {
    assert.ok(
      !code.includes("geolocation"),
      `${name} talks to a platform location API directly`,
    );
  }
});
