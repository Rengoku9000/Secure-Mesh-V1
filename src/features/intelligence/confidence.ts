/**
 * How a model-stated confidence is presented to an operator.
 *
 * # Why no number is ever shown
 *
 * The analysis schema asks the model for `confidence` as an unbounded
 * `number`, and `RawAnalysis::validate` clamps whatever comes back to
 * `0.0..=1.0`. Measured against the fine-tuned candidate and the stock model
 * on fresh reports, what actually arrives is integers on mixed scales — 1, 2,
 * 3, 50, 90, 95, 100 — because the training targets contain no `confidence`
 * field at all (0 of 1052 train, 0 of 214 val records), so the model has no
 * learned convention to reproduce.
 *
 * The clamp then destroys the distinction: 90, 95 and 100 all become 1.0, and
 * so do 1, 2 and 3. Rendered as `confidence * 100`, every one of those became
 * **"100%"**. The outputs that look *least* confident displayed as maximum
 * confidence, which is worse than showing nothing.
 *
 * Reinterpreting 95 as 0.95 would be guessing at a scale the model was never
 * taught, and inventing a substitute number would be fabricating evidence. The
 * rule layer's `categoryConfidence` is defensible but scores *category* only,
 * so presenting it as an overall analysis confidence would be a different
 * claim than the one it supports.
 *
 * So the operator is told the value is unavailable, and why. An honest absence
 * is more useful than a confident-looking number with nothing behind it.
 */

/** Shown when the model stated no confidence at all. */
export const MODEL_CONFIDENCE_ABSENT = "not stated by the model";

/**
 * Shown when a value is present but carries no defensible meaning — including
 * rows stored before this policy, which may hold a clamped legacy value.
 */
export const MODEL_CONFIDENCE_WITHHELD =
  "not available — the model does not produce a calibrated confidence";

/**
 * The operator-facing text for a model-stated confidence.
 *
 * Never returns a digit or a percentage, for any input. That property is the
 * point of this function, and it is what the accompanying test pins down.
 */
export function formatModelConfidence(confidence: number | null | undefined): string {
  if (confidence === null || confidence === undefined || !Number.isFinite(confidence)) {
    return MODEL_CONFIDENCE_ABSENT;
  }
  return MODEL_CONFIDENCE_WITHHELD;
}
