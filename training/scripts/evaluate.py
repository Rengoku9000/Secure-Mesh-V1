#!/usr/bin/env python3
"""Scores model predictions against gold targets, in the shape of
docs/ai/EVALUATION.md's existing metrics.

# Two ways to evaluate a SecureMesh-SLM candidate

1. **This script** — offline, stdlib-only, scores a JSONL file of
   `{id, report_text, target, predicted}` records you already produced by
   whatever means (a manual llama.cpp run, a notebook, anything). Useful for
   quick iteration on the training data or a merged/quantized checkpoint
   before it's wired into SecureMesh at all.

2. **The existing Rust harness** — `cargo run --example run_benchmark` in
   `src-tauri/`, which is the authoritative, already-built evaluation used
   for every number in `docs/ai/EVALUATION.md`. Once a candidate model is
   pointed at from `LlamaConfig` (a later, human decision — not made by this
   pipeline), that harness is what actually measures it end-to-end, including
   latency and RAG grounding this script does not touch. This script does
   not replace it and does not invoke it automatically.

# No model is run here

This script never loads a model, calls llama.cpp, or makes a network
request. It reads a predictions file you already generated and a gold file
already produced by `build_dataset.py`.
"""

from __future__ import annotations

import argparse
import json
import sys
from collections import Counter
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import validate_dataset as vd  # noqa: E402


def load_jsonl(path: Path) -> list[dict]:
    records = []
    with path.open("r", encoding="utf-8") as handle:
        for line in handle:
            line = line.strip()
            if line:
                records.append(json.loads(line))
    return records


def load_gold(path: Path) -> dict[str, dict]:
    return {record["id"]: record for record in load_jsonl(path)}


def load_predictions(path: Path) -> dict[str, dict]:
    """Loads predictions, keyed by id.

    Each record must carry `id` and `predicted`. `predicted` may be a JSON
    object already, or a raw string (exactly what a model's
    `generate_structured` call would return) — the latter is parsed here, and
    a parse failure is scored as an invalid response rather than raising, so
    one malformed prediction does not stop the whole evaluation.

    `latency_ms`, when present, is carried through for timing statistics.
    """
    predictions: dict[str, dict] = {}
    for record in load_jsonl(path):
        rid = record.get("id")
        if not rid:
            continue
        predicted = record.get("predicted")
        if isinstance(predicted, str):
            try:
                predicted = json.loads(predicted)
            except json.JSONDecodeError:
                predicted = None
        predictions[rid] = {
            "raw_valid_json": predicted is not None,
            "value": predicted,
            "latency_ms": record.get("latency_ms"),
            "error": record.get("error"),
        }
    return predictions


def confusion_matrix(pairs: list[tuple[str, str]], labels: list[str]) -> dict:
    """Gold-vs-predicted counts, plus per-label accuracy.

    Reported as nested dicts rather than a bare grid so a reader can see
    which *direction* a confusion runs — "COMMUNICATIONS read as EVACUATION"
    and the reverse are different failures with different fixes.
    """
    matrix = {gold: {pred: 0 for pred in labels + ["<invalid>"]} for gold in labels}
    for gold, predicted in pairs:
        if gold not in matrix:
            continue
        key = predicted if predicted in labels else "<invalid>"
        matrix[gold][key] += 1

    per_label = {}
    for gold, row in matrix.items():
        total = sum(row.values())
        per_label[gold] = {
            "n": total,
            "correct": row.get(gold, 0),
            "accuracy_pct": round(100.0 * row.get(gold, 0) / total, 1) if total else None,
            "most_confused_with": (
                max(
                    ((k, v) for k, v in row.items() if k != gold and v > 0),
                    key=lambda item: item[1],
                    default=("-", 0),
                )[0]
            ),
        }

    # Rows and cells that are entirely zero say nothing; dropping them keeps
    # a 12x13 matrix readable.
    trimmed = {
        gold: {pred: count for pred, count in row.items() if count}
        for gold, row in matrix.items()
        if sum(row.values())
    }
    return {"matrix": trimmed, "per_label": per_label}


def confidence_scale_report(predictions: dict[str, dict]) -> dict:
    """Reports which numeric scale a model used for `confidence`.

    Worth measuring on its own rather than folding into a schema-validity
    figure, because the two failure modes are different and only one is
    visible to an operator.

    The production schema (`ai/prompt.rs:197`) declares `confidence` as a
    bare `{"type": "number"}` with no bounds, and no prompt text tells the
    model what range to use. `RawAnalysis::validate` then **clamps** rather
    than rejects (`domain/intelligence.rs:319-322`), so a model answering
    `50` — meaning 50% — is stored as `1.0` and rendered by
    `IncidentAnalysis.tsx:129` as "100% (model's own estimate)".

    A value of `0.5` and a value of `50` express the same belief and end up
    at opposite ends of the displayed scale. That inversion is invisible in
    an accuracy score, so it is counted here explicitly.
    """
    in_range = out_of_range = absent = 0
    out_of_range_values: Counter = Counter()
    for prediction in predictions.values():
        value = prediction.get("value")
        if not isinstance(value, dict) or "confidence" not in value:
            absent += 1
            continue
        confidence = value["confidence"]
        if isinstance(confidence, (int, float)) and not isinstance(confidence, bool):
            if 0.0 <= float(confidence) <= 1.0:
                in_range += 1
            else:
                out_of_range += 1
                out_of_range_values[confidence] += 1
        else:
            out_of_range += 1

    emitted = in_range + out_of_range
    return {
        "emitted": emitted,
        "absent": absent,
        "in_0_1": in_range,
        "out_of_0_1": out_of_range,
        "out_of_range_pct_of_emitted": (
            round(100.0 * out_of_range / emitted, 1) if emitted else 0.0
        ),
        "out_of_range_values": dict(out_of_range_values.most_common()),
        "clamped_to_1_by_production": out_of_range,
        "note": (
            "values >1 are clamped to 1.0 by RawAnalysis::validate and displayed "
            "as '100% (model's own estimate)' — a model expressing doubt is shown "
            "as certain"
        ),
    }


def latency_stats(predictions: dict[str, dict]) -> dict | None:
    values = sorted(
        p["latency_ms"] for p in predictions.values() if isinstance(p.get("latency_ms"), int)
    )
    if not values:
        return None
    return {
        "n": len(values),
        "mean_ms": round(sum(values) / len(values)),
        "median_ms": values[len(values) // 2],
        "min_ms": values[0],
        "max_ms": values[-1],
    }


def entity_words(target: dict) -> set[str]:
    words: set[str] = set()
    for field in ("entities", "affected_resources"):
        for item in target.get(field, []) or []:
            words.update(w.lower() for w in item.split() if len(w) > 2)
    for field in ("summary", "asset", "cause", "location_hint"):
        value = target.get(field)
        if isinstance(value, str):
            words.update(w.strip(".,!?").lower() for w in value.split() if len(w) > 2)
    return words


def score(gold: dict[str, dict], predictions: dict[str, dict]) -> dict:
    total = 0
    valid_json = 0
    category_correct = 0
    category_scored = 0
    severity_correct = 0
    severity_scored = 0
    access_status_correct = 0
    access_status_scored = 0
    schema_valid = 0
    recall_scores: list[float] = []
    missing_predictions: list[str] = []
    category_confusion: Counter = Counter()
    # Gold/predicted pairs feeding the confusion matrices. A record whose
    # output would not parse still contributes a pair, with "<invalid>" as
    # the prediction, so a model that fails to answer cannot improve its
    # apparent accuracy by dropping the hard cases.
    category_pairs: list[tuple[str, str]] = []
    severity_pairs: list[tuple[str, str]] = []
    access_pairs: list[tuple[str, str]] = []

    for record_id, gold_record in gold.items():
        total += 1
        gold_target = gold_record["target"]
        prediction = predictions.get(record_id)

        if prediction is None:
            missing_predictions.append(record_id)
            continue

        if prediction["raw_valid_json"]:
            valid_json += 1
        predicted_target = prediction["value"] if isinstance(prediction["value"], dict) else None
        if predicted_target is None:
            for field, pairs in (
                ("category", category_pairs),
                ("severity", severity_pairs),
                ("access_status", access_pairs),
            ):
                if field in gold_target:
                    pairs.append((str(gold_target[field]).upper(), "<invalid>"))
            continue

        # Re-uses the same schema check training data itself must pass, so
        # "did the model produce something SecureMesh would accept" is
        # measured with the identical rule set, not a second approximation.
        if not vd.validate_target(predicted_target):
            schema_valid += 1

        if "category" in gold_target:
            category_scored += 1
            gold_cat = str(gold_target["category"]).upper()
            pred_cat = str(predicted_target.get("category", "")).upper()
            category_confusion[(gold_cat, pred_cat)] += 1
            category_pairs.append((gold_cat, pred_cat))
            if pred_cat == gold_cat:
                category_correct += 1

        if "severity" in gold_target:
            severity_scored += 1
            gold_sev = str(gold_target["severity"]).upper()
            pred_sev = str(predicted_target.get("severity", "")).upper()
            severity_pairs.append((gold_sev, pred_sev))
            if pred_sev == gold_sev:
                severity_correct += 1

        if "access_status" in gold_target:
            access_status_scored += 1
            gold_access = str(gold_target["access_status"]).upper()
            pred_access = str(predicted_target.get("access_status", "")).upper()
            access_pairs.append((gold_access, pred_access))
            if pred_access == gold_access:
                access_status_correct += 1

        gold_words = entity_words(gold_target)
        predicted_words = entity_words(predicted_target)
        if gold_words:
            recall_scores.append(len(gold_words & predicted_words) / len(gold_words))

    def pct(numerator: int, denominator: int) -> float:
        return round(100.0 * numerator / denominator, 1) if denominator else 0.0

    # Field-level extraction: did the model populate the optional fields the
    # gold target populates? Measured as presence agreement rather than exact
    # string match, because "structural failure" for "collapse" is not an
    # extraction error — the same reasoning EVALUATION.md gives for using
    # recall on keywords rather than exact match.
    field_presence = {}
    for field in ("entities", "affected_resources", "location_hint", "asset", "cause"):
        expected = [
            (record_id, gold_record)
            for record_id, gold_record in gold.items()
            if gold_record["target"].get(field)
        ]
        found = 0
        for record_id, _gold_record in expected:
            prediction = predictions.get(record_id)
            if prediction and isinstance(prediction.get("value"), dict):
                if prediction["value"].get(field):
                    found += 1
        field_presence[field] = {
            "gold_has_field": len(expected),
            "predicted_has_field": found,
            "recall_pct": pct(found, len(expected)),
        }

    return {
        "total_gold_records": total,
        "missing_predictions": missing_predictions,
        "request_errors": sum(1 for p in predictions.values() if p.get("error")),
        "valid_json_rate_pct": pct(valid_json, total),
        "schema_valid_rate_pct": pct(schema_valid, total),
        "category_accuracy_pct": pct(category_correct, category_scored),
        "severity_accuracy_pct": pct(severity_correct, severity_scored),
        "access_status_accuracy_pct": pct(access_status_correct, access_status_scored),
        "keyword_recall_mean_pct": (
            round(100.0 * sum(recall_scores) / len(recall_scores), 1) if recall_scores else 0.0
        ),
        "field_presence_recall": field_presence,
        "category_confusion": confusion_matrix(category_pairs, vd_category_labels()),
        "severity_confusion": confusion_matrix(severity_pairs, vd_severity_labels()),
        "access_status_confusion": confusion_matrix(access_pairs, vd_access_labels()),
        "confidence_scale": confidence_scale_report(predictions),
        "latency": latency_stats(predictions),
        "category_confusion_top10": category_confusion.most_common(10),
    }


def vd_category_labels() -> list[str]:
    return sorted(vd.CATEGORY_VALUES)


def vd_severity_labels() -> list[str]:
    # Ordered by urgency rather than alphabetically: a severity confusion
    # matrix is read for how far off a prediction was, and alphabetical order
    # would put CRITICAL next to HIGH and LOW next to MEDIUM by accident.
    return ["LOW", "MEDIUM", "HIGH", "CRITICAL"]


def vd_access_labels() -> list[str]:
    return ["OPEN", "RESTRICTED", "BLOCKED", "UNKNOWN"]


def print_confusion(title: str, confusion: dict, labels: list[str]) -> None:
    print(f"\n{title}")
    print(f"  {'gold \\ predicted':22s} {'n':>4s} {'correct':>8s} {'acc%':>7s}  most confused with")
    for label in labels:
        stats = confusion["per_label"].get(label)
        if not stats or not stats["n"]:
            continue
        accuracy = "-" if stats["accuracy_pct"] is None else f"{stats['accuracy_pct']:.1f}"
        print(
            f"  {label:22s} {stats['n']:4d} {stats['correct']:8d} {accuracy:>7s}  "
            f"{stats['most_confused_with']}"
        )


def print_human_report(report: dict) -> None:
    print(f"Records scored: {report['total_gold_records']}")
    if report.get("missing_predictions"):
        print(f"Missing predictions: {len(report['missing_predictions'])}")
    if report.get("request_errors"):
        print(f"Request errors: {report['request_errors']}")

    print("\nHeadline metrics:")
    for label, key in (
        ("Valid structured output", "valid_json_rate_pct"),
        ("Schema-valid output", "schema_valid_rate_pct"),
        ("Category accuracy", "category_accuracy_pct"),
        ("Severity accuracy", "severity_accuracy_pct"),
        ("Access status accuracy", "access_status_accuracy_pct"),
        ("Keyword recall (mean)", "keyword_recall_mean_pct"),
    ):
        print(f"  {label:26s} {report[key]:6.1f}%")

    conf = report.get("confidence_scale")
    if conf and conf["emitted"]:
        print(
            f"\nConfidence scale: {conf['emitted']} emitted, {conf['in_0_1']} within 0..1, "
            f"{conf['out_of_0_1']} outside ({conf['out_of_range_pct_of_emitted']}% of emitted)"
        )
        if conf["out_of_range_values"]:
            print(f"  out-of-range values: {conf['out_of_range_values']}")
            print(f"  {conf['note']}")

    latency = report.get("latency")
    if latency:
        print(
            f"\nLatency over {latency['n']} requests: "
            f"mean {latency['mean_ms']} ms, median {latency['median_ms']} ms, "
            f"min {latency['min_ms']} ms, max {latency['max_ms']} ms"
        )

    print("\nField presence recall (gold has it -> model produced it):")
    for field, stats in report.get("field_presence_recall", {}).items():
        if stats["gold_has_field"]:
            print(
                f"  {field:20s} {stats['predicted_has_field']:4d}/{stats['gold_has_field']:<4d} "
                f"({stats['recall_pct']:.1f}%)"
            )

    print_confusion("Category, per label:", report["category_confusion"], vd_category_labels())
    print_confusion("Severity, per label:", report["severity_confusion"], vd_severity_labels())
    print_confusion(
        "Access status, per label:", report["access_status_confusion"], vd_access_labels()
    )

    print("\nMost frequent category confusions (gold -> predicted):")
    for (gold_label, predicted_label), count in report["category_confusion_top10"]:
        if gold_label != predicted_label:
            print(f"  {gold_label} -> {predicted_label}: {count}")


def main() -> int:
    vd._fix_windows_console_encoding()
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--gold",
        type=Path,
        required=True,
        help="A processed split, e.g. training/data/processed/test.jsonl",
    )
    parser.add_argument(
        "--predictions",
        type=Path,
        required=True,
        help="JSONL of {id, predicted} — predicted is a JSON object or a raw JSON string",
    )
    parser.add_argument("--out", type=Path, help="Optional path to write the report as JSON")
    parser.add_argument(
        "--json-only", action="store_true", help="Print raw JSON instead of the readable report"
    )
    args = parser.parse_args()

    gold = load_gold(args.gold)
    predictions = load_predictions(args.predictions)
    report = score(gold, predictions)
    report["gold_file"] = str(args.gold)
    report["predictions_file"] = str(args.predictions)

    if args.json_only:
        print(json.dumps(report, indent=2))
    else:
        print_human_report(report)

    if args.out:
        args.out.parent.mkdir(parents=True, exist_ok=True)
        args.out.write_text(json.dumps(report, indent=2), encoding="utf-8")
        print(f"\nWritten to {args.out}")

    if report["missing_predictions"]:
        print(
            f"\nNOTE: {len(report['missing_predictions'])} gold record(s) had no "
            "matching prediction and were excluded from scoring.",
            file=sys.stderr,
        )

    print(
        "\nFor latency, RAM, and full RAG-grounding numbers, run the existing "
        "harness once this model is provisioned: "
        "`cd src-tauri && cargo run --example run_benchmark -- <seed> <corpus> <analysed> <questions>` "
        "— see docs/ai/EVALUATION.md."
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
