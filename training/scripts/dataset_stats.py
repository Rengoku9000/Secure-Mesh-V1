#!/usr/bin/env python3
"""Reports what the SecureMesh-SLM dataset actually contains.

Distributions, coverage of the capabilities the dataset is meant to teach,
and the one number that matters most for this phase: how predictable
severity is from category alone.

That last figure is the reason this script exists. The Phase 1 corpus scored
well on paper while quietly teaching "MEDICAL means CRITICAL" (3 of its 5
MEDICAL examples were) and "OTHER means LOW" (4 of 5). A model that learns
the shortcut looks accurate on a corpus built around it and fails on the
first field report that breaks the pattern. Measuring the correlation is how
that stays visible.

Standard library only; reads local JSONL and writes a report. No network.
"""

from __future__ import annotations

import argparse
import json
import sys
from collections import Counter, defaultdict
from pathlib import Path

SCRIPT_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPT_DIR))

import validate_dataset as vd  # noqa: E402

TRAINING_ROOT = SCRIPT_DIR.parent

# Report length bands, in characters. Chosen against the Phase 1 corpus,
# whose reports all fell between 77 and 130 characters — the entire dataset
# sat inside one band, which is why length diversity is tracked here.
SHORT_MAX = 80
LONG_MIN = 160

NOISY_TAGS = {"noisy", "typo"}
ABBREVIATION_TAGS = {"abbreviation", "abbrev"}
CONTRASTIVE_KINDS = {"ladder", "ambiguous", "conflicting", "false_positive"}


def load_records(paths: list[Path]) -> list[dict]:
    records = []
    for path in paths:
        with path.open("r", encoding="utf-8") as handle:
            for line in handle:
                line = line.strip()
                if line:
                    records.append(json.loads(line))
    return records


def distribution(records: list[dict], field: str) -> dict[str, int]:
    return dict(
        Counter(str(r.get("target", {}).get(field, "ABSENT")) for r in records).most_common()
    )


def severity_by_category(records: list[dict]) -> dict[str, dict[str, int]]:
    table: dict[str, Counter] = defaultdict(Counter)
    for record in records:
        target = record.get("target", {})
        table[str(target.get("category"))][str(target.get("severity"))] += 1
    return {category: dict(counts) for category, counts in sorted(table.items())}


def category_severity_predictability(records: list[dict]) -> dict:
    """How well you could do by guessing severity from category alone.

    The baseline is what a model that ignores the report entirely would
    score: for each category, always answer its most common severity. A high
    number means the corpus rewards the shortcut. Chance with four severity
    levels is 25%.
    """
    table = severity_by_category(records)
    correct = 0
    total = 0
    worst: list[tuple[str, float, str]] = []
    for category, severities in table.items():
        category_total = sum(severities.values())
        top_severity, top_count = max(severities.items(), key=lambda item: item[1])
        correct += top_count
        total += category_total
        worst.append((category, top_count / category_total, top_severity))
    worst.sort(key=lambda item: -item[1])
    return {
        "shortcut_accuracy_pct": round(100.0 * correct / total, 1) if total else 0.0,
        "chance_pct": 25.0,
        "most_predictable_categories": [
            {"category": c, "share_pct": round(100 * s, 1), "severity": sev}
            for c, s, sev in worst[:3]
        ],
    }


def length_bands(records: list[dict]) -> dict[str, int]:
    bands = Counter()
    for record in records:
        length = len(record.get("report_text", ""))
        if length < SHORT_MAX:
            bands["short"] += 1
        elif length > LONG_MIN:
            bands["long"] += 1
        else:
            bands["medium"] += 1
    return dict(bands)


def tag_counts(records: list[dict]) -> Counter:
    return Counter(tag for record in records for tag in record.get("variation", []) or [])


def feature_counts(records: list[dict], tags: Counter) -> dict[str, int]:
    """Counts of the capabilities this dataset is meant to exercise.

    Counted from the records themselves rather than from their tags wherever
    possible, so a mis-tagged record cannot inflate a figure.
    """
    return {
        "resource_requests": sum(
            1 for r in records if r.get("target", {}).get("affected_resources")
        ),
        "people_counts": sum(
            1
            for r in records
            if any(
                any(ch.isdigit() for ch in entity)
                for entity in r.get("target", {}).get("entities", []) or []
            )
        ),
        "with_entities": sum(1 for r in records if r.get("target", {}).get("entities")),
        "location_references": sum(
            1 for r in records if r.get("target", {}).get("location_hint")
        ),
        "multi_message": sum(1 for r in records if "\n" in r.get("report_text", "")),
        "noisy": sum(
            1 for r in records if NOISY_TAGS & set(r.get("variation", []) or [])
        ),
        "abbreviated": sum(
            1 for r in records if ABBREVIATION_TAGS & set(r.get("variation", []) or [])
        ),
        "contrastive": sum(
            1
            for r in records
            if CONTRASTIVE_KINDS & set(r.get("variation", []) or [])
        ),
        "ambiguous": tags.get("ambiguous", 0),
        "conflicting": tags.get("conflicting", 0),
        "false_positive": tags.get("false_positive", 0),
        "non_emergency_info": tags.get("non_emergency_info", 0),
    }


def scenario_group_stats(records: list[dict]) -> dict:
    per_group = Counter(r.get("scenario_group", "?") for r in records)
    sizes = Counter(per_group.values())
    return {
        "groups": len(per_group),
        "records_per_group_min": min(per_group.values()) if per_group else 0,
        "records_per_group_max": max(per_group.values()) if per_group else 0,
        "records_per_group_mean": round(sum(per_group.values()) / len(per_group), 1)
        if per_group
        else 0,
        "group_size_histogram": dict(sorted(sizes.items())),
    }


def contrastive_groups(records: list[dict]) -> dict:
    """Scenario groups that carry more than one severity.

    These are the contrastive families: the same situation, differing
    evidence, differing correct answer. A group with one severity teaches a
    scenario; a group with several teaches the distinction.
    """
    severities_by_group: dict[str, set[str]] = defaultdict(set)
    for record in records:
        severities_by_group[record.get("scenario_group", "?")].add(
            str(record.get("target", {}).get("severity"))
        )
    multi = {g: sorted(s) for g, s in severities_by_group.items() if len(s) > 1}
    return {
        "groups_with_multiple_severities": len(multi),
        "groups_total": len(severities_by_group),
        "records_in_contrastive_groups": sum(
            1 for r in records if r.get("scenario_group") in multi
        ),
    }


def build_report(records: list[dict]) -> dict:
    tags = tag_counts(records)
    lengths = [len(r.get("report_text", "")) for r in records]
    return {
        "total_examples": len(records),
        "category_distribution": distribution(records, "category"),
        "severity_distribution": distribution(records, "severity"),
        "access_status_distribution": distribution(records, "access_status"),
        "provenance_distribution": dict(
            Counter(str(r.get("provenance")) for r in records).most_common()
        ),
        "scenario_groups": scenario_group_stats(records),
        "contrastive": contrastive_groups(records),
        "severity_by_category": severity_by_category(records),
        "category_severity_shortcut": category_severity_predictability(records),
        "report_length_chars": {
            "min": min(lengths) if lengths else 0,
            "max": max(lengths) if lengths else 0,
            "mean": round(sum(lengths) / len(lengths)) if lengths else 0,
        },
        "report_length_bands": length_bands(records),
        "features": feature_counts(records, tags),
        "variation_tags": dict(tags.most_common()),
    }


def print_human_report(report: dict) -> None:
    print(f"Total examples: {report['total_examples']}")

    print("\nCategory distribution:")
    for name, count in sorted(report["category_distribution"].items()):
        share = 100.0 * count / report["total_examples"]
        print(f"  {name:20s} {count:5d}  ({share:4.1f}%)")

    print("\nSeverity distribution:")
    for name in ("LOW", "MEDIUM", "HIGH", "CRITICAL"):
        count = report["severity_distribution"].get(name, 0)
        share = 100.0 * count / report["total_examples"]
        print(f"  {name:20s} {count:5d}  ({share:4.1f}%)")

    print("\nAccess status distribution:")
    for name, count in sorted(report["access_status_distribution"].items()):
        share = 100.0 * count / report["total_examples"]
        print(f"  {name:20s} {count:5d}  ({share:4.1f}%)")

    print("\nProvenance distribution:")
    for name, count in report["provenance_distribution"].items():
        print(f"  {name:20s} {count:5d}")

    groups = report["scenario_groups"]
    print(
        f"\nScenario groups: {groups['groups']} "
        f"(min {groups['records_per_group_min']}, max {groups['records_per_group_max']}, "
        f"mean {groups['records_per_group_mean']} records per group)"
    )

    contrast = report["contrastive"]
    print(
        f"Contrastive groups: {contrast['groups_with_multiple_severities']} of "
        f"{contrast['groups_total']} carry more than one severity "
        f"({contrast['records_in_contrastive_groups']} records)"
    )

    print("\nSeverity by category (the shortcut check):")
    header = f"  {'category':20s} " + " ".join(f"{s:>9s}" for s in ("LOW", "MEDIUM", "HIGH", "CRITICAL"))
    print(header)
    for category, severities in report["severity_by_category"].items():
        row = " ".join(f"{severities.get(s, 0):9d}" for s in ("LOW", "MEDIUM", "HIGH", "CRITICAL"))
        print(f"  {category:20s} {row}")

    shortcut = report["category_severity_shortcut"]
    print(
        f"\n  Guessing severity from category alone scores "
        f"{shortcut['shortcut_accuracy_pct']}% (chance is {shortcut['chance_pct']}%)."
    )
    for entry in shortcut["most_predictable_categories"]:
        print(
            f"    most predictable: {entry['category']} is {entry['share_pct']}% {entry['severity']}"
        )

    lengths = report["report_length_chars"]
    bands = report["report_length_bands"]
    print(
        f"\nReport length: min {lengths['min']}, max {lengths['max']}, mean {lengths['mean']} chars"
    )
    print(
        f"  short (<{SHORT_MAX}): {bands.get('short', 0)}   "
        f"medium: {bands.get('medium', 0)}   long (>{LONG_MIN}): {bands.get('long', 0)}"
    )

    print("\nCapability coverage:")
    for name, count in report["features"].items():
        print(f"  {name:24s} {count:5d}")


def main() -> int:
    vd._fix_windows_console_encoding()
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "paths",
        nargs="*",
        type=Path,
        default=None,
        help="JSONL files to report on (default: everything in data/raw/)",
    )
    parser.add_argument("--json", type=Path, help="Also write the report as JSON here")
    args = parser.parse_args()

    paths = args.paths or sorted((TRAINING_ROOT / "data" / "raw").glob("*.jsonl"))
    if not paths:
        print("No dataset files found")
        return 1

    records = load_records(paths)
    report = build_report(records)
    report["source_files"] = [p.name for p in paths]

    print(f"Sources: {', '.join(p.name for p in paths)}\n")
    print_human_report(report)

    if args.json:
        args.json.write_text(json.dumps(report, indent=2, sort_keys=True), encoding="utf-8")
        print(f"\nJSON report written to {args.json}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
