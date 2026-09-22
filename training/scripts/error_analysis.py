#!/usr/bin/env python3
"""Classifies what a model got wrong, and on which kinds of input.

An accuracy number says how much is wrong. This says *what* is wrong, which
is the part that decides whether fine-tuning is worth doing and, afterwards,
whether it actually helped.

Errors are classified into the failure kinds a SecureMesh analysis can have,
and then cross-tabulated against the input properties each record is tagged
with — noisy, abbreviated, multi-message, ambiguous. That cross-tabulation is
the useful half: "severity accuracy is 40%" is a fact about the model, while
"severity accuracy is 55% on plain text and 28% on noisy text" is a fact
about what to fix.

Reads local files only. No model is run and no network is touched.
"""

from __future__ import annotations

import argparse
import json
import sys
from collections import Counter, defaultdict
from pathlib import Path

SCRIPT_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPT_DIR))

import evaluate as ev  # noqa: E402
import validate_dataset as vd  # noqa: E402

# Input properties worth measuring separately, and the record `variation`
# tag that marks each. Written against the tags the Phase 2 generator emits.
INPUT_SLICES = {
    "noisy (typos)": "noisy",
    "abbreviated": "abbreviation",
    "multi-message": "multi_message",
    "ambiguous evidence": "ambiguous",
    "conflicting reports": "conflicting",
    "false positive / stand-down": "false_positive",
    "non-emergency info": "non_emergency_info",
    "short report": "short_report",
    "long report": "long_report",
}

ERROR_KINDS = [
    "formatting_failure",
    "category_confusion",
    "severity_confusion",
    "access_status_confusion",
    "extraction_failure",
]


def classify(gold_record: dict, prediction: dict | None) -> list[str]:
    """Returns every failure kind this record exhibits. Empty means correct."""
    target = gold_record["target"]

    if prediction is None or prediction.get("value") is None:
        return ["formatting_failure"]

    predicted = prediction["value"]
    if not isinstance(predicted, dict):
        return ["formatting_failure"]

    failures = []
    # A model whose output will not satisfy the schema the runtime enforces
    # has failed in a way that matters regardless of its content.
    if vd.validate_target(predicted):
        failures.append("formatting_failure")

    if "category" in target and str(predicted.get("category", "")).upper() != str(
        target["category"]
    ).upper():
        failures.append("category_confusion")

    if "severity" in target and str(predicted.get("severity", "")).upper() != str(
        target["severity"]
    ).upper():
        failures.append("severity_confusion")

    if "access_status" in target and str(predicted.get("access_status", "")).upper() != str(
        target["access_status"]
    ).upper():
        failures.append("access_status_confusion")

    # Extraction: the gold target populates a field from the report and the
    # model left it empty. Checked as presence, not wording — a different
    # phrasing of the same fact is not an extraction failure.
    for field in ("entities", "affected_resources", "location_hint"):
        if target.get(field) and not predicted.get(field):
            failures.append("extraction_failure")
            break

    return failures


def severity_distance(gold: str, predicted: str) -> int | None:
    """How many severity levels apart a prediction is.

    A CRITICAL read as HIGH and a CRITICAL read as LOW are both wrong and are
    not equally wrong, and an accuracy figure cannot tell them apart.
    """
    order = {"LOW": 0, "MEDIUM": 1, "HIGH": 2, "CRITICAL": 3}
    if gold not in order or predicted not in order:
        return None
    return order[predicted] - order[gold]


def analyse(gold: dict[str, dict], predictions: dict[str, dict]) -> dict:
    per_record = {}
    error_counts: Counter = Counter()
    slice_stats: dict[str, dict] = {}

    for record_id, gold_record in gold.items():
        failures = classify(gold_record, predictions.get(record_id))
        per_record[record_id] = failures
        for failure in failures:
            error_counts[failure] += 1

    total = len(gold)
    fully_correct = sum(1 for failures in per_record.values() if not failures)

    # Accuracy per input slice, which is where the interesting differences
    # live — a model can look fine overall and be badly degraded on noise.
    for label, tag in INPUT_SLICES.items():
        ids = [
            record_id
            for record_id, record in gold.items()
            if tag in (record.get("variation") or [])
        ]
        if not ids:
            continue
        slice_stats[label] = summarise_slice(ids, gold, predictions, per_record)

    plain_ids = [
        record_id
        for record_id, record in gold.items()
        if not ({"noisy", "abbreviation"} & set(record.get("variation") or []))
    ]
    if plain_ids:
        slice_stats["clean text (no noise/abbrev)"] = summarise_slice(
            plain_ids, gold, predictions, per_record
        )

    # Severity direction: does the model over- or under-call urgency? Both
    # are dangerous and they call for opposite corrections.
    over = under = exact = 0
    for record_id, gold_record in gold.items():
        prediction = predictions.get(record_id)
        if not prediction or not isinstance(prediction.get("value"), dict):
            continue
        gold_severity = str(gold_record["target"].get("severity", "")).upper()
        predicted_severity = str(prediction["value"].get("severity", "")).upper()
        distance = severity_distance(gold_severity, predicted_severity)
        if distance is None:
            continue
        if distance > 0:
            over += 1
        elif distance < 0:
            under += 1
        else:
            exact += 1

    return {
        "total_records": total,
        "fully_correct": fully_correct,
        "fully_correct_pct": round(100.0 * fully_correct / total, 1) if total else 0.0,
        "error_counts": dict(error_counts.most_common()),
        "error_rates_pct": {
            kind: round(100.0 * error_counts.get(kind, 0) / total, 1) for kind in ERROR_KINDS
        },
        "by_input_slice": slice_stats,
        "severity_direction": {
            "over_called": over,
            "under_called": under,
            "exact": exact,
            "note": "over_called = model assigned higher urgency than the gold label",
        },
        "per_record_failures": per_record,
    }


def summarise_slice(
    ids: list[str], gold: dict, predictions: dict, per_record: dict
) -> dict:
    correct_category = correct_severity = correct_access = scored = 0
    clean = 0
    for record_id in ids:
        failures = per_record[record_id]
        if not failures:
            clean += 1
        prediction = predictions.get(record_id)
        if not prediction or not isinstance(prediction.get("value"), dict):
            continue
        scored += 1
        target = gold[record_id]["target"]
        predicted = prediction["value"]
        if str(predicted.get("category", "")).upper() == str(target.get("category", "")).upper():
            correct_category += 1
        if str(predicted.get("severity", "")).upper() == str(target.get("severity", "")).upper():
            correct_severity += 1
        if str(predicted.get("access_status", "")).upper() == str(
            target.get("access_status", "")
        ).upper():
            correct_access += 1

    def pct(numerator: int) -> float:
        return round(100.0 * numerator / scored, 1) if scored else 0.0

    return {
        "n": len(ids),
        "scored": scored,
        "category_accuracy_pct": pct(correct_category),
        "severity_accuracy_pct": pct(correct_severity),
        "access_status_accuracy_pct": pct(correct_access),
        "fully_correct_pct": round(100.0 * clean / len(ids), 1) if ids else 0.0,
    }


def representative_examples(
    gold: dict, predictions: dict, per_record: dict, per_kind: int = 3
) -> dict:
    """A few real cases per failure kind, taken in dataset order.

    Deliberately not selected for how illustrative they look: the first
    matching records are used, so this cannot become a curated highlight
    reel of flattering or unflattering cases.
    """
    examples: dict[str, list] = defaultdict(list)
    for record_id, failures in per_record.items():
        for failure in failures:
            if len(examples[failure]) >= per_kind:
                continue
            gold_record = gold[record_id]
            prediction = predictions.get(record_id)
            predicted = prediction.get("value") if prediction else None
            examples[failure].append(
                {
                    "id": record_id,
                    "report_text": gold_record["report_text"],
                    "variation": gold_record.get("variation", []),
                    "gold": {
                        key: gold_record["target"].get(key)
                        for key in ("category", "severity", "access_status")
                    },
                    "predicted": (
                        {
                            key: predicted.get(key)
                            for key in ("category", "severity", "access_status")
                        }
                        if isinstance(predicted, dict)
                        else None
                    ),
                }
            )
    return dict(examples)


def main() -> int:
    vd._fix_windows_console_encoding()
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--gold", type=Path, required=True)
    parser.add_argument("--predictions", type=Path, required=True)
    parser.add_argument("--out", type=Path)
    parser.add_argument("--examples-per-kind", type=int, default=3)
    args = parser.parse_args()

    gold = ev.load_gold(args.gold)
    predictions = ev.load_predictions(args.predictions)
    report = analyse(gold, predictions)
    report["examples"] = representative_examples(
        gold, predictions, report["per_record_failures"], args.examples_per_kind
    )

    print(f"Records: {report['total_records']}")
    print(
        f"Fully correct (category, severity, access, extraction all right): "
        f"{report['fully_correct']} ({report['fully_correct_pct']}%)"
    )

    print("\nError rates by kind:")
    for kind in ERROR_KINDS:
        count = report["error_counts"].get(kind, 0)
        print(f"  {kind:26s} {count:4d}  ({report['error_rates_pct'][kind]:.1f}%)")

    direction = report["severity_direction"]
    print(
        f"\nSeverity direction: {direction['over_called']} over-called, "
        f"{direction['under_called']} under-called, {direction['exact']} exact"
    )

    print("\nAccuracy by input type:")
    print(f"  {'slice':32s} {'n':>4s} {'cat%':>7s} {'sev%':>7s} {'acc%':>7s} {'all%':>7s}")
    for label, stats in report["by_input_slice"].items():
        print(
            f"  {label:32s} {stats['n']:4d} {stats['category_accuracy_pct']:7.1f} "
            f"{stats['severity_accuracy_pct']:7.1f} {stats['access_status_accuracy_pct']:7.1f} "
            f"{stats['fully_correct_pct']:7.1f}"
        )

    print("\nRepresentative failures (first matching records, not curated):")
    for kind, examples in report["examples"].items():
        if not examples:
            continue
        print(f"\n  --- {kind} ---")
        for example in examples:
            print(f"  [{example['id']}]")
            print(f"    report:    {example['report_text'][:110]}")
            print(f"    gold:      {example['gold']}")
            print(f"    predicted: {example['predicted']}")

    if args.out:
        args.out.parent.mkdir(parents=True, exist_ok=True)
        args.out.write_text(json.dumps(report, indent=2), encoding="utf-8")
        print(f"\nWritten to {args.out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
