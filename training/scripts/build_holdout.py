#!/usr/bin/env python3
"""Builds the Phase 8 fresh held-out evaluation set.

# Why a new set rather than a new split

Every one of the 168 scenario groups in the corpus is already allocated — 120
to train, 24 to val, 24 to test. The model learned from train, checkpoint-66
was selected on val, and test is consumed. A "held-out" split carved from any
of them would not be held out, so there is nothing in the existing corpus to
re-split. The evaluation set is therefore generated from **new families** the
model has never seen (`holdout_families.HOLDOUT_FAMILIES`).

# Nothing about the frozen corpus changes

The new families are deliberately absent from `scenarios/__init__.py`, so
`generate_dataset.generate()` still produces `incidents_v2_generated.jsonl`
byte for byte. This script imports that module's `build_record` and style
rules rather than reimplementing them: the held-out records are built by the
same code path as the training records, so a difference in results cannot be
an artefact of a second, subtly different generator.

# The consumed test split is never opened

Group separation against test is proved from the group **names** recorded in
`data/processed/manifest.json`, not by reading `test.jsonl`. Near-duplicate
text comparison runs against train and val only. That is a real limitation —
no text-level duplicate check against test was performed — and it is recorded
in the manifest rather than glossed. It is bounded by construction: every new
group carries an `h3_` prefix that no existing group uses.

# What is checked before the set is written

- every record passes `validate_dataset.validate_record`;
- the corpus-level checks (`run_corpus_checks`) report no errors;
- no new group name collides with any of the 168 allocated groups;
- no two records share report text.

A failure stops the build rather than emitting a set with a known defect.
"""

from __future__ import annotations

import hashlib
import json
import sys
from collections import Counter
from pathlib import Path

SCRIPT_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPT_DIR))

import validate_dataset as vd  # noqa: E402
from generate_dataset import (  # noqa: E402
    LADDER_STYLES,
    SPECIAL_STYLES,
    build_record,
)
from holdout_families import HOLDOUT_FAMILIES  # noqa: E402
from scenarios.schema import KIND_MULTI_MESSAGE  # noqa: E402

TRAINING_ROOT = SCRIPT_DIR.parent
PROCESSED = TRAINING_ROOT / "data" / "processed"
DEFAULT_OUT = TRAINING_ROOT / "data" / "holdout" / "holdout_v3.jsonl"

HOLDOUT_VERSION = "securemesh-slm-holdout-v3"


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def generate_holdout() -> list[dict]:
    """Mirrors `generate_dataset.generate()` exactly, over the new families."""
    records: list[dict] = []
    seen_ids: set[str] = set()
    seen_texts: set[str] = set()

    for family in HOLDOUT_FAMILIES:
        for level_index, level in enumerate(family.levels):
            styles = LADDER_STYLES if level.kind == "ladder" else SPECIAL_STYLES
            for phrasing_index, phrasing in enumerate(level.phrasings):
                for style in styles:
                    # Same rule as the training generator: multi-message
                    # reports are already long and structured, so clipping
                    # them as well would obscure the message boundaries.
                    if level.kind == KIND_MULTI_MESSAGE and style == "noisy":
                        style = "abbrev"
                    record = build_record(
                        family, level_index, level, phrasing_index, phrasing, style
                    )
                    # Deduplicate on the *validator's* notion of a duplicate,
                    # not a weaker one. `normalise_for_comparison` ignores
                    # punctuation, so a `noisy` variant that only dropped a
                    # comma is the same record as its `plain` original — a
                    # difference the raw-text key used by the training
                    # generator does not see. Keeping both would add weight
                    # without adding information and would be rejected by
                    # `run_corpus_checks` anyway.
                    text_key = vd.normalise_for_comparison(record["report_text"])
                    if record["id"] in seen_ids or text_key in seen_texts:
                        continue
                    seen_ids.add(record["id"])
                    seen_texts.add(text_key)
                    records.append(record)
    return records


def allocated_groups() -> dict[str, set[str]]:
    """Group names per existing split, read from the manifest.

    Deliberately not read from the split files: the test split is consumed and
    is not opened by this phase, and the manifest already records every group
    name it contains.
    """
    manifest = json.loads((PROCESSED / "manifest.json").read_text(encoding="utf-8"))
    return {
        name: set(split["scenario_groups"]) for name, split in manifest["splits"].items()
    }


def load_split_records(name: str) -> list[dict]:
    path = PROCESSED / f"{name}.jsonl"
    return [
        json.loads(line)
        for line in path.read_text(encoding="utf-8").splitlines()
        if line.strip()
    ]


def main() -> int:
    vd._fix_windows_console_encoding()
    out_path = DEFAULT_OUT

    records = generate_holdout()
    print(f"generated {len(records)} records from {len(HOLDOUT_FAMILIES)} new families")

    problems: list[str] = []

    # 1. Per-record schema validation, by the same validator the corpus uses.
    for record in records:
        for error in vd.validate_record(record):
            problems.append(f"{record['id']}: {error}")

    # 2. Corpus-level checks (imbalance, near-duplicates, unsupported numbers).
    errors, warnings = vd.run_corpus_checks(records)
    problems.extend(errors)

    # 3. Group separation against every allocated group, from the manifest.
    allocated = allocated_groups()
    every_allocated = set().union(*allocated.values())
    new_groups = {r["scenario_group"] for r in records}
    collisions = new_groups & every_allocated
    if collisions:
        problems.append(f"scenario_group collision with an existing split: {sorted(collisions)}")

    # 4. Near-duplicate text against train and val. Test is not opened.
    seen_elsewhere = set()
    for split in ("train", "val"):
        for record in load_split_records(split):
            seen_elsewhere.add(vd.normalise_for_comparison(record["report_text"]))
    overlap = [
        r["id"] for r in records
        if vd.normalise_for_comparison(r["report_text"]) in seen_elsewhere
    ]
    if overlap:
        problems.append(f"report text also present in train/val: {overlap[:5]}")

    if problems:
        print(f"\nREFUSING to write: {len(problems)} problem(s)")
        for problem in problems[:25]:
            print(f"  - {problem}")
        return 1

    for warning in warnings:
        print(f"  warning: {warning}")

    out_path.parent.mkdir(parents=True, exist_ok=True)
    with out_path.open("w", encoding="utf-8") as handle:
        for record in records:
            handle.write(json.dumps(record, ensure_ascii=False) + "\n")

    tags = Counter()
    for record in records:
        tags.update(record.get("variation") or [])

    manifest = {
        "holdout_version": HOLDOUT_VERSION,
        "why": (
            "Phase 7 removed `confidence` from analysis_schema(), changing the "
            "constrained-decoding grammar, so Phase 5/6 numbers no longer describe "
            "the current configuration. All 168 corpus scenario groups were already "
            "allocated (train 120 / val 24 / test 24), so this set is generated from "
            "new families rather than re-split from the corpus."
        ),
        "authoring_bias_note": (
            "These families were authored after the Phase 5/6 results were known. "
            "They were frozen before either model was run and are not revised after "
            "scoring, but this is a newly authored set, not a pre-existing holdout."
        ),
        "source": "training/scripts/holdout_families.py",
        "generator": "training/scripts/build_holdout.py (reuses generate_dataset.build_record)",
        "frozen_corpus_untouched": True,
        "test_split_opened": False,
        "record_count": len(records),
        "family_count": len(HOLDOUT_FAMILIES),
        "scenario_groups": sorted(new_groups),
        "group_collisions_with_existing_splits": 0,
        "category": dict(sorted(Counter(r["target"]["category"] for r in records).items())),
        "severity": dict(sorted(Counter(r["target"]["severity"] for r in records).items())),
        "access_status": dict(
            sorted(Counter(r["target"]["access_status"] for r in records).items())
        ),
        "variation_tags": dict(sorted(tags.items())),
        "limitations": [
            "No text-level duplicate check against test.jsonl: the consumed split is "
            "not opened. Group separation from test is proved from manifest group names.",
            "Synthetic throughout. Never to be represented as real incident data.",
        ],
    }
    manifest_path = out_path.with_suffix(".manifest.json")
    manifest_path.write_text(json.dumps(manifest, indent=2), encoding="utf-8")

    print(f"\nwrote {len(records)} records to {out_path}")
    print(f"  sha256 {sha256_file(out_path)}")
    print(f"  manifest {manifest_path}")
    print(f"  categories {len(manifest['category'])}  severities {len(manifest['severity'])}"
          f"  access {len(manifest['access_status'])}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
