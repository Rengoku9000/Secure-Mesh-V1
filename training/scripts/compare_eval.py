#!/usr/bin/env python3
"""Side-by-side comparison of two scored evaluation runs.

Reads the JSON that the **unchanged** `evaluate.py` and `error_analysis.py`
already produce, and computes differences. It does not re-score anything, does
not re-parse model output for accuracy, and applies no new definition of
correctness — every accuracy figure here is the evaluator's own.

One supplementary check is added, using an existing rule rather than a new
one: `validate_dataset.unsupported_number_claims`, the fabricated-precision
check from Phase 2, is applied to each model output against its report. It
maps the "unsupported / invented information" error kind onto the project's
existing definition of invented quantities.

# What is compared

- **control**: HF base model, NF4, grammar-constrained decoding
- **treatment**: the same, plus the checkpoint-66 LoRA adapter
- **recorded baseline** (reference only): the Phase 3 GGUF run. Different
  quantisation (Q4_K_M) and runtime (llama.cpp on CPU), so it is shown for
  context and never used to compute an improvement.

# What is not measured

Every field scored here is **model-generated**. SecureMesh's deterministic
rule layer (`src-tauri/src/ai/nlp.rs`) is Rust and is not run by this
pipeline, so no deterministic NLP field appears in any figure.
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

SCRIPT_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPT_DIR))

import validate_dataset as vd  # noqa: E402

CATEGORIES = [
    "MEDICAL", "INFRASTRUCTURE", "EVACUATION", "SEVERE_WEATHER", "FLOODING",
    "COMMUNICATIONS", "RESOURCE_SHORTAGE", "ROAD_BLOCKAGE", "POWER", "FIRE",
    "EARTHQUAKE", "OTHER",
]
SEVERITIES = ["LOW", "MEDIUM", "HIGH", "CRITICAL"]
ACCESS = ["OPEN", "RESTRICTED", "BLOCKED", "UNKNOWN"]


def load(path: Path) -> dict:
    return json.loads(path.read_text(encoding="utf-8"))


def delta(control, treatment) -> dict:
    if control is None or treatment is None:
        return {"control": control, "treatment": treatment, "pp": None, "relative_pct": None}
    pp = round(treatment - control, 1)
    relative = round(100.0 * (treatment - control) / control, 1) if control else None
    return {"control": control, "treatment": treatment, "pp": pp, "relative_pct": relative}


def fmt(value, suffix="") -> str:
    return "  —" if value is None else f"{value:.1f}{suffix}"


def signed(value, suffix="") -> str:
    return "  —" if value is None else f"{value:+.1f}{suffix}"


def invented_quantities(gold_path: Path, predictions_path: Path) -> dict:
    """Counts outputs asserting a quantity their report never states."""
    gold = {
        json.loads(line)["id"]: json.loads(line)
        for line in gold_path.read_text(encoding="utf-8").splitlines()
        if line.strip()
    }
    flagged = []
    checked = 0
    for line in predictions_path.read_text(encoding="utf-8").splitlines():
        if not line.strip():
            continue
        prediction = json.loads(line)
        record = gold.get(prediction["id"])
        if record is None:
            continue
        try:
            target = json.loads(prediction.get("predicted") or "")
        except json.JSONDecodeError:
            continue
        if not isinstance(target, dict):
            continue
        checked += 1
        claims = vd.unsupported_number_claims({"report_text": record["report_text"], "target": target})
        if claims:
            flagged.append({"id": prediction["id"], "claims": claims})
    return {
        "outputs_checked": checked,
        "outputs_with_invented_quantity": len(flagged),
        "rate_pct": round(100.0 * len(flagged) / checked, 1) if checked else 0.0,
        "examples": flagged[:5],
    }


def per_label(control: dict, treatment: dict, key: str, labels: list[str]) -> list[dict]:
    rows = []
    for label in labels:
        c = control[key]["per_label"].get(label) or {}
        t = treatment[key]["per_label"].get(label) or {}
        rows.append(
            {
                "label": label,
                "n": t.get("n", c.get("n", 0)),
                "control_correct": c.get("correct"),
                "treatment_correct": t.get("correct"),
                "control_most_confused_with": c.get("most_confused_with"),
                "treatment_most_confused_with": t.get("most_confused_with"),
                **delta(c.get("accuracy_pct"), t.get("accuracy_pct")),
            }
        )
    return rows


def print_label_table(title: str, rows: list[dict]) -> None:
    print(f"\n{title}")
    print(f"  {'label':18s} {'n':>4s} {'control':>9s} {'treatment':>10s} {'Δpp':>8s}   "
          f"{'control confused with':22s} treatment confused with")
    for row in rows:
        if not row["n"]:
            continue
        print(
            f"  {row['label']:18s} {row['n']:4d} {fmt(row['control'], '%'):>9s} "
            f"{fmt(row['treatment'], '%'):>10s} {signed(row['pp']):>8s}   "
            f"{str(row['control_most_confused_with']):22s} {row['treatment_most_confused_with']}"
        )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--gold", type=Path, required=True)
    parser.add_argument("--control-metrics", type=Path, required=True)
    parser.add_argument("--treatment-metrics", type=Path, required=True)
    parser.add_argument("--control-errors", type=Path, required=True)
    parser.add_argument("--treatment-errors", type=Path, required=True)
    parser.add_argument("--control-predictions", type=Path, required=True)
    parser.add_argument("--treatment-predictions", type=Path, required=True)
    parser.add_argument("--recorded-baseline-metrics", type=Path)
    parser.add_argument("--recorded-baseline-errors", type=Path)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()

    vd._fix_windows_console_encoding()
    cm, tm = load(args.control_metrics), load(args.treatment_metrics)
    ce, te = load(args.control_errors), load(args.treatment_errors)
    rb = load(args.recorded_baseline_metrics) if args.recorded_baseline_metrics else None
    rbe = load(args.recorded_baseline_errors) if args.recorded_baseline_errors else None

    headline_keys = [
        ("JSON parse", "valid_json_rate_pct", "m"),
        ("Schema accepted", "schema_valid_rate_pct", "m"),
        ("Category accuracy", "category_accuracy_pct", "m"),
        ("Severity accuracy", "severity_accuracy_pct", "m"),
        ("Access accuracy", "access_status_accuracy_pct", "m"),
        ("Keyword recall", "keyword_recall_mean_pct", "m"),
        ("Fully correct", "fully_correct_pct", "e"),
    ]
    headline = []
    for label, key, source in headline_keys:
        c = (cm if source == "m" else ce)[key]
        t = (tm if source == "m" else te)[key]
        reference = None
        if source == "m" and rb:
            reference = rb.get(key)
        elif source == "e" and rbe:
            reference = rbe.get(key)
        headline.append({"metric": label, **delta(c, t), "recorded_gguf_baseline": reference})

    latency = {
        "mean_ms": delta(cm["latency"]["mean_ms"], tm["latency"]["mean_ms"]),
        "median_ms": delta(cm["latency"]["median_ms"], tm["latency"]["median_ms"]),
    }

    categories = per_label(cm, tm, "category_confusion", CATEGORIES)
    severities = per_label(cm, tm, "severity_confusion", SEVERITIES)
    access = per_label(cm, tm, "access_status_confusion", ACCESS)

    slices = []
    for name in te["by_input_slice"]:
        c = ce["by_input_slice"].get(name, {})
        t = te["by_input_slice"][name]
        slices.append(
            {
                "slice": name,
                "n": t["n"],
                "category": delta(c.get("category_accuracy_pct"), t.get("category_accuracy_pct")),
                "severity": delta(c.get("severity_accuracy_pct"), t.get("severity_accuracy_pct")),
                "access": delta(c.get("access_status_accuracy_pct"), t.get("access_status_accuracy_pct")),
                "fully_correct": delta(c.get("fully_correct_pct"), t.get("fully_correct_pct")),
            }
        )

    extraction = {
        field: delta(cm["field_presence_recall"][field]["recall_pct"], tm["field_presence_recall"][field]["recall_pct"])
        | {"gold_has_field": tm["field_presence_recall"][field]["gold_has_field"]}
        for field in tm["field_presence_recall"]
    }

    error_kinds = {
        kind: delta(ce["error_rates_pct"].get(kind), te["error_rates_pct"].get(kind))
        for kind in te["error_rates_pct"]
    }

    invented = {
        "control": invented_quantities(args.gold, args.control_predictions),
        "treatment": invented_quantities(args.gold, args.treatment_predictions),
    }

    report = {
        "note": "control = HF base + grammar; treatment = HF base + checkpoint-66 + grammar; identical "
                "NF4 quantisation, prompt, tokenizer, decoding and scoring. recorded_gguf_baseline is a "
                "different runtime and is reference only.",
        "headline": headline,
        "latency": latency,
        "categories": categories,
        "severities": severities,
        "severity_matrix": {"control": cm["severity_confusion"]["matrix"], "treatment": tm["severity_confusion"]["matrix"]},
        "severity_direction": {"control": ce["severity_direction"], "treatment": te["severity_direction"]},
        "access": access,
        "access_matrix": {"control": cm["access_status_confusion"]["matrix"], "treatment": tm["access_status_confusion"]["matrix"]},
        "input_slices": slices,
        "extraction_field_presence": extraction,
        "error_rates": error_kinds,
        "invented_quantities": invented,
        "confidence_scale": {"control": cm.get("confidence_scale"), "treatment": tm.get("confidence_scale")},
    }

    print("HEADLINE (control = base+grammar, treatment = checkpoint-66+grammar)")
    print(f"  {'metric':20s} {'control':>9s} {'treatment':>10s} {'Δpp':>8s} {'rel%':>8s}   {'recorded GGUF (ref)':>20s}")
    for row in headline:
        print(f"  {row['metric']:20s} {fmt(row['control'], '%'):>9s} {fmt(row['treatment'], '%'):>10s} "
              f"{signed(row['pp']):>8s} {signed(row['relative_pct'], '%'):>8s}   "
              f"{fmt(row['recorded_gguf_baseline'], '%'):>20s}")
    for key, row in latency.items():
        print(f"  {'latency ' + key:20s} {row['control']:>9} {row['treatment']:>10} "
              f"{row['treatment'] - row['control']:>+8} {signed(row['relative_pct'], '%'):>8s}")

    print_label_table("CATEGORY (per label)", categories)
    print_label_table("SEVERITY (per label)", severities)
    print(f"  direction control   {ce['severity_direction']}")
    print(f"  direction treatment {te['severity_direction']}")
    print_label_table("ACCESS STATUS (per label)", access)

    print("\nINPUT SLICES")
    print(f"  {'slice':32s} {'n':>4s}  {'cat Δ':>14s}  {'sev Δ':>14s}  {'acc Δ':>14s}  {'all Δ':>14s}")
    for s in slices:
        def cell(d):
            return f"{fmt(d['control'])}→{fmt(d['treatment'])}"
        print(f"  {s['slice']:32s} {s['n']:4d}  {cell(s['category']):>14s}  {cell(s['severity']):>14s}  "
              f"{cell(s['access']):>14s}  {cell(s['fully_correct']):>14s}")

    print("\nEXTRACTION — field presence recall (model-generated fields only)")
    for field, row in extraction.items():
        if row["gold_has_field"]:
            print(f"  {field:20s} gold n={row['gold_has_field']:4d}  {fmt(row['control'], '%'):>8s} → "
                  f"{fmt(row['treatment'], '%'):>8s}  ({signed(row['pp'])}pp)")

    print("\nERROR RATES")
    for kind, row in error_kinds.items():
        print(f"  {kind:26s} {fmt(row['control'], '%'):>8s} → {fmt(row['treatment'], '%'):>8s}  ({signed(row['pp'])}pp)")

    print("\nINVENTED QUANTITIES (existing Phase 2 fabricated-precision rule)")
    for arm, row in invented.items():
        print(f"  {arm:10s} {row['outputs_with_invented_quantity']}/{row['outputs_checked']} outputs ({row['rate_pct']}%)")

    print("\nCONFIDENCE SCALE (reported separately; production bug not fixed here)")
    for arm in ("control", "treatment"):
        scale = report["confidence_scale"][arm] or {}
        print(f"  {arm:10s} emitted {scale.get('emitted')}, within 0..1 {scale.get('in_0_1')}, "
              f"outside {scale.get('out_of_0_1')} {scale.get('out_of_range_values')}")

    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(report, indent=2), encoding="utf-8")
    print(f"\nwritten to {args.out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
