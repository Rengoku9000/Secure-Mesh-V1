/**
 * What the map draws, and — more importantly — what it refuses to draw.
 *
 * The rules under test are the ones that keep the map honest: no marker for an
 * incident with no position, no accuracy circle for a reading that carried no
 * accuracy, no peer anywhere, and no second severity scale.
 */

import { test } from "node:test";
import assert from "node:assert/strict";
import type { Incident, Peer, Severity } from "../../types/core";
import {
  accuracyRadiusPixels,
  DEVICE_SOURCE_LABEL,
  incidentFacts,
  SOURCE_LABEL,
  hasPosition,
  incidentMarkers,
  markerSignature,
  peerMarkers,
  SEVERITY_RADIUS,
  SEVERITY_TOKEN,
} from "./markers.ts";

function incident(overrides: Partial<Incident> = {}): Incident {
  return {
    id: "11111111-1111-4111-8111-111111111111",
    createdBy: "a7f32c9e00000000000000000000000000000000000000000000000000000000",
    description: "landslide across the access road",
    severity: "HIGH",
    latitude: 13.133599,
    longitude: 77.56533,
    accuracyMeters: 4.5,
    locationSource: "GNSS",
    locationCapturedAt: "2026-08-20T10:00:00.000Z",
    createdAt: "2026-08-20T10:01:00.000Z",
    updatedAt: "2026-08-20T10:01:00.000Z",
    syncStatus: "PENDING",
    ...overrides,
  };
}

test("an incident with coordinates becomes a marker", () => {
  const markers = incidentMarkers([incident()]);

  assert.equal(markers.length, 1);
  assert.equal(markers[0].incidentId, incident().id);
  assert.equal(markers[0].position.latitude, 13.133599);
  assert.equal(markers[0].position.longitude, 77.56533);
  assert.equal(markers[0].severity, "HIGH");
  assert.equal(markers[0].accuracyMeters, 4.5);
});

test("an incident with no coordinates produces no marker", () => {
  // Not a marker at a default position. An unlocated incident is ordinary in
  // SecureMesh and belongs in the table, not at a guessed point.
  const markers = incidentMarkers([
    incident({ latitude: null, longitude: null, accuracyMeters: null }),
  ]);
  assert.equal(markers.length, 0);
});

test("a half-recorded position produces no marker", () => {
  assert.equal(incidentMarkers([incident({ latitude: null })]).length, 0);
  assert.equal(incidentMarkers([incident({ longitude: null })]).length, 0);
});

test("a non-finite coordinate produces no marker", () => {
  assert.equal(incidentMarkers([incident({ latitude: Number.NaN })]).length, 0);
  assert.equal(
    incidentMarkers([incident({ longitude: Number.POSITIVE_INFINITY })]).length,
    0,
  );
});

test("located and unlocated incidents can be mixed", () => {
  const markers = incidentMarkers([
    incident({ id: "a" }),
    incident({ id: "b", latitude: null, longitude: null }),
    incident({ id: "c", latitude: 13.2, longitude: 77.6 }),
  ]);

  assert.deepEqual(
    markers.map((marker) => marker.incidentId),
    ["a", "c"],
  );
});

test("hasPosition agrees with marker production", () => {
  assert.equal(hasPosition(incident()), true);
  assert.equal(hasPosition(incident({ latitude: null })), false);
});

test("every severity maps to an existing theme token", () => {
  // The map must not invent a second severity scale. These are the tokens
  // already defined in src/styles/tokens.css and used by the incident table.
  const severities: Severity[] = ["LOW", "MEDIUM", "HIGH", "CRITICAL"];

  for (const severity of severities) {
    assert.ok(SEVERITY_TOKEN[severity].startsWith("var(--sev-"));
    assert.ok(SEVERITY_RADIUS[severity] > 0);
  }

  // Ordered, so a more serious incident reads first on a crowded map.
  assert.ok(SEVERITY_RADIUS.CRITICAL > SEVERITY_RADIUS.HIGH);
  assert.ok(SEVERITY_RADIUS.HIGH > SEVERITY_RADIUS.MEDIUM);
  assert.ok(SEVERITY_RADIUS.MEDIUM > SEVERITY_RADIUS.LOW);
});

test("a recorded accuracy becomes a circle of the right ground size", () => {
  // At zoom 14 near the equator a pixel is about 9.5 m, so a 165 m fix should
  // be roughly 17 px. The point is that the circle covers real ground rather
  // than a fixed number of pixels.
  const radius = accuracyRadiusPixels(165, 13.133599, 14);
  assert.ok(radius !== null);
  assert.ok(radius > 15 && radius < 20, `got ${radius}`);

  // Zooming in doubles it, because the ground area is unchanged.
  const closer = accuracyRadiusPixels(165, 13.133599, 15);
  assert.ok(closer !== null);
  assert.ok(Math.abs(closer - radius * 2) < 0.01);
});

test("no recorded accuracy means no circle at all", () => {
  // The circle means "the true position is somewhere in here". A reading that
  // carried no accuracy has no honest radius, and a default would invent
  // precision that was never measured.
  assert.equal(accuracyRadiusPixels(null, 13.133599, 15), null);
});

test("a nonsensical accuracy produces no circle", () => {
  assert.equal(accuracyRadiusPixels(Number.NaN, 13.1, 15), null);
  assert.equal(accuracyRadiusPixels(Number.POSITIVE_INFINITY, 13.1, 15), null);
  assert.equal(accuracyRadiusPixels(-5, 13.1, 15), null);
  assert.equal(accuracyRadiusPixels(0, 13.1, 15), null);
});

test("an accuracy finer than the zoom can show is not drawn", () => {
  // A sub-pixel ring inside the marker reads as decoration, not uncertainty.
  assert.equal(accuracyRadiusPixels(0.5, 13.1, 5), null);
});

test("no peer produces a marker, because no peer location is recorded", () => {
  // SecureMesh holds identity, trust and reachability for a peer, and nothing
  // about where it is. Placing one at an incident's coordinates would state a
  // position the system does not know.
  const peers: Peer[] = [
    {
      nodeId: "b8e4",
      nodeName: "SM-B8E4",
      publicKey: "00",
      transportPeerId: "12D3",
      connectionState: "CONNECTED",
      lastSeen: "2026-08-20T10:00:00.000Z",
      protocolVersion: 1,
      capabilities: [],
      equivocating: false,
      pendingEvents: 0,
      firstSeen: "2026-08-20T09:00:00.000Z",
      trustState: "TRUSTED",
      role: "PEER",
      grantedCapabilities: [],
      enrolledAt: "2026-08-20T09:30:00.000Z",
      enrolledBy: null,
      revokedAt: null,
      revokedBy: null,
      trustNotes: null,
    } as unknown as Peer,
  ];

  assert.deepEqual(peerMarkers(peers), []);
  assert.deepEqual(peerMarkers([]), []);
});

test("the redraw signature ignores a poll that changed nothing", () => {
  // The dashboard re-reads every two seconds and returns a fresh array each
  // time. Without a content signature the map would rebuild thirty times a
  // minute for no reason.
  const first = [incident({ id: "a" }), incident({ id: "b", latitude: 13.2 })];
  const second = [incident({ id: "a" }), incident({ id: "b", latitude: 13.2 })];

  assert.notEqual(first, second, "different array identities");
  assert.equal(markerSignature(first), markerSignature(second));
});

test("the redraw signature changes when a drawn field changes", () => {
  const base = [incident({ id: "a" })];

  for (const changed of [
    incident({ id: "a", latitude: 13.2 }),
    incident({ id: "a", longitude: 77.7 }),
    incident({ id: "a", severity: "CRITICAL" }),
    incident({ id: "a", accuracyMeters: 90 }),
    incident({ id: "a", syncStatus: "SYNCED" }),
  ]) {
    assert.notEqual(
      markerSignature(base),
      markerSignature([changed]),
      "a change the map draws must force a redraw",
    );
  }
});

test("the redraw signature ignores fields the map does not draw", () => {
  // Editing a description or re-running analysis must not redraw the map.
  const base = [incident({ id: "a" })];
  const reworded = [incident({ id: "a", description: "completely different text" })];

  assert.equal(markerSignature(base), markerSignature(reworded));
});

test("the redraw signature stays cheap at a hundred incidents", () => {
  const incidents = Array.from({ length: 100 }, (_, index) =>
    incident({ id: `incident-${index}`, latitude: 13.1 + index * 0.001 }),
  );

  const started = performance.now();
  for (let round = 0; round < 30; round += 1) {
    markerSignature(incidents);
  }
  const elapsed = performance.now() - started;

  // Thirty rounds is a minute of polling at the dashboard's two-second cycle.
  assert.ok(elapsed < 100, `a minute of polling cost ${elapsed} ms`);
});

// ---------------------------------------------------------------------------
// The incident card
// ---------------------------------------------------------------------------

test("the card states a full position when one was recorded", () => {
  const facts = incidentFacts(incident());

  assert.ok(facts);
  assert.equal(facts.coordinates, "13.133599, 77.565330");
  assert.equal(facts.accuracy, "±4.5 m");
  assert.equal(facts.source, "GNSS (satellite)");
  assert.equal(facts.captured, "2026-08-20T10:00:00.000Z");
});

test("an incident with no location has no card", () => {
  // It has no marker either. An unlocated incident belongs in the table.
  assert.equal(incidentFacts(incident({ latitude: null, longitude: null })), null);
});

test("a missing accuracy reads Unknown, never a number", () => {
  // The difference between "measured to ±5 m" and "nobody recorded how good
  // this is" is exactly what an operator needs, and a blank field loses it.
  const facts = incidentFacts(incident({ accuracyMeters: null }));
  assert.equal(facts?.accuracy, "Unknown");
});

test("a missing capture time reads Unknown", () => {
  const facts = incidentFacts(incident({ locationCapturedAt: null }));
  assert.equal(facts?.captured, "Unknown");
});

test("a wireless position is never described as GPS", () => {
  // Windows commonly returns a network-assisted fix. Calling it GPS would
  // invite someone to trust a city-block estimate as a satellite one.
  const facts = incidentFacts(incident({ locationSource: "WIRELESS" }));
  assert.equal(facts?.source, "Wireless");
  assert.ok(!facts?.source.includes("GPS"));
});

test("an unattested position says so", () => {
  const facts = incidentFacts(incident({ locationSource: "UNKNOWN" }));
  assert.equal(facts?.source, "Unknown");
});

test("every recorded source has a label, and none of them says GPS", () => {
  // "GPS" is the word this project refuses to use loosely; GNSS is named
  // precisely because it is the satellite case.
  for (const [source, label] of Object.entries(SOURCE_LABEL)) {
    assert.ok(label.length > 0, `${source} has no label`);
    assert.ok(!/\bGPS\b/.test(label), `${source} is labelled "${label}"`);
  }
  assert.equal(SOURCE_LABEL.GNSS, "GNSS (satellite)");
});

test("every live provider source has a label, and IP is not called wireless", () => {
  // The device layer can tell an IP guess from a Wi-Fi fix. The live readout
  // shows the distinction even though the stored record coarsens it.
  assert.equal(DEVICE_SOURCE_LABEL.SATELLITE, "GNSS (satellite)");
  assert.equal(DEVICE_SOURCE_LABEL.WIRELESS, "Wireless");
  assert.equal(DEVICE_SOURCE_LABEL.IP_ADDRESS, "IP address estimate");
  assert.notEqual(DEVICE_SOURCE_LABEL.IP_ADDRESS, DEVICE_SOURCE_LABEL.WIRELESS);
  assert.equal(DEVICE_SOURCE_LABEL.UNKNOWN, "Unknown");
});

test("coordinates are shown at the precision they were recorded", () => {
  // Six places, matching the incident table, so the two never appear to
  // disagree about where something is.
  const facts = incidentFacts(incident({ latitude: 13.1, longitude: 77.5 }));
  assert.equal(facts?.coordinates, "13.100000, 77.500000");
});
