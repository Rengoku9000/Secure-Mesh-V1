# SecureMesh-SLM — Training Pipeline

**Phase 2 status: dataset expanded to 1,428 validated examples. No
fine-tuning run has happened. No model has been changed.**

This directory specializes the **existing** local model
(Qwen2.5-1.5B-Instruct, already provisioned and evaluated — see
`ai/models/README.md` and `docs/ai/EVALUATION.md`) through offline LoRA/QLoRA
fine-tuning. It does not add a second AI architecture, a second schema, or a
second runtime. Everything downstream of a trained model —
`LocalInferenceEngine`, `LlamaServerEngine`, `IntelligenceService`,
`InferenceGate`, the prompt/schema validation in `ai/prompt.rs` and
`domain/intelligence.rs`, and the RAG system — is untouched and stays
untouched by design: this directory produces a `.gguf` file, nothing more.

```
training/
├── data/
│   ├── raw/          incidents_v1.jsonl      60 hand-authored examples (Phase 1)
│   │                 incidents_v2_generated.jsonl  1,368 from authored families
│   └── processed/    train/val/test splits + manifest.json + dataset_stats.json
├── scripts/
│   ├── scenarios/           authored scenario families — the actual source content
│   │   ├── schema.py          what a family and an evidence level are
│   │   ├── hazard.py          FIRE, FLOODING, SEVERE_WEATHER, EARTHQUAKE
│   │   ├── people.py          MEDICAL, EVACUATION, OTHER
│   │   ├── infra.py           INFRASTRUCTURE, POWER, COMMUNICATIONS,
│   │   │                      ROAD_BLOCKAGE, RESOURCE_SHORTAGE
│   │   └── detailed.py        long, multi-entity, mixed-quality reports
│   ├── generate_dataset.py  expands families into records (deterministic)
│   ├── build_dataset.py     validates, splits by scenario_group, checks leakage
│   ├── validate_dataset.py  the schema mirror + corpus quality checks
│   ├── dataset_stats.py     distributions and capability coverage
│   ├── finetune.py          LoRA/QLoRA training (scaffold; not yet run)
│   ├── export_gguf.py       merge adapter -> GGUF -> quantize (scaffold; not run)
│   └── evaluate.py          offline scoring of a predictions file against gold
├── configs/
│   └── lora_config.yaml     hyperparameters, target modules, data file paths
└── README.md          this file
```

Nothing in this directory is imported by `src-tauri` or `src/`. It is not
part of the Tauri build, and running any script here cannot affect the
running SecureMesh application.

---

## The schema is not invented here — it is copied from Rust and cited

Every training record's `target` object must be exactly what
`RawAnalysis::validate()` in `src-tauri/src/domain/intelligence.rs` accepts,
because that is what the running application accepts from a real model. The
source of truth is:

| What | Where |
|---|---|
| Category enum (12 values) | `IncidentCategory::ALL`, `intelligence.rs:69-99` |
| Severity enum (4 values) | `Severity::ALL`, `src-tauri/src/domain/incident.rs:28-42` |
| Access status enum (4 values) | `AccessStatus::schema_values`, `intelligence.rs:204-209` |
| The optional-field set and required fields | `RawAnalysis`, `intelligence.rs:239-254`, validated `intelligence.rs:270-327` |
| Field length bounds (`MAX_SUMMARY_CHARS`, `MAX_FIELD_CHARS`, `MAX_ENTITIES`) | `intelligence.rs:41-44` |
| Report text length bound (`MAX_INCIDENT_CHARS`) | `src-tauri/src/ai/prompt.rs:31` |
| The system prompt shape | `analysis_system_prompt()`, `ai/prompt.rs:46-67` |

`training/scripts/validate_dataset.py` is the Python mirror of the first six
rows, with each constant and enum citing the exact Rust line it copies.
`training/scripts/finetune.py` mirrors the seventh (`SYSTEM_PROMPT_TEMPLATE`)
so a fine-tuned model is trained on the same instructions the runtime
actually sends it.

### Keeping this in sync with prompt.rs and intelligence.rs

There is no automated check that these stay in sync — Python cannot import
Rust, and adding a code-generation step was judged out of scope for Phase 1.
**Whenever any of the following changes in `src-tauri/src/ai/prompt.rs` or
`src-tauri/src/domain/intelligence.rs`, update the corresponding constant in
`validate_dataset.py` and `finetune.py` by hand:**

- `IncidentCategory::ALL` gains, loses, or renames a variant
- `AccessStatus` or `Severity` variants change
- `RawAnalysis`'s field set changes, or a field's required-ness changes
- `MAX_SUMMARY_CHARS`, `MAX_FIELD_CHARS`, `MAX_ENTITIES`, or `MAX_INCIDENT_CHARS` change
- `analysis_system_prompt()`'s wording changes

A drift here is silent and dangerous in one specific direction: it would let
`validate_dataset.py` accept a training record the real Rust validator would
reject, which would only surface once a fine-tuned model started producing
output the running application refuses. Re-running
`validate_dataset.py` after any of the above changes, against the current
`data/raw/*.jsonl`, is the cheapest check available.

---

## Provenance is load-bearing

Every record carries a `provenance` field: `synthetic`, `real`, or
`operator_corrected`.

- **`synthetic`** — hand-authored, or expanded from hand-authored scenarios,
  and not describing a real event. This is all 1,428 records today: 60
  written directly and 1,368 expanded from 108 authored families. They are
  modelled on plausible field phrasing and describe no actual incident,
  place, or person; place names are generic by design ("Sector 4", "the north
  camp"). `validate_dataset.py` additionally refuses a `synthetic` record
  whose text claims a real authority (NDMA, NDRF, FEMA, IFRC, etc.) — the
  same rule already applied to the operational knowledge pack
  (`docs/architecture/ARCHITECTURE.md` §6d.4).
- **`real`** — an actual field report, or text derived from one. Requires a
  `source` field stating where it came from. None exist yet.
- **`operator_corrected`** — a model's analysis that an operator corrected.
  SecureMesh already stores both the model's severity and the operator's
  side by side (`ai/service.rs`, `docs/architecture/ARCHITECTURE.md` §6.3) —
  that disagreement is the natural supervision signal for this category, but
  no mechanism to export it exists yet. Also requires `source`.

`synthetic` data must never be represented as real, and never will pass
validation with any of the disallowed-authority phrases. This is enforced in
`validate_dataset.py`, not left to discipline.

---

## How the dataset is built: authored families, controlled variation

The dataset is not paraphrases of a seed set, and not free generation. Every
record traces to a hand-written **scenario family** in `scripts/scenarios/`,
which carries several **evidence levels** — the same underlying situation
established to different degrees:

```
"burning smell near the store, nothing visible"   -> FIRE, LOW
"visible smoke from the store, area cleared"      -> FIRE, HIGH
"confirmed fire, flames through the roof, 4 in"   -> FIRE, CRITICAL
```

Severity moves because the evidence moves. That is the whole point: the
Phase 1 corpus let severity be guessed from category alone **48.3%** of the
time (MEDICAL was 3/5 CRITICAL, OTHER 4/5 LOW), so a model could score well
while ignoring the report. The expanded corpus brings that to **31.9%**
against a 25% chance floor, measured by `dataset_stats.py` on every run.

Slot fills (`{loc}`, `{count}`, `{asset}`) and style transforms
(abbreviation, typos, clipping, informal markers) vary the surface only. A
generated record can never carry a label its family did not author, so noise
is safe to add in bulk — a typo cannot silently relabel an example.

**To extend the dataset, author a family** in the appropriate
`scripts/scenarios/` module. Do not add near-paraphrases of existing
families: `build_dataset.py` will reject cross-group near-duplicates.

## Running the pipeline

All commands assume a working directory at the repository root, and a local
Python 3.10+ with **no packages beyond the standard library** for everything
except `finetune.py` and `export_gguf.py`.

### 1. Generate records from the authored families

```
python training/scripts/generate_dataset.py
```

Writes `training/data/raw/incidents_v2_generated.jsonl`. Deterministic: the
same authored families always produce a byte-identical file, so a diff means
the *scenarios* changed. Currently **108 families, 264 evidence levels,
1,368 records**.

### 2. Validate

```
python training/scripts/validate_dataset.py training/data/raw/*.jsonl --corpus-checks
```

Per-record checks enforce the Rust schema (above). `--corpus-checks` adds the
checks that need the whole corpus: exact duplicates, cross-group
near-duplicates, shared wording across families, label imbalance, and whether
severity is predictable from category. `--strict` turns warnings into
failures.

Currently: **1,428/1,428 valid, 0 errors, 1 warning** (see "Known accepted
warning" below).

### 3. Build train/val/test splits

```
python training/scripts/build_dataset.py
```

Re-validates everything (a build cannot proceed on invalid data), assigns
whole `scenario_group`s to splits **stratified by category**, then verifies
separation two ways before writing anything:

- no scenario_group spans two splits;
- no held-out report is a near-duplicate of any training report
  (Jaccard ≥ 0.80 over word shingles, checked for all 376 held-out records
  against all 1,052 training records).

Current result: **1,052 train / 214 val / 162 test**, 120/24/24 scenario
groups, all 12 categories and all 4 severities present in every split, zero
leakage of either kind.

Stratification is not cosmetic: a flat hash bucket left the test split with
no MEDICAL and no OTHER records at all, which would have made the test score
silent about two of the twelve categories.

### 4. Statistics

```
python training/scripts/dataset_stats.py --json training/data/processed/dataset_stats.json
```

Reports distributions, scenario-group structure, report-length bands,
capability coverage, and the category→severity shortcut score.

### Known accepted warning

`access_status=UNKNOWN: 162 records (11.3%), below 12.5% of the corpus`

This is reported and deliberately not "fixed". The imbalance check compares
each label against an even share of the corpus, which is a reasonable default
for category and severity but a poor model for access status — `OPEN`,
`RESTRICTED`, `BLOCKED` and `UNKNOWN` are not equally likely in real field
reporting, and most reports do establish access one way or another. Forcing
`UNKNOWN` up to 25% would misrepresent the domain to make a threshold go
green. The warning stays visible instead.

### 5. Fine-tune (NOT yet run)

```
python training/scripts/finetune.py --i-have-configured-a-local-training-environment
```

Requires, locally and beforehand — **nothing is downloaded by this script**:

- `torch`, `transformers`, `peft`, `pyyaml` (and `bitsandbytes` for QLoRA)
- A local HF-format checkout of Qwen2.5-1.5B-Instruct (full precision or
  bf16 — **not** the `.gguf` SecureMesh runs; that's a different format for a
  different purpose) at the path set in `configs/lora_config.yaml`

Without the `--i-have-configured-a-local-training-environment` flag, the
script only checks these preconditions and reports what's missing — it does
not train. This is deliberate: no fine-tuning happens until an operator has
explicitly confirmed the environment is ready.

### 6. Export to GGUF (NOT yet run)

```
python training/scripts/export_gguf.py \
  --base-model-path <path> --adapter-dir <path> \
  --llama-cpp-dir <path> --output-path training/artifacts/securemesh-slm-v1.gguf
```

Requires a local llama.cpp checkout with `convert_hf_to_gguf.py` and
`llama-quantize` — the same tooling family already used to provision the
production model (`ai/models/README.md`), obtained the same way: never
downloaded by this script. `--check-only` verifies every required local file
is present without merging, converting, or quantizing anything.

### 7. Evaluate

```
python training/scripts/evaluate.py \
  --gold training/data/processed/test.jsonl \
  --predictions <your predictions file>
```

Fully offline and runnable today (see `docs/ai/FINETUNING.md` for a worked
example). Scores category/severity/access-status accuracy, schema-valid
rate, and keyword recall — the same metric families as
`docs/ai/EVALUATION.md` — from a predictions file you produce by any means.
For latency, RAM, and the full RAG-grounding measurement, the existing
`cargo run --example run_benchmark` harness in `src-tauri/` remains
authoritative once a candidate model is actually provisioned; this script
does not replace it and does not invoke it.

---

## What this pipeline will never do

- Download a model, dataset, or tool automatically.
- Call a cloud API of any kind (OpenAI, Gemini, Claude, or otherwise).
- Modify `ai/models/llm/qwen2.5-1.5b-instruct-q4_k_m.gguf` or any other file
  the running application reads.
- Modify `LlamaConfig`, any file under `src-tauri/src/ai/`,
  `src-tauri/src/networking/`, `src-tauri/src/sync/`,
  `src-tauri/src/identity/`, `src-tauri/src/security/`, or the frontend.
- Represent synthetic data as real, or real data without recording where it
  came from.

Deploying a trained model — copying a `.gguf` into `ai/models/llm/`,
updating `LlamaConfig`, re-running the Rust evaluation harness, deciding
whether the numbers justify it — is a separate, later, human decision. This
pipeline produces the file and the evidence; it does not act on either.
