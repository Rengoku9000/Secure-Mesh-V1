#!/usr/bin/env python3
"""Builds train/val/test splits from the raw SecureMesh-SLM training records.

# What this does

1. Reads every `*.jsonl` file in `training/data/raw/`.
2. Validates every record with `validate_dataset.py` — the exact same rules
   the training data must satisfy to be usable, so a malformed or
   schema-violating record stops the build rather than silently entering the
   dataset.
3. Splits by `scenario_group`, not by record, using a deterministic hash
   bucket. This is the leakage guard: a scenario_group ("flood_rooftop_1",
   say) always lands entirely in one split, so a paraphrase of a training
   example can never appear in validation or test.
4. Writes `training/data/processed/{train,val,test}.jsonl` and a
   `manifest.json` recording exactly what went into them.

# Offline by construction

This script reads and writes local files only. It has no dependency beyond
the Python standard library and `validate_dataset.py` in this same
directory, and makes no network call of any kind.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import sys
from collections import Counter, defaultdict
from datetime import datetime, timezone
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import validate_dataset as vd  # noqa: E402  (path must be set up first)

SCRIPT_DIR = Path(__file__).resolve().parent
TRAINING_ROOT = SCRIPT_DIR.parent
DEFAULT_RAW_DIR = TRAINING_ROOT / "data" / "raw"
DEFAULT_PROCESSED_DIR = TRAINING_ROOT / "data" / "processed"

DATASET_VERSION = "securemesh-slm-v2"

# Share of each category's scenario groups held out. The remainder trains.
# Applied per category rather than over the corpus as a whole — see
# `assign_groups_to_splits`.
TEST_SHARE = 0.15
VAL_SHARE = 0.15


def collect_raw_files(raw_dir: Path) -> list[Path]:
    return sorted(raw_dir.glob("*.jsonl"))


def load_and_validate(raw_files: list[Path]) -> list[dict]:
    """Validates every raw file. Exits the process if any record is invalid.

    A build that proceeded on partially-invalid data would let a bad example
    reach a fine-tuning run silently; refusing here is the whole point of
    running the validator as a build step rather than only by hand.
    """
    all_valid: list[dict] = []
    any_errors = False

    for path in raw_files:
        valid_records, errors = vd.validate_file(path)
        for lineno, record_id, message in errors:
            print(f"VALIDATION ERROR {path}:{lineno} [{record_id}] {message}")
            any_errors = True
        all_valid.extend(valid_records)

    if any_errors:
        print("\nRefusing to build a dataset from invalid records. Fix the errors above.")
        sys.exit(1)

    # A record's `id` must also be unique *across files*, not just within one.
    seen: dict[str, str] = {}
    for record in all_valid:
        rid = record["id"]
        if rid in seen:
            print(f"VALIDATION ERROR: id {rid!r} appears in both {seen[rid]} and another file")
            any_errors = True
        else:
            seen[rid] = "raw set"
    if any_errors:
        sys.exit(1)

    return all_valid


def assign_groups_to_splits(records: list[dict]) -> dict[str, str]:
    """Assigns each scenario_group to one split, stratified by category.

    The split unit is the **scenario group**, never the record: a whole
    family — including all its contrastive variants — lands in one split, so
    a paraphrase of a training example can never appear in validation or
    test.

    Stratifying by category on top of that is not decoration. A flat hash
    bucket over 168 groups left the test split with no MEDICAL and no OTHER
    examples at all, which would have made the test score silent about two
    of the twelve categories. Groups are therefore distributed within each
    category, so every split sees every category.

    Deterministic: groups are ordered by the SHA-256 of their name, which
    does not depend on file order, insertion order, or Python's per-process
    hash seed.
    """
    groups: dict[str, list[dict]] = defaultdict(list)
    for record in records:
        groups[record["scenario_group"]].append(record)

    by_category: dict[str, list[str]] = defaultdict(list)
    for group, group_records in groups.items():
        # A family has one category; the majority vote only guards against a
        # hand-authored group that accidentally mixes them.
        category = Counter(
            r["target"].get("category", "OTHER") for r in group_records
        ).most_common(1)[0][0]
        by_category[category].append(group)

    assignment: dict[str, str] = {}
    for category, group_names in sorted(by_category.items()):
        ordered = sorted(
            group_names, key=lambda g: hashlib.sha256(g.encode("utf-8")).hexdigest()
        )
        count = len(ordered)
        if count == 1:
            held_out_test, held_out_val = 0, 0
        elif count == 2:
            held_out_test, held_out_val = 1, 0
        else:
            held_out_test = max(1, round(count * TEST_SHARE))
            held_out_val = max(1, round(count * VAL_SHARE))

        for index, group in enumerate(ordered):
            if index < held_out_test:
                assignment[group] = "test"
            elif index < held_out_test + held_out_val:
                assignment[group] = "val"
            else:
                assignment[group] = "train"
    return assignment


def split_records(records: list[dict]) -> dict[str, list[dict]]:
    """Places every record in the split its scenario_group was assigned."""
    assignment = assign_groups_to_splits(records)
    splits: dict[str, list[dict]] = {"train": [], "val": [], "test": []}
    for record in records:
        splits[assignment[record["scenario_group"]]].append(record)
    for bucket in splits:
        splits[bucket].sort(key=lambda r: r["id"])
    return splits


def verify_no_leakage(splits: dict[str, list[dict]]) -> bool:
    """Re-checks, independently of how the split was built, that no
    scenario_group appears in more than one split. This is a safety net, not
    the mechanism — `scenario_bucket` already guarantees this by construction
    — but a mechanism that guarantees something is worth checking anyway.
    """
    group_to_splits: dict[str, set[str]] = defaultdict(set)
    for split_name, records in splits.items():
        for record in records:
            group_to_splits[record["scenario_group"]].add(split_name)

    leaked = {group: sorted(s) for group, s in group_to_splits.items() if len(s) > 1}
    if leaked:
        print("LEAKAGE DETECTED — the following scenario_groups span multiple splits:")
        for group, split_names in leaked.items():
            print(f"  {group}: {split_names}")
        return False
    return True


def verify_no_near_duplicate_leakage(splits: dict[str, list[dict]]) -> bool:
    """Checks that no held-out report closely resembles a training report.

    Group separation stops *identical scenarios* crossing the boundary. This
    catches the subtler case: two independently authored families that
    happen to have produced near-identical text, which would let a test
    score be partly a memory score. It is the check that actually protects
    the evaluation, so it runs on every build rather than on request.
    """
    train = splits["train"]
    held_out = splits["val"] + splits["test"]
    if not train or not held_out:
        return True

    train_shingles = [(r["id"], vd.shingles(r["report_text"])) for r in train]
    index: dict[str, list[int]] = defaultdict(list)
    for position, (_id, shingle_set) in enumerate(train_shingles):
        for shingle in shingle_set:
            index[shingle].append(position)

    findings = []
    for record in held_out:
        shingle_set = vd.shingles(record["report_text"])
        candidates: set[int] = set()
        for shingle in shingle_set:
            candidates.update(index[shingle])
        for position in candidates:
            train_id, train_set = train_shingles[position]
            score = vd.jaccard(shingle_set, train_set)
            if score >= vd.NEAR_DUPLICATE_THRESHOLD:
                findings.append((record["id"], train_id, round(score, 3)))

    if findings:
        print(
            f"NEAR-DUPLICATE LEAKAGE — {len(findings)} held-out record(s) closely "
            f"resemble a training record (threshold {vd.NEAR_DUPLICATE_THRESHOLD}):"
        )
        for held_id, train_id, score in findings[:20]:
            print(f"  {held_id} ~ {train_id} (similarity {score})")
        return False

    print(
        f"  no near-duplicate leakage: {len(held_out)} held-out records checked "
        f"against {len(train)} training records"
    )
    return True


def build_manifest(splits: dict[str, list[dict]], raw_files: list[Path]) -> dict:
    def summarize(records: list[dict]) -> dict:
        return {
            "count": len(records),
            "provenance": dict(Counter(r["provenance"] for r in records)),
            "category": dict(Counter(r["target"].get("category", "UNSET") for r in records)),
            "severity": dict(Counter(r["target"].get("severity", "UNSET") for r in records)),
            "scenario_groups": sorted({r["scenario_group"] for r in records}),
        }

    return {
        "dataset_version": DATASET_VERSION,
        # Informational only, like `created_at` elsewhere in this project —
        # never used to decide anything, only to record when this ran.
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "source_files": [str(p.name) for p in raw_files],
        "split_shares": {"test": TEST_SHARE, "val": VAL_SHARE, "stratified_by": "category"},
        "splits": {name: summarize(records) for name, records in splits.items()},
        "total_records": sum(len(records) for records in splits.values()),
    }


def write_jsonl(path: Path, records: list[dict]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w", encoding="utf-8") as handle:
        for record in records:
            handle.write(json.dumps(record, ensure_ascii=False, sort_keys=True))
            handle.write("\n")


def main() -> int:
    vd._fix_windows_console_encoding()
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--raw-dir", type=Path, default=DEFAULT_RAW_DIR)
    parser.add_argument("--out-dir", type=Path, default=DEFAULT_PROCESSED_DIR)
    args = parser.parse_args()

    raw_files = collect_raw_files(args.raw_dir)
    if not raw_files:
        print(f"No *.jsonl files found in {args.raw_dir}")
        return 1

    print(f"Reading {len(raw_files)} raw file(s) from {args.raw_dir}")
    records = load_and_validate(raw_files)
    print(f"{len(records)} record(s) passed validation")

    splits = split_records(records)
    if not verify_no_leakage(splits):
        return 1
    if not verify_no_near_duplicate_leakage(splits):
        return 1

    corpus_errors, corpus_warnings = vd.run_corpus_checks(records)
    for message in corpus_errors:
        print(f"CORPUS ERROR: {message}")
    if corpus_errors:
        print("\nRefusing to build: the corpus has duplication or leakage defects.")
        return 1
    for message in corpus_warnings:
        print(f"CORPUS WARNING: {message}")

    for name, split_records_ in splits.items():
        write_jsonl(args.out_dir / f"{name}.jsonl", split_records_)
        print(f"  {name}: {len(split_records_)} record(s) -> {args.out_dir / f'{name}.jsonl'}")

    manifest = build_manifest(splits, raw_files)
    manifest_path = args.out_dir / "manifest.json"
    manifest_path.write_text(json.dumps(manifest, indent=2, sort_keys=True), encoding="utf-8")
    print(f"Manifest written to {manifest_path}")

    if any(len(v) == 0 for v in splits.values()):
        print(
            "\nWARNING: at least one split is empty. This is expected with a "
            "small starting dataset (~60 records split by scenario_group can "
            "easily leave val/test thin) but must be watched as more data is "
            "added — see training/README.md."
        )

    return 0


if __name__ == "__main__":
    sys.exit(main())
