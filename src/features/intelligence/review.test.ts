/**
 * The operator-review panel's display decisions.
 *
 * Run with `node --test "src/features/intelligence/*.test.ts"`.
 *
 * These cover the *logic* of what the panel shows. There is no component
 * runner in this project and `node --test` cannot transform JSX, so DOM
 * rendering is deliberately **not** covered here — see `review.ts`.
 *
 * The properties being pinned are safety properties, not formatting ones: the
 * model's answer must survive unchanged, evidence must stay distinguishable
 * from it, and silence must never be reported as verification.
 */

import { test } from "node:test";
import assert from "node:assert/strict";
import {
  EVIDENCE_NOT_CORRECTION,
  NO_DISAGREEMENT_NOTICE,
  REVIEW_REQUIRED_HEADING,
  formatUncheckedNotice,
  reviewPanel,
} from "./review.ts";
import type { ConsistencyReport, Disagreement } from "../../types/core";

const UNCHECKED = ["asset", "cause", "location_hint", "affected_resources"];

function disagreement(field: string, modelResult: string, evidence: string): Disagreement {
  return {
    field,
    modelResult,
    deterministicEvidence: evidence,
    reason: `${field} differs from the report's stated facts.`,
  };
}

function report(disagreements: Disagreement[]): ConsistencyReport {
  return {
    disagreements,
    uncheckedFields: UNCHECKED,
    needsOperatorReview: disagreements.length > 0,
  };
}

// 1. No disagreement.
test("a report with no disagreement does not ask for review", () => {
  const panel = reviewPanel(report([]));

  assert.equal(panel.state, "no-disagreement");
  assert.equal(panel.heading, null);
  assert.deepEqual(panel.rows, []);
});

// 2. One disagreement.
test("a single disagreement produces a single row and a review heading", () => {
  const panel = reviewPanel(report([disagreement("severity", "LOW", "CRITICAL (score 7)")]));

  assert.equal(panel.state, "review-required");
  assert.equal(panel.heading, REVIEW_REQUIRED_HEADING);
  assert.equal(panel.rows.length, 1);
  assert.equal(panel.rows[0].field, "severity");
});

// 3. Multiple disagreements.
test("several disagreements are all carried through, in order", () => {
  const panel = reviewPanel(
    report([
      disagreement("category", "OTHER", "FIRE (rule confidence 0.80)"),
      disagreement("severity", "LOW", "CRITICAL (score 7)"),
      disagreement("access_status", "OPEN", "active hazard, no route information"),
    ]),
  );

  assert.equal(panel.rows.length, 3);
  assert.deepEqual(
    panel.rows.map((row) => row.field),
    ["category", "severity", "access_status"],
    "rows must not be reordered or collapsed",
  );
});

// 4. Unchecked fields are visibly identified.
test("fields with no deterministic source are named", () => {
  for (const panel of [reviewPanel(report([])), reviewPanel(report([disagreement("severity", "LOW", "HIGH")]))]) {
    assert.ok(panel.uncheckedNotice, "unchecked fields must be stated in every state");
    for (const field of UNCHECKED) {
      assert.ok(
        panel.uncheckedNotice!.includes(field),
        `${field} must be named as unchecked: ${panel.uncheckedNotice}`,
      );
    }
  }
});

// 5. The model result is shown unchanged.
test("the model's own result is carried through verbatim", () => {
  // The defect this guards: replacing the model's answer with the rules'
  // answer, which would hide that a substitution happened.
  const panel = reviewPanel(report([disagreement("severity", "LOW", "CRITICAL (score 7)")]));

  assert.equal(panel.rows[0].modelResult, "LOW");
  assert.notEqual(panel.rows[0].modelResult, "CRITICAL");
});

// 6. Deterministic evidence is a separate field.
test("evidence is kept separate from the model result, never merged", () => {
  const panel = reviewPanel(report([disagreement("category", "OTHER", "FIRE (rule confidence 0.80)")]));
  const row = panel.rows[0];

  assert.equal(row.modelResult, "OTHER");
  assert.equal(row.deterministicEvidence, "FIRE (rule confidence 0.80)");
  assert.ok(
    !row.modelResult.includes(row.deterministicEvidence),
    "the two readings must remain independently readable",
  );
});

// 7. The review warning is rendered.
test("review required states the relationship between the two readings", () => {
  const panel = reviewPanel(report([disagreement("severity", "HIGH", "CRITICAL (score 8)")]));

  assert.equal(panel.notice, EVIDENCE_NOT_CORRECTION);
  assert.ok(panel.notice!.includes("not a correction"));
  assert.ok(panel.notice!.includes("Neither is authoritative"));
});

// 8. An empty list is not "everything verified".
test("no disagreement is never presented as verification", () => {
  const panel = reviewPanel(report([]));

  assert.equal(panel.notice, NO_DISAGREEMENT_NOTICE);
  assert.ok(
    panel.notice!.includes("not a correctness guarantee"),
    "the weaker claim must be stated explicitly",
  );
  for (const forbidden of ["verified", "validated", "all correct", "guaranteed correct"]) {
    assert.ok(
      !panel.notice!.toLowerCase().includes(forbidden),
      `must not claim "${forbidden}": ${panel.notice}`,
    );
  }
  // And it still says what was never checked at all.
  assert.ok(panel.uncheckedNotice);
});

test("no analysis yet is distinct from an analysis that found nothing", () => {
  const panel = reviewPanel(null);

  assert.equal(panel.state, "unavailable");
  assert.equal(panel.notice, null);
  assert.equal(panel.uncheckedNotice, null);
});

test("an empty unchecked list produces no notice rather than an empty one", () => {
  assert.equal(formatUncheckedNotice([]), null);
  assert.ok(formatUncheckedNotice(["asset"])!.includes("asset"));
});
