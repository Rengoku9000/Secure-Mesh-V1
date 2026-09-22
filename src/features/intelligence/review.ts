/**
 * What the operator-review panel displays, as a pure decision.
 *
 * # Why this is a module rather than logic inside the component
 *
 * There is no component test runner in this project — no vitest, jest,
 * testing-library or jsdom — and `node --test` strips TypeScript types but
 * cannot transform JSX, so a `.tsx` component cannot be imported into a test.
 * Introducing a runner would be new frontend infrastructure.
 *
 * So the *decisions* live here, in plain TypeScript that the existing runner
 * can execute, and the component renders what this returns. That covers the
 * display logic. It does **not** cover DOM rendering, and this file should not
 * be mistaken for component coverage.
 *
 * Mirrors the `confidence.ts` precedent, which exists for the same reason.
 *
 * # The property this is here to protect
 *
 * The deterministic layer is *evidence*, never a correction. Three things
 * follow, and the accompanying tests pin all three:
 *
 * - the model's own result is carried through **unchanged**, never rewritten
 *   to the rules' answer and never merged with it;
 * - model result and deterministic evidence stay in **separate fields**, so a
 *   reader can always tell which is which;
 * - an empty disagreement list is **never** presented as "verified". It means
 *   the two agree on what can be checked, which is a much weaker claim.
 */

import type { ConsistencyReport } from "../../types/core";

export const REVIEW_REQUIRED_HEADING = "Operator review required";

/**
 * Shown above the disagreements. States the relationship between the two
 * readings, because an operator who thinks the rules are authoritative will
 * defer to them — and they are frequently wrong too.
 */
export const EVIDENCE_NOT_CORRECTION =
  "Both readings are shown. Neither is authoritative — the deterministic checks are " +
  "evidence for you to weigh, not a correction.";

/**
 * Shown when nothing disagrees.
 *
 * Deliberately not "verified", "validated" or "all correct". The checks cover
 * five fields and cannot speak to the rest, so the strongest honest statement
 * is that the model and the rules agree on what can be checked.
 */
export const NO_DISAGREEMENT_NOTICE =
  "Deterministic checks found no disagreement. That is not a correctness guarantee — " +
  "it means the model and the rules agree on everything that can be checked.";

/** One row of the review panel. */
export interface ReviewRow {
  field: string;
  /** Exactly what the model said. Never rewritten. */
  modelResult: string;
  /** What rules over the same report text derived. Kept separate. */
  deterministicEvidence: string;
  reason: string;
}

export type ReviewState = "review-required" | "no-disagreement" | "unavailable";

export interface ReviewPanel {
  state: ReviewState;
  /** Non-null only when review is required. */
  heading: string | null;
  rows: ReviewRow[];
  /** Always present when the report names unchecked fields. */
  uncheckedNotice: string | null;
  /** The explanatory line for the current state. */
  notice: string | null;
}

/** Names the fields nothing deterministic could speak to. */
export function formatUncheckedNotice(uncheckedFields: string[]): string | null {
  if (uncheckedFields.length === 0) {
    return null;
  }
  return `Not checked: ${uncheckedFields.join(", ")}.`;
}

/**
 * Decides what the review panel shows for one consistency report.
 *
 * `null` means no analysis has been produced yet — distinct from an analysis
 * whose checks found nothing.
 */
export function reviewPanel(consistency: ConsistencyReport | null | undefined): ReviewPanel {
  if (!consistency) {
    return {
      state: "unavailable",
      heading: null,
      rows: [],
      uncheckedNotice: null,
      notice: null,
    };
  }

  const uncheckedNotice = formatUncheckedNotice(consistency.uncheckedFields);

  if (!consistency.needsOperatorReview) {
    return {
      state: "no-disagreement",
      heading: null,
      rows: [],
      uncheckedNotice,
      notice: NO_DISAGREEMENT_NOTICE,
    };
  }

  return {
    state: "review-required",
    heading: REVIEW_REQUIRED_HEADING,
    // Carried through verbatim, in order. Nothing is summarised, reordered or
    // collapsed: an operator comparing two readings needs both as written.
    rows: consistency.disagreements.map((disagreement) => ({
      field: disagreement.field,
      modelResult: disagreement.modelResult,
      deterministicEvidence: disagreement.deterministicEvidence,
      reason: disagreement.reason,
    })),
    uncheckedNotice,
    notice: EVIDENCE_NOT_CORRECTION,
  };
}
