# SecureMesh-SLM — Fine-Tuning Plan and Status

**Status: pipeline complete, dataset expanded to 1,428 validated examples,
no fine-tuning run has happened. Nothing in `src-tauri/` has changed.** This
document exists so that claim can be checked rather than taken on faith, in
the same spirit as `PROVISIONING.md` and `EVALUATION.md`: every number below
is either measured or explicitly marked not yet measured.

---

## What SecureMesh-SLM is, and is not

SecureMesh already runs a fully local, schema-validated, evaluated
generation model — Qwen2.5-1.5B-Instruct, reached through
`LocalInferenceEngine` / `LlamaServerEngine`, gated by `InferenceGate`,
prompted by `ai/prompt.rs`, and validated on the way out by
`RawAnalysis::validate()`. See `docs/architecture/ARCHITECTURE.md` §6 and
`docs/ai/EVALUATION.md` for what is already built and already measured.

**SecureMesh-SLM is a specialization of that same model via LoRA/QLoRA
fine-tuning, not a new subsystem.** It changes only the weights loaded at
`LlamaConfig::generation()`'s model path. It does not:

- introduce a second inference engine, schema, or trust boundary;
- change what a model is allowed to say (the schema in `ai/prompt.rs` is the
  target for training data, not something this pipeline redefines);
- touch networking, sync, identity, security, or the frontend, at any point.

The motivation is the two weakest numbers in `EVALUATION.md`: **category
accuracy at 70–80%** and **severity accuracy at 25–43%** (the latter partly
unlearnable by construction — see that document's own analysis). Prompt
engineering already recovered most of what it can (bare enum → described
categories took accuracy from 36.7% to 70.0%, per `EVALUATION.md`); the next
lever is training data, not more prompt tuning.

---

## The pipeline

```
training/scripts/scenarios/        108 authored scenario families
        │  generate_dataset.py — slot fills + style transforms, deterministic
        ▼
training/data/raw/*.jsonl          1,428 records, schema-validated
        │  build_dataset.py — validate, split by group, verify no leakage
        ▼
training/data/processed/{train,val,test}.jsonl
        │  finetune.py — LoRA/QLoRA on a local Qwen2.5-1.5B-Instruct (NOT RUN YET)
        ▼
training/artifacts/securemesh-slm-v1-lora/    (adapter; does not exist yet)
        │  export_gguf.py — merge, convert, quantize (NOT RUN YET)
        ▼
training/artifacts/securemesh-slm-v1.gguf     (does not exist yet)
        │  evaluate.py + cargo run --example run_benchmark
        ▼
        a human decision: copy to ai/models/llm/, update LlamaConfig, or not
```

Full detail, including exact commands and dependencies, is in
`training/README.md`. This document is the project-facing plan and status;
that one is the pipeline's own operating manual.

---

## The dataset

### Schema

Every record's `target` is exactly the shape `RawAnalysis::validate()` in
`src-tauri/src/domain/intelligence.rs` accepts — same enums, same required
fields, same length bounds. Not a new schema; see `training/README.md`'s
"Keeping this in sync" section for the exact Rust lines each constant in
`validate_dataset.py` mirrors, and what to update if the Rust side changes.

Record envelope:

```json
{
  "id": "sm-016",
  "report_text": "woman in labour needs urgent help, nearest clinic is 40 min away by road, road partially blocked too, pls advise",
  "target": {
    "category": "MEDICAL",
    "severity": "CRITICAL",
    "summary": "A woman in labour urgently needs medical help, but the nearest clinic is forty minutes away and the access road is partially blocked.",
    "access_status": "RESTRICTED",
    "entities": ["woman in labour"],
    "affected_resources": ["ambulance", "medical transport"],
    "location_hint": "40 minutes from nearest clinic"
  },
  "provenance": "synthetic",
  "scenario_group": "medical_labour_1",
  "variation": ["informal", "incomplete_grammar"]
}
```

`provenance` is `synthetic`, `real`, or `operator_corrected`; `real` and
`operator_corrected` additionally require a `source` field. A `synthetic`
record is refused by the validator if its text claims a real agency
authority (NDMA, NDRF, FEMA, IFRC, etc.) — the same discipline already
applied to the operational knowledge pack
(`docs/architecture/ARCHITECTURE.md` §6d.4).

### What exists today

**1,428 examples, all `provenance: synthetic`**, in two files:

| File | Records | Origin |
|---|---|---|
| `incidents_v1.jsonl` | 60 | Written directly, one scenario each |
| `incidents_v2_generated.jsonl` | 1,368 | Expanded from **108 authored scenario families** in `training/scripts/scenarios/` |

Nothing is derived from the existing template generator in `ai/dataset.rs`,
and nothing is free-generated. Each family is hand-written and carries
several **evidence levels** — the same situation established to different
degrees — plus slot vocabulary and style transforms that vary surface form
without touching the label.

### The problem this dataset was built to fix

The Phase 1 set of 60 had a defect that only shows up when measured:
**severity was predictable from category alone 48.3% of the time** (chance is
25%). MEDICAL was 3/5 CRITICAL, OTHER was 4/5 LOW, INFRASTRUCTURE had no LOW
and no CRITICAL example at all. A model could have scored respectably on it
while ignoring the report text entirely — which is the one behaviour an
emergency classifier must not have.

The expansion attacks that directly. Ladder severities are assigned across
each category's four ladders so every category contributes LOW, MEDIUM, HIGH
and CRITICAL in roughly equal measure:

| | Phase 1 (60) | Phase 2 (1,428) |
|---|---|---|
| Severity guessable from category | **48.3%** | **31.9%** |
| Most predictable category | OTHER, 80% LOW | OTHER, 43.7% LOW |
| Report length range | 77–130 chars | 48–395 chars |
| Scenario groups | 60 (all singletons) | 168 (mean 8.5 records) |
| Groups with >1 severity (contrastive) | 0 | 106 of 168 |

Chance is 25%, so the remaining 6.9 points above it reflect genuine domain
structure — routine administrative traffic really is usually low severity —
rather than a shortcut the corpus rewards.

### Coverage

| Requirement | Records | How |
|---|---|---|
| All 12 categories | 105–109 each (8.1–8.4%) | 9 families per category |
| Contrastive (evidence changes the label) | 1,180 | Ladder, ambiguous, conflicting and false-positive levels |
| Ambiguous / insufficient evidence | 120 | Conservative answer, `UNKNOWN` access, nothing invented |
| Conflicting reports | 96 | Disagreement left unresolved rather than averaged |
| False positives / stand-downs | 28 | Alarm that was burnt toast; inspection that found nothing |
| Non-emergency informational | 92 | Shift handovers, weekly summaries, resupply arrivals |
| Multi-message situations | 96 | Timestamped sequences that escalate *and* de-escalate |
| Noisy (typos) | 491 | Deterministic character perturbation, digits never touched |
| Abbreviated | 874 | `ppl`, `nr`, `b4`, `thru`, `w/`, `evac` … |
| Short reports (<80 chars) | 451 | Clipping transform |
| Long reports (>160 chars) | 130 | Authored site write-ups, up to 395 chars |
| Resource requests | 1,133 | `affected_resources` populated |
| People counts | 499 | Numeric quantities in `entities` |
| Multiple entities | — | Detailed families carry 3–4 per record |
| Location references | 1,416 | `location_hint`, absent where the report names none |

Two known hard cases from `EVALUATION.md`'s own failure analysis are
retained verbatim in `incidents_v1.jsonl`: `sm-016` ("woman in labour…") and
`sm-055` ("the river has breached the embankment…").

### Validated

```
python training/scripts/validate_dataset.py training/data/raw/*.jsonl --corpus-checks
# TOTAL: 1428 valid record(s), 0 error(s)
# 0 corpus error(s), 1 warning(s)
```

Per-record checks enforce the Rust schema. Corpus checks — added in this
phase — cover exact duplicates, cross-group near-duplicates, wording shared
across families, label imbalance, and the category→severity shortcut score.

Two new checks earned their place immediately by finding real defects during
this phase:

- **Fabricated precision.** A target may not claim a quantity its report
  never gave. It caught a hand-authored record asserting `"2 people affected"`
  where one phrasing said only "a couple of people" — exactly the invented
  precision an emergency record must never contain. Scoped to quantities of
  two or more, because English expresses "one" without a numeral constantly.
- **Duplicate reports.** It caught five generated records where two style
  transforms had coincided on a sentence that offered neither anything to
  change, producing identical text under different ids.

The validator was also re-confirmed to reject bad input rather than pass
everything: six deliberately broken records (invalid severity enum, smuggled
unknown field, missing required field, `real` provenance with no `source`, a
`synthetic` record claiming NDMA authorship, a duplicate `id`) are all still
rejected with specific per-field messages.

One warning is reported and deliberately not fixed: `access_status=UNKNOWN`
sits at 11.3% against a 12.5% even-share floor. The four access values are
not equally likely in real reporting, and inflating `UNKNOWN` to satisfy a
threshold would misrepresent the domain. See `training/README.md`.

### Split

```
python training/scripts/build_dataset.py
```

Result: **1,052 train / 214 val / 162 test** across 120 / 24 / 24 scenario
groups, with all 12 categories and all 4 severities present in every split.

Splitting is by whole `scenario_group` — a family and all its contrastive
variants stay together — and **stratified by category**. The stratification
is not cosmetic: a flat hash bucket produced a test split containing no
MEDICAL and no OTHER records, which would have made the test score silent
about two of the twelve categories.

Separation is verified two ways on every build, not assumed:

- no scenario_group appears in more than one split;
- no held-out report is a near-duplicate of any training report — all 376
  held-out records checked against all 1,052 training records at a Jaccard
  threshold of 0.80 over word shingles. **Zero findings.**

The generator is deterministic: regenerating produces a byte-identical file
(verified by SHA-256 across runs), so a diff in the corpus means the authored
scenarios changed, never that a random seed moved.

---

## What has NOT happened

- **No fine-tuning run.** `finetune.py` is complete and was verified to
  correctly detect a missing local environment (no `torch`/`transformers`/
  `peft`/`pyyaml` installed on the machine this was built on) and refuse
  without attempting anything — it was not otherwise exercised.
- **No GGUF export.** `export_gguf.py` was verified to correctly detect
  missing local tools (no base model, no adapter, no llama.cpp checkout) and
  refuse — it was not otherwise exercised.
- **No change to the production model or its configuration.**
  `ai/models/llm/qwen2.5-1.5b-instruct-q4_k_m.gguf`, `LlamaConfig`, and every
  file under `src-tauri/` are byte-for-byte what they were before this work
  started. Verified with `git status`: the only changes in the repository are
  `training/` and this document.
- **`evaluate.py` was exercised and works.** It requires no model and no
  network; it was run against a synthetic predictions file (deliberately
  seeded with wrong categories, an unparsable response, and a missing
  prediction) and correctly computed per-field accuracy, flagged the
  malformed and missing entries, and left the correct fields at 100% — see
  `training/README.md` for the exact command.

---

## Baseline: the unmodified production model on the held-out test set

**Measured, not estimated.** The production GGUF —
`qwen2.5-1.5b-instruct-q4_k_m.gguf`, SHA-256 verified against
`ai/models/README.md` — run through the provisioned `llama-server` against
all 162 held-out test records, with the same schema-constrained decoding the
Rust adapter uses. 565 s wall time, zero request failures.

```
python training/scripts/run_baseline.py --out training/results/predictions-base-test.jsonl
python training/scripts/evaluate.py --gold training/data/processed/test.jsonl \
    --predictions training/results/predictions-base-test.jsonl \
    --out training/results/baseline-test-metrics.json
```

| Metric | Base Qwen2.5-1.5B-Instruct |
|---|---:|
| Output parsed as JSON | **99.4%** (161/162) |
| Output the *training-data* schema accepts | 82.7% |
| Category accuracy (12 classes, chance 8.3%) | **47.2%** |
| Severity accuracy (4 classes, chance 25%) | **37.3%** |
| Access status accuracy (4 classes) | **13.0%** |
| Keyword recall | 41.7% |
| Latency, mean / median | 3 485 ms / 3 323 ms |

### What the baseline actually fails at

Four findings, each of which a fine-tuned model would have to beat:

**1. It will not commit to an access status.** It answered `UNKNOWN` for 153
of 161 parsed records — `OPEN` 0/77, `RESTRICTED` 0/34, `BLOCKED` 5/35,
`UNKNOWN` 16/16. Part of this is genuine model weakness and part is a
convention mismatch: the system prompt says "use the UNKNOWN value rather
than guessing", and the model is obeying it literally, while the dataset
labels access status from context. Any later improvement here is therefore
partly the model *learning this project's convention* and should be
described that way rather than as pure comprehension gain.

**2. It never extracts entities.** `entities` 0/68 and `asset` 0/21 — those
fields were emitted zero times. It does populate `location_hint` (134/160,
83.1%) and sometimes `affected_resources` (44/134). People counts, the field
an operator most needs, are absent entirely.

**3. It systematically under-calls urgency.** 80 predictions below the gold
severity against 21 above it. Under-calling is the dangerous direction in
this domain, and it is the single clearest argument for domain fine-tuning.

**4. Noise costs real accuracy.** Category accuracy is 50.0% on clean text
and 34.1% on typo-noised text — a 16-point drop attributable to surface
form alone. Long detailed reports scored 100% (10/10) and multi-message
sequences 74.4%, so the weakness is noise, not length or structure.

### A finding worth acting on independently of fine-tuning

Every one of the 27 schema-invalid outputs had the same cause: the model
emitted **`confidence: 50`**, reporting a percentage where the schema
specifies a 0.0–1.0 fraction. `RawAnalysis::validate` clamps rather than
rejects (`intelligence.rs:319-322`), so production accepts these — and
records a model that said "50% confident" as **1.0 confident**. That is a
silent overstatement of certainty in the operational record, and it exists
in the shipped system today. It is not caused by anything in this phase.

The 82.7% figure above is therefore against the *stricter* training-data
validator; measured against what production actually accepts, structured
output validity is 99.4%.

### Test-set discipline

Splits were verified before any evaluation: zero overlap of record ids,
`scenario_group`s, or report text between train, val and test. The test set
has been used exactly once, for this baseline. Nothing in the dataset was
regenerated or altered after seeing these numbers.

One caveat on reading per-category figures: the test split is 162 records
across 24 scenario groups, and group sizes differ, so per-category support
ranges from 26 (EARTHQUAKE, ROAD_BLOCKAGE) down to 2 (OTHER). A per-category
figure on single-digit support is indicative only.

## Training readiness audit (Phase 3A)

Three issues were audited before any training run. Two are defects in the
*shipped* system that this work happened to surface; both are documented here
as **proposed** changes and neither has been applied.

### Confidence is unbounded in the schema and clamped on the way in

| Layer | What it says about `confidence` |
|---|---|
| Production JSON schema (`ai/prompt.rs:197`) | `{"type": "number"}` — **no minimum, no maximum** |
| Production prompt text | **never mentions confidence at all** |
| `RawAnalysis::validate` (`intelligence.rs:319-322`) | clamps to `0.0..=1.0`; non-finite treated as absent |
| SQLite (`004_local_intelligence.sql:41`) | `CHECK (confidence BETWEEN 0.0 AND 1.0)` |
| UI (`IncidentAnalysis.tsx:129`) | renders `confidence * 100` as "N% (model's own estimate)" |
| Training dataset | **0 of 1,428 records carry a confidence value** |
| Training validator | requires `0.0..=1.0` (stricter than production) |

Measured on the baseline run: the model emitted `confidence` in 123 of 162
records and **used two scales at once** — 96 fractions (0.5, 0.9, 0.95, 1.0)
and 27 percentages (25, 50, 75, 80, 100).

**Why this is a production safety issue, not just untidiness.** Everything
above 1.0 clamps to exactly 1.0, so `25` — a model saying it is 25% confident
— is stored as `1.0` and displayed to an operator as **"100% (model's own
estimate)"**. A value of `0.5` and a value of `50` express the same belief and
land at opposite ends of the displayed scale. The least confident outputs are
presented as the most confident. Nothing gates on confidence, so this is a
misinformation-to-operator problem rather than a control-flow hazard, but this
project's own standard is that a figure shown to an operator must not overstate
what the system knows.

**Where it should be fixed.** The schema, structurally — the same principle
`EVALUATION.md` records for citations: a small model follows a *schema* far
more reliably than an *instruction*. Adding `"minimum": 0, "maximum": 1` to
the `confidence` property makes constrained decoding unable to emit `50`.
Clamping then becomes unreachable rather than load-bearing.

**Proposed production change — NOT APPLIED, requires approval:**

```rust
// src-tauri/src/ai/prompt.rs:197
"confidence": { "type": "number", "minimum": 0, "maximum": 1 }
```

Applying it changes base-model behaviour, so the Phase 3 baseline would have
to be re-measured before any comparison against a fine-tuned model.

**Training-side position: no confidence labels will be fabricated.** The
dataset deliberately carries none. Inventing a confidence figure for each of
1,428 records would be precisely the fabricated precision the Phase 2
validator exists to catch, and the "correct" confidence of a hand-authored
label is not a knowable quantity. Fine-tuning on this data therefore teaches
the model to *omit* the field, which is an honest outcome. `evaluate.py` now
reports confidence-scale violations as their own metric so the behaviour stays
visible instead of hiding inside a schema-validity percentage.

### access_status: the prompt and the dataset disagree

`AccessStatus` is documented in Rust as "how reachable the model **judged**
the affected area to be" — an inference, by its own definition. But:

- the four values are **never described to the model**. `category` gets a
  one-line gloss per value via `IncidentCategory::described_list()`;
  `access_status` is offered as a bare enum, which is exactly the state
  `category` was in when it scored 36.7%, before descriptions took it to
  70.0% (`EVALUATION.md`);
- the prompt says "Where the report does not say, omit the field or use the
  UNKNOWN value rather than guessing" — while `access_status` is in
  `required`, so UNKNOWN is the model's only way to abstain;
- the dataset labels it inferentially. Measured across all 1,428 records,
  the share whose label is explicitly stated in the report text is **5.7% of
  OPEN, 9.2% of RESTRICTED, 22.2% of BLOCKED**. The rest are judgements the
  author made from context.

So the baseline's 153-of-161 `UNKNOWN` is **the model obeying the prompt**,
and the dataset penalising it for doing so. Diagnosis: **D — a combination**,
dominated by (B) prompt convention mismatch, with (C) dataset convention a
real contributor and (A) genuine model weakness the smallest share.

**No labels were changed.** Relabelling after seeing baseline scores is the
practice Phase 3's own test-integrity rule forbids, and the held-out test set
is immutable — relabelling train and val alone would leave the model trained
on one convention and scored against another.

**Recommended convention, for a future dataset version:** keep inference (the
field is a derived, non-replicated opinion, and "judged" is its stated
meaning), but describe the values to the model so inference is guided rather
than guessed, and treat UNKNOWN as correct when a report genuinely says
nothing about reachability. Adopting it means regenerating the corpus
*including a fresh test split* and re-baselining — a deliberate Phase 4
decision, not an in-place edit.

**Proposed production change — NOT APPLIED, requires approval:** describe the
enum in `analysis_system_prompt()`, as categories already are. This would
likely move the base model's 13% substantially on its own, and must be
re-baselined if applied.

**Standing caveat:** until the conventions are reconciled, any access_status
gain from fine-tuning is **convention alignment, not comprehension**, and
must be reported as such.

### Noise transforms: two real defects, one latent

Tested by applying each style transform to fixed text (1,056 controlled
pairs), which isolates the transform from slot variation.

1. **Typos corrupt negation.** 25 of 1,056 pairs lost a negation cue to
   character-level noise: `nothing` became `nothign`, `nohting`, `nthing`,
   `noothings`, `notthing`. These land on LOW-severity records where
   "nothing serious" / "nothing needed" is the *entire reason* the label is
   LOW. The transform can therefore degrade the single most label-relevant
   token in a record. Fix: exempt negation and hedging words from typo
   corruption.
2. **One abbreviation is semantically wrong.** `situation` → `sitrep`.
   "sitrep" means *situation report*, so "Situation at the north camp is
   deteriorating" becomes "sitrep at the north camp is deteriorating", which
   says something different. Fix: remove the entry. Also `right now` → `rn`
   is dead code — the regex matches single words, so it never fires.
3. **Latent: `b4` injects spurious numeric support.** `supported_numbers()`
   scans digit runs, so abbreviating "before" to "b4" makes the validator
   believe the report stated the number 4, defeating the fabricated-precision
   check for that value. **No current record depends on this** (verified: 0),
   so it is a latent defect that would bite future data. Fix: normalise `b4`
   back to `before` before extracting supported numbers.

**None of these fixes have been applied**, because the generator writes
`incidents_v2_generated.jsonl`, which feeds the held-out test set. Applying
them regenerates the test set and invalidates the Phase 3 baseline. They
belong to a v3 corpus with a fresh baseline.

Noise composition is otherwise sound and consistent across splits (clean
38–43%, abbreviated 57–62%, noisy 27–36%). Two representativeness gaps are
worth knowing before reading any training curve:

- **val contains zero multi-message records**, so that capability cannot be
  monitored during training;
- **test is 24.7% multi-message against train's 5.3%** — the stratified split
  balances categories, not capability slices, and with only 24 groups in test
  one family swings a slice by several points.

Neither is a leak; both make small-slice test figures noisy.

### There are no controlled noise pairs

Slot values are reseeded per style, so the `noisy` variant of a scenario
differs from its `plain` sibling in **both** wording and content (different
`{loc}`, `{count}`). Of 480 plain/noisy sibling pairs, only 140 share most
content words. "Did fine-tuning improve noise robustness?" is therefore
confounded: the comparison mixes surface noise with different facts. A future
corpus should emit minimal pairs — identical slots, differing only in style —
for the noise slice specifically.

## Pipeline corrections (Phase 3B)

The Phase 3A audit found three defects in the training pipeline. All three
are fixed, and each is now covered by a test that fails if it regresses.
**No training has been run, no package installed, and the corpus and test set
are untouched.**

### 1. Loss is now computed on the assistant turn only

The original preprocessing set `labels = input_ids`, computing loss over the
system prompt and the report as well as the answer. Measured on a real
example: **266 prompt tokens against 31 target tokens** — roughly 90% of the
gradient would have gone into reproducing a system prompt that is
byte-identical in all 1,428 examples and handed to the model for free at
inference time.

Masking is by **token-count boundary**, not string search:

```
prompt_text = chat_template([system, user], add_generation_prompt=True)
full_text   = chat_template([system, user, assistant])
labels      = [-100] * len(encode(prompt_text)) + full_ids[len(prompt_ids):]
```

The boundary is exact because the generation prompt is a string prefix of the
full conversation under ChatML. That prefix relationship is **verified per
example** rather than assumed (`assert_prompt_is_prefix`): a tokenizer that
merged a token across the boundary would shift every label by one, which no
metric would reveal. If it ever fails, preprocessing stops.

### 2. The collator no longer discards the masking

`DataCollatorForLanguageModeling` overwrites `labels` with a clone of
`input_ids`. Under it, the masking above would have been computed and then
thrown away — the pipeline would have looked corrected while training exactly
as before. Replaced with `MaskedCausalCollator`, which pads three fields with
the values that make padding inert: `pad_token_id` for `input_ids`, `0` for
`attention_mask`, `-100` for `labels`.

### 3. The pad token is configured explicitly

Set from the tokenizer's EOS when absent. Safe here precisely because padding
is excluded from the loss, so reusing EOS as the pad id cannot teach the model
anything about where sequences end.

### Sequence length: 1024, now measured with the real tokenizer

`max_seq_length` moved from 2048 to **1024** — a memory decision (roughly
halving activations on ~6.8 GB of free VRAM), not an accuracy one.

**Measured, not estimated (Phase 3E).** The real Qwen2.5 tokenizer was read
out of the local production GGUF — no Hugging Face download — and run over
all 1,266 train and validation examples using the exact prompt construction
`finetune.py` uses:

| | train | val | combined |
|---|---:|---:|---:|
| Records | 1,052 | 214 | 1,266 |
| Max total tokens | 579 | 596 | **596** |
| p95 / p99 | 517 / 564 | 560 / 590 | 525 / 569 |
| Median / mean | 457 / 462 | 452 / 464 | 457 / 463 |
| Max prompt tokens | 445 | 451 | **451** |
| Max target tokens | 140 | 155 | **155** |
| Records over 1024 | 0 | 0 | **0** |

**1024 VERIFIED SAFE** — 428 tokens of headroom over the longest example
(`gen-detailed_power_status-l0-p0-noisy`, scenario group
`detailed_power_status`). The earlier character bound predicted ~694 tokens
at 3.5 chars/token; the real figure is 596, so the estimate erred in the safe
direction.

The prefix assertion and the masking invariants were re-checked on **every
one of the 1,266 records**, not a sample: 0 prefix failures, 0 masking
failures. Mean split is **379 prompt tokens masked against 83 supervised** —
so without the Phase 3B fix roughly 82% of the loss would have gone into
reproducing the system prompt, now measured rather than inferred.

Nothing relies on the measurement staying true: `measure_corpus` re-tokenizes
the corpus before training and `finetune.py` **refuses to start** if any
example exceeds the limit, naming the offenders. Truncation is never silent.

### Reading the tokenizer from the GGUF

`gguf_tokenizer.py` loads the Qwen2.5 tokenizer from
`ai/models/llm/qwen2.5-1.5b-instruct-q4_k_m.gguf`, **read-only**. This is how
a real measurement was possible before the HF weights exist on the machine:
the tokenizer is already present, embedded in the file the running
application serves.

It is verified rather than trusted. The GGUF's SHA-256 is checked against
`ai/models/README.md` (match confirmed), so the tokenizer provably comes from
the same model the Phase 3 baseline was measured on; the loader then refuses
anything whose vocabulary is under 150,000, whose ChatML markers are not
single tokens, or which has no chat template. Loaded:
`Qwen2Tokenizer`, vocab 151,936, `<|im_start|>` 151644 / `<|im_end|>` 151645
/ `<|endoftext|>` 151643, with the authentic Qwen chat template carried
through by the transformers GGUF loader. Its own pad token
(`<|endoftext|>`) was already defined, so the EOS fallback was not needed.

### Tests

`python training/scripts/test_preprocessing.py` — **31 assertions, all
passing**, with no installed packages. A stub ChatML tokenizer drives the
same functions the real run calls, covering: prompt masking (A), target-only
loss (B), pad token and inert padding (C), refusal to truncate (D),
deterministic preprocessing (E), dataset version and frozen split counts (F),
and test-set exclusion (G).

The suite includes a deliberately broken tokenizer that merges across the
prompt boundary, to prove the boundary check fires. An earlier version of that
stub perturbed prompt and target identically, left the prefix intact, and
passed — it was corrected so the test actually tests something.

What the stub **cannot** prove is real BPE token counts, which is why length
is enforced at training time instead of claimed here.

`python training/scripts/dryrun_preprocessing.py` loads the configured
dataset, verifies the boundary, masking, padding and test-set exclusion, and
reports whether tokenization could be executed. It currently reports that it
could not, and says so rather than implying the lengths are verified.

### One duplication removed

`finetune.py` carried its own copy of the production prompt while
`run_baseline.py` used `securemesh_prompt.py`. They were byte-identical when
checked, but two mirrors of one prompt can drift — and had they drifted, the
fine-tuned model would have been trained on a different task from the one the
baseline measured, with nothing to reveal it. There is now one copy.

---

## Base model acquired, QLoRA initialization verified (Phase 3F)

The HF-format `Qwen/Qwen2.5-1.5B-Instruct` was fetched into
`training/models/Qwen2.5-1.5B-Instruct/` — **2.886 GiB, 7 files, one
safetensors shard** (`model.safetensors`, 2.875 GiB, bf16, Qwen2ForCausalLM,
28 layers, vocab 151,936).

**Training model and production model are separate artefacts by
construction.** The quantised GGUF under `ai/models/llm/` is what the running
application serves; it is an inference format and cannot be fine-tuned.
`fetch_base_model.py` refuses any destination inside `ai/models/` rather than
relying on convention. The production GGUF was re-hashed after this phase and
is byte-identical.

### The HF tokenizer matches the GGUF tokenizer exactly

Two surface differences appeared and both are immaterial, verified rather
than waved away:

- **Vocab 151,665 (HF) vs 151,936 (GGUF).** 151,936 is the *padded embedding
  matrix* size from `config.json`; 151,665 is the count of real vocabulary
  entries. The 271-row difference is embedding padding. Both agree on the
  number that matters.
- **Chat template hashes differ** (`cd8e9439…` vs `d5495a1e…`). The template
  *text* differs in tool-calling branches this pipeline never triggers.

What settles it is output, not metadata: across **all 1,266 train and
validation records**, the two tokenizers produced **0 differing token ids, 0
differing prompt boundaries, and 0 differing target lengths**. Special token
ids are identical (`<|im_start|>` 151644, `<|im_end|>` 151645,
`<|endoftext|>` 151643) and the rendered generation prompt is byte-identical.

Re-measuring with the HF tokenizer reproduced Phase 3E exactly — max **596**
tokens, p95 525, p99 569, max prompt 451, max target 155, **0 records over
1024**, same longest example. `max_seq_length = 1024` stands.

### QLoRA initialization smoke test

One load, one forward, one backward. No optimizer step, no epoch, nothing
saved, every hyperparameter taken from the config unchanged.

| | |
|---|---|
| Quantization | 4-bit NF4, bf16 compute — **196 `Linear4bit` modules**, so the weights really are 4-bit |
| LoRA | r=16, α=32, dropout=0.05, bias=none, CAUSAL_LM, seed 42 |
| Target modules adapted | all 7 requested (`q_proj`, `k_proj`, `v_proj`, `o_proj`, `gate_proj`, `up_proj`, `down_proj`) across **196 layer instances** |
| Total / trainable params | 907,081,216 / **18,464,768 (2.0356%)** |
| Forward | loss **1.9504** (finite), logits bf16 on `cuda:0`, 969 ms |
| Backward | **392/392** trainable params received gradients; **0 base params did** — LoRA isolation confirmed, not assumed. Grad norm 2.7755 |
| VRAM | 1.131 GiB after base model, peak **4.848 GiB** allocated / 5.178 GiB reserved, 1.587 GiB free at the end |

### Batch size 2 does not fit in physical VRAM (Phase 3G)

The batch-1 figures above were measured with `per_device_train_batch_size: 2`
in the config, so batch 2 was tested directly, using the two longest training
records by **token** count (579 and 577 tokens).

It **did not raise `OutOfMemoryError`** — and it still does not fit:

| | batch 1 | batch 2 | ratio |
|---|---:|---:|---:|
| Peak allocated | 4.848 GiB | **8.754 GiB** | 1.81× |
| Peak reserved | 5.178 GiB | **9.316 GiB** | 1.80× |
| Forward latency | 969 ms | 1,259 ms | 1.30× |
| **Forward + backward** | **386 ms** | **2,634 ms** | **6.82×** |
| Loss | 1.9504 | 1.9731 | — |

Physical VRAM is **7.960 GiB**. Batch 2 peaked **1.356 GiB above it**.

That is arithmetically impossible on the device alone, and the explanation is
the driver model: the GPU runs under **WDDM**, where NVIDIA spills past
dedicated VRAM into shared system memory over PCIe rather than failing. The
computation stays correct — finite loss, 392/392 LoRA gradients, 0 base-model
gradients — but memory scaled 1.8× while backward latency scaled **6.8×**,
which is the signature of host-memory spill rather than of a larger batch.

**"No OOM" was therefore not the same as "fits".** The smoke test originally
reported `PASSED` for this run, because it only caught exceptions. It now
compares peak against physical VRAM and reports
`COMPLETED WITHOUT OOM, BUT DOES NOT FIT IN PHYSICAL VRAM` — a defect in the
instrument, found by disbelieving its own verdict.

### Resolution: batch 1 × accumulation 16 (Phase 3H)

The microbatch/accumulation split moved from **2 × 8** to **1 × 16**. Nothing
else changed — effective batch size, epochs, learning rate, scheduler, LoRA
rank, alpha, dropout, target modules, quantization, bf16, sequence length,
seed and dataset are all untouched.

**The schedule is provably identical.** From `trainer.py`,
`num_update_steps_per_epoch = max(len_dl // gas + int(len_dl % gas > 0), 1)`:

| | len(dataloader) | gas | steps/epoch | max_steps | effective batch | warmup |
|---|---:|---:|---:|---:|---:|---:|
| batch 2 × accum 8 | 526 | 8 | 66 | 198 | 16 | 6 |
| **batch 1 × accum 16** | 1052 | 16 | **66** | **198** | **16** | **6** |

The cosine schedule traverses exactly the same 198 optimizer steps either
way, so learning dynamics are unaffected.

**Measured on the true longest sequence** (`gen-detailed_power_status-l0-p0-noisy`,
596 tokens — which lives in *val*, and reaches the GPU because
`eval_strategy: epoch`):

| | batch 2 × 8 | **batch 1 × 16** |
|---|---:|---:|
| Peak allocated | 8.754 GiB | **4.987 GiB** |
| Peak reserved | 9.316 GiB | **5.436 GiB** |
| vs 7.96 GiB physical | **1.356 GiB over** | **2.524 GiB spare** |
| Forward + backward | 2,634 ms | **343 ms** |

Loss 2.1476 finite, 392/392 LoRA gradients, 0 base-model gradients.

### Accumulation semantics verified

Checked two ways, without running training.

**Static**, read from the installed `transformers` source: `optimizer.step()`
(L1793), `lr_scheduler.step()` (L1802) and `model.zero_grad()` (L1804) all sit
inside the same `if do_sync_step:` guard, so none of them fires per
microbatch; and `training_step` applies
`loss = loss / self.current_gradient_accumulation_steps`.

**Empirical**, 16 microbatches by hand with no optimizer: gradient norm rose
0.294 → 0.549 → 1.801 → 2.978 across the 16 steps, never resetting — direct
evidence that gradients accumulate rather than being cleared. A LoRA weight
snapshotted before and after was bit-identical, confirming nothing was
updated.

**A note on how that was validated.** The first comparison — accumulated
gradients versus the same microbatches computed individually and averaged —
"failed" at 7.6e-2 relative difference against a 1e-3 tolerance. The
tolerance was the problem, not the code: running the *identical* computation
twice already differs by 6.9e-2, because 4-bit matmul and bf16 reductions are
not bitwise reproducible. The check now measures that noise floor and asks
whether the two methods differ by more than the hardware differs from itself.
They do not — **1.40× the noise floor**, where a genuine semantic error would
show up as orders of magnitude. The fix was to measure the floor, not to
loosen the threshold until it passed.

## First training run: complete, and it overfits (Phase 4)

`securemesh-slm-v2-qlora-r1` ran to completion — 198/198 optimizer steps, 3
epochs, 906 s, 0 truncated examples, test split never loaded. The adapter
loads and produces schema-valid output.

**It also overfits, clearly, and that is the headline finding.**

| Epoch | train loss | **eval loss** |
|---|---:|---:|
| 1 | ~0.35 | **0.6222** ← best |
| 2 | ~0.078 | 0.8364 |
| 3 | final 0.2956 (run mean) | **0.9070** ← worst |

Validation loss rose monotonically across all three epochs while training
loss collapsed by roughly 4.5×. The model is memorising 1,052 records.

**Consequence worth knowing before anything is evaluated:**
`load_best_model_at_end` is not set, so the adapter written to the top level
of the run directory is the **epoch-3** weights — the *worst* of the three by
validation loss. The epoch-1 checkpoint survives as `checkpoint-66`, and
`checkpoint-132` / `checkpoint-198` alongside it. Evaluating "the adapter"
without saying which one would silently evaluate the most overfit version.

No hyperparameter was changed in response, and the test set remains
untouched. The obvious candidate fixes — fewer epochs, early stopping on
validation loss, more data — are all decisions for a subsequent run, not
adjustments to this one.

### Two defects found during the run

**`warmup_ratio` no longer exists.** The first launch died at
`TrainingArguments` construction: transformers 5.x removed the parameter,
leaving only `warmup_steps`. The failure was clean — before any model write,
so no partial adapter and no touched data. The configured ratio is now
translated with the same formula the removed implementation used,
`ceil(total_steps * ratio)` = `ceil(198 * 0.03)` = **6 steps**, so the
experiment is unchanged. Both the ratio and the applied step count are
recorded in the manifest.

**`per_device_eval_batch_size` defaults to 8.** Caught in pre-flight rather
than at the first epoch boundary. With `eval_strategy: epoch` the default
would have forwarded eight sequences of up to 596 tokens at once on a device
where batch 2 already overshot physical VRAM. Set explicitly to 1; evaluation
is forward-only, so this is memory, not learning.

### Memory: the smoke test was optimistic

| | smoke test (batch 1) | actual run |
|---|---:|---:|
| Peak allocated | 4.987 GiB | 4.422 GiB |
| Peak **reserved** | 5.436 GiB | **10.654 GiB** |
| vs 7.96 GiB physical | fits | **2.7 GiB over** |

Allocated memory came in *below* the smoke-test prediction, but **reserved
memory exceeded physical VRAM by 2.7 GiB**, so the caching allocator spilled
into shared system memory during the real run despite batch 1 fitting in
isolation.

A reserved-to-allocated ratio of 2.4× is fragmentation, not working set: the
run pushed 3,168 microbatches and 642 evaluation forwards at sequence lengths
from 48 to 596 tokens, and the allocator accumulated cached blocks per size
class. Step time drifting from 4.0 s to 4.8 s is consistent with it.

The smoke test could not have caught this — it measured one forward and one
backward with no optimizer, no evaluation, and no opportunity to fragment.
That is a limitation of the instrument, worth stating rather than treating
the earlier "fits with 2.5 GiB to spare" as having been wrong.

`PYTORCH_CUDA_ALLOC_CONF=expandable_segments:True` is the standard mitigation
and changes no hyperparameter, but it was **not** applied: the run was
already frozen, and it belongs to the next one.

## Frozen test evaluation protocol (Phase 5) — fixed before any test record was read

This section was written while the test evaluation was still running and
before any test result existed, so the protocol is on record ahead of the
outcome.

### Which adapter

**`checkpoint-66` only** — the epoch-1 checkpoint, selected on validation loss
(0.6222, the lowest of the three epochs) before the test set was touched.
`checkpoint-132`, `checkpoint-198` and the top-level adapter are not
evaluated, and no test result is compared across epochs.

### Why the recorded baseline could not serve as the control

The Phase 3 baseline was measured with **Q4_K_M quantisation, llama.cpp on
CPU, and `json_schema` grammar-constrained decoding**. The adapter cannot be
measured that way: llama.cpp does not load a PEFT LoRA, and GGUF export is out
of scope. It runs through transformers — NF4, GPU. Comparing the two would
attribute three runtime differences to the fine-tuning.

So both arms run on the identical HF path, and the recorded GGUF figures are
shown for reference only.

### Why grammar-constrained decoding on both arms

Measured on 20 **validation** records, with the unchanged evaluator:

| Condition | schema | category | severity | access | all 4 required fields |
|---|---:|---:|---:|---:|---:|
| HF base, unconstrained | 0% | 50% | 0% | 0% | 0/20 |
| GGUF base, grammar (production decoding) | 95% | 65% | 30% | 30% | 20/20 |
| HF + checkpoint-66, unconstrained | 100% | 95% | 70% | 75% | 20/20 |

Unconstrained, the base model writes `{"category": ...}` and stops, so it
scores 0% on severity and access because nothing forces it to write those
fields. That control would have credited the adapter with a large gain that
is really "learned to write complete JSON" — something production's grammar
already gives the base model for free. Production always decodes against
`ai/prompt.rs::analysis_schema()`, so the control should too.

The protocol was chosen by the project owner from that validation evidence,
with the test set still untouched.

### The protocol, identical for both arms

| | |
|---|---|
| Base weights | `training/models/Qwen2.5-1.5B-Instruct`, NF4 4-bit, bf16 compute |
| Adapter | none (control) / `checkpoint-66` (treatment) |
| Prompt | `securemesh_prompt.py`, the single mirror of `ai/prompt.rs` |
| Decoding | greedy, `repetition_penalty` 1.0, `max_new_tokens` 512, stop on `<|im_end|>` / `<|endoftext|>` |
| Constraint | `analysis_schema()` via lm-format-enforcer 0.11.3 |
| Scoring | `evaluate.py` and `error_analysis.py`, unchanged |
| Test set | 162 records, SHA-256 verified before the first record is read |

Two decoding details were made explicit rather than inherited. The model's
`generation_config.json` ships sampling defaults and `repetition_penalty` 1.1,
which `generate()` applies silently; a repetition penalty penalises repeated
quotes, braces and field names, which is to say it penalises JSON.

lm-format-enforcer's transformers integration does not import under
transformers 5.x — it imports `PreTrainedTokenizerBase` from a module path that
no longer exists, and misreports that as "transformers is not installed".
0.11.3 is the newest release. The two functions evaluation needs are mirrored
in `training/scripts/grammar_adapter.py` on the library's working core with
only that import corrected, rather than downgrading transformers or editing
site-packages.

## Frozen test result (Phase 5)

One evaluation of the pre-selected `checkpoint-66`, against a control re-measured
on the identical path. Test SHA-256 verified before and after; no other
checkpoint evaluated; nothing tuned on the result.

| Metric | Control (base + grammar) | **checkpoint-66 + grammar** | Δ pp | 95% CI (cluster bootstrap) | Recorded GGUF (ref only) |
|---|---:|---:|---:|---|---:|
| JSON parse | 94.4% | **100.0%** | +5.6 | — | 99.4% |
| Schema accepted | 24.1% | **100.0%** | +75.9 | — | 82.7% |
| Category | 47.1% | **78.4%** | +31.3 | [+14.7, +46.7] | 47.2% |
| Severity | 35.3% | **58.6%** | +23.3 | [+6.9, +39.4] | 37.3% |
| Access status | 11.8% | **74.7%** | +62.9 | [+51.7, +73.9] | 13.0% |
| Keyword recall | 41.7% | **50.9%** | +9.2 | — | 41.7% |
| Fully correct | 0.0% | **33.3%** | +33.3 | [+24.8, +43.0] | 0.6% |

Confidence intervals resample the 24 scenario groups rather than the 162
records, because records within a group are correlated. All four intervals
exclude zero.

The control reproduces the recorded GGUF baseline on content (category 47.1 vs
47.2, keyword recall identical), which is evidence the HF grammar path is a
faithful stand-in for production decoding.

### What the numbers do not mean

- **Schema acceptance is mostly the confidence bug.** 114 of the control's
  failures had exactly one error — `confidence` outside 0–1 — and 9 were
  `summary` strings that never closed before the 512-token limit. The adapter
  emits no `confidence` at all, because the training data carries none. The
  +75.9pp is a formatting effect, not comprehension. "Fully correct" is not
  affected: ignoring confidence-only failures still leaves the control at 0.0%.
- **Access status is largely convention alignment.** The control answered
  `UNKNOWN` for every OPEN record, exactly as the prompt instructs; the adapter
  learned the dataset's inferential convention. See the Phase 3A note below.
- **The test set is in-distribution synthetic data.** No scenario group is
  shared with training, but authoring style, slot vocabulary and noise
  transforms are. These figures are an upper bound for real field reports and
  are not evidence about them.

### Where it got worse

- **CRITICAL is still badly under-called**: 34.8%, with 30 of 46 CRITICAL
  records predicted HIGH. The collapse toward LOW is gone (under-calls 80 → 48,
  MEDIUM→LOW 39 → 9), but a systematic CRITICAL→HIGH downgrade replaced it. In
  this domain that is the dangerous direction.
- **MEDICAL −16.7pp and long reports −30pp** are one scenario group:
  all three `detailed_medical_scene` records (a wall collapse with casualties)
  flipped to FIRE.
- **Asset extraction fell** from 8 to 5 real values out of 21. The raw
  presence figure (61.9% → 23.8%) overstates the drop, because 5 of the
  control's hits were the placeholder string `"UNKNOWN"`.
- **Severity on conflicting reports did not move** (31.2% both arms).

Per-category figures rest on one to three scenario groups each and should be
read as anecdotes, not rates.

**The test set has now been used.** A subsequent training run must not be
tuned against these numbers; a new held-out set is needed before any claim of
a second improvement.

## Known production issue requiring separate approval and re-baselining

**The `confidence` field is unbounded in the schema and silently clamped on
the way in.** `ai/prompt.rs:197` declares `{"type": "number"}` with no
minimum or maximum, no prompt text explains the range, and
`RawAnalysis::validate` clamps rather than rejects. A model answering `25` —
meaning 25% confident — is stored as `1.0` and displayed by
`IncidentAnalysis.tsx:129` as "100% (model's own estimate)". Measured on the
baseline: 22.0% of emitted confidences used a percentage scale.

**Not fixed, and not worked around.** The remedy is a bound in the production
JSON schema, which changes base-model behaviour and therefore invalidates the
Phase 3 baseline. It needs explicit approval and a re-baseline.

**It will not be papered over with training data.** The dataset carries no
confidence values and none will be fabricated: inventing a figure per record
is exactly the fabricated precision the Phase 2 validator exists to catch, and
the "correct" confidence of a hand-authored label is not a knowable quantity.
`evaluate.py` reports confidence-scale violations as their own metric so the
behaviour stays visible.

## Known convention mismatch: access_status

**`access_status` currently has a prompt/dataset convention mismatch, and any
fine-tuning gain on it must be interpreted carefully.**

The prompt instructs the model to "use the UNKNOWN value rather than
guessing" and never describes what the four values mean; the dataset labels
the field inferentially, with only 5.7% of `OPEN`, 9.2% of `RESTRICTED` and
22.2% of `BLOCKED` labels explicitly stated in the report text. The baseline's
153-of-161 `UNKNOWN` is largely the model obeying the prompt.

**No labels were changed and the corpus was not regenerated.** Relabelling
after seeing baseline scores is what the test-integrity rule forbids, and the
test set is frozen. An improvement here after fine-tuning would be **the model
learning this project's convention, not the model understanding reports
better**, and must be reported that way.

## Exact next step

Fine-tuning is **blocked on this machine**, for two independent reasons
established by the Phase 3 Step 0 environment check:

**1. No training stack is installed.** `torch`, `transformers`, `peft`,
`pyyaml`, `bitsandbytes`, `accelerate` and `datasets` are all absent. The
only interpreter present is **CPython 3.14.7**, and whether PyTorch
publishes wheels for 3.14 has not been verified here — checking would
require network access this phase forbids. If it does not, a second
interpreter (3.11 or 3.12) is needed before anything else.

**2. There is no trainable copy of the model.** `ai/models/llm/` holds the
**GGUF**, which is a quantised llama.cpp *inference* format — LoRA cannot be
applied to it. Fine-tuning needs the original Hugging Face weights, and the
local HF cache contains only `all-MiniLM-L6-v2`. `lora_config.yaml`'s
`base_model.path` is still the unset placeholder `PATH_TO_LOCAL/…`.

The hardware itself is adequate: an RTX 5050 Laptop GPU with 8 151 MiB
(6 927 MiB free) comfortably fits QLoRA on a 1.5B model, alongside 23 GB
system RAM and 8 cores.

To unblock, in order:

1. Install a Python 3.11 or 3.12 interpreter if torch does not support 3.14.
2. Install `torch`, `transformers`, `peft`, `pyyaml`, `bitsandbytes`,
   `accelerate` into it.
3. Obtain a local HF-format checkout of Qwen2.5-1.5B-Instruct (bf16 or
   full precision — **not** the `.gguf`) by whatever means is already used
   on this machine for open-weight models, and point
   `lora_config.yaml`'s `base_model.path` at it.
4. Run `python training/scripts/finetune.py
   --i-have-configured-a-local-training-environment`.
5. Re-run `run_baseline.py` against the exported candidate and compare with
   `training/results/baseline-test-metrics.json`.

One config change is recommended before training, on memory grounds rather
than to move a metric: `max_seq_length` is 2048 to match
`LlamaConfig::generation()`'s context, but the longest training example is
~694 tokens. 1024 would halve activation memory with nothing truncated.

**This document's own stop condition, stated the way
`PLATFORM_EVALUATION.md` and `ROADMAP.md` state theirs:** no model has been
trained, no adapter merged, no GGUF exported, and no file under
`ai/models/`, `src-tauri/`, or `src/` changed. The baseline was produced by
*reading* the provisioned model, and the test set has been consumed exactly
once.
