/**
 * The confidence display policy.
 *
 * Run with `node --test "src/features/intelligence/*.test.ts"`. Node's built-in
 * runner executes these directly — no test framework is installed, matching the
 * convention already used by the map tests.
 *
 * These exist because the defect was not a formatting slip but a *policy*
 * failure: a clamped `1.0` rendered as "100%" whether the model had said 0.95,
 * 2 or 100. So the assertion is not "this input formats that way" — it is that
 * **no input whatsoever produces a number**. A future change that reintroduces
 * a percentage fails here regardless of which value it chooses to trust.
 */

import { test } from "node:test";
import assert from "node:assert/strict";
import {
  formatModelConfidence,
  MODEL_CONFIDENCE_ABSENT,
  MODEL_CONFIDENCE_WITHHELD,
} from "./confidence.ts";

/**
 * Every value observed from a model, plus the boundaries validation allows.
 * 1, 2, 3, 50, 90, 95 and 100 were emitted by the candidate and stock models on
 * fresh reports; 0.9 and 0.95 are legacy stored values on the 0..1 scale.
 */
const OBSERVED_AND_LEGACY = [
  null,
  undefined,
  0,
  0.5,
  0.75,
  0.9,
  0.95,
  1,
  2,
  3,
  50,
  80,
  90,
  95,
  100,
  -1,
  -2,
  1e9,
  Number.NaN,
  Number.POSITIVE_INFINITY,
  Number.NEGATIVE_INFINITY,
];

test("no confidence value is ever rendered as a number", () => {
  for (const value of OBSERVED_AND_LEGACY) {
    const shown = formatModelConfidence(value as number | null | undefined);
    assert.ok(
      !/\d/.test(shown),
      `confidence ${String(value)} rendered with a digit: ${shown}`,
    );
  }
});

test("no confidence value is ever rendered as a percentage", () => {
  for (const value of OBSERVED_AND_LEGACY) {
    const shown = formatModelConfidence(value as number | null | undefined);
    assert.ok(
      !shown.includes("%"),
      `confidence ${String(value)} rendered as a percentage: ${shown}`,
    );
  }
});

test("the values that all collapsed to 100% are no longer indistinguishable from each other", () => {
  // 1, 2, 3, 90, 95 and 100 every one clamped to 1.0 and displayed as "100%".
  // They must now all report unavailability rather than a false maximum.
  for (const value of [1, 2, 3, 90, 95, 100]) {
    assert.equal(formatModelConfidence(value), MODEL_CONFIDENCE_WITHHELD);
  }
});

test("an absent confidence is distinguished from a withheld one", () => {
  assert.equal(formatModelConfidence(null), MODEL_CONFIDENCE_ABSENT);
  assert.equal(formatModelConfidence(undefined), MODEL_CONFIDENCE_ABSENT);
  assert.equal(formatModelConfidence(0.95), MODEL_CONFIDENCE_WITHHELD);
});

test("a non-finite confidence is treated as absent rather than shown", () => {
  // Mirrors `RawAnalysis::validate`, which drops non-finite values.
  for (const bad of [Number.NaN, Number.POSITIVE_INFINITY, Number.NEGATIVE_INFINITY]) {
    assert.equal(formatModelConfidence(bad), MODEL_CONFIDENCE_ABSENT);
  }
});

test("the operator is told why the value is missing, not just that it is", () => {
  // An unexplained blank invites an operator to assume a tool failure; the
  // point is that the model has no calibrated confidence to give.
  assert.ok(MODEL_CONFIDENCE_WITHHELD.includes("calibrated"));
  assert.ok(MODEL_CONFIDENCE_ABSENT.length > 0);
});
