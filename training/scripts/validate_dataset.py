#!/usr/bin/env python3
"""Validates SecureMesh-SLM training records against the existing Rust schema.

# Why this exists

SecureMesh already has one schema for what a model may say about an incident:
`RawAnalysis` / `IncidentAnalysis` in `src-tauri/src/domain/intelligence.rs`,
enforced at the runtime boundary by `RawAnalysis::validate()` and offered to
the model as JSON Schema by `analysis_schema()` in `src-tauri/src/ai/prompt.rs`.

This file is the Python-side mirror of those same rules, so a training example
that would be rejected by the running application cannot be used to fine-tune
a model into producing it. It is deliberately NOT a new schema — every field,
enum value and length bound below is copied from, and cites, the Rust source
it must stay in sync with.

# Keeping this in sync

Python cannot import Rust. If `IncidentCategory`, `AccessStatus`, `Severity`,
or any bound in `RawAnalysis`/`intelligence.rs` changes, this file must be
updated by hand. `training/README.md` lists exactly what to check.

# No network access

This script reads local files only. It imports nothing beyond the Python
standard library, so running it requires no dependency installation and can
never reach the network.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
from collections import Counter, defaultdict
from pathlib import Path
from typing import Any, Iterable

# --- Mirrored from src-tauri/src/domain/intelligence.rs -------------------

# IncidentCategory::ALL / as_str (intelligence.rs:69-99)
CATEGORY_VALUES = {
    "INFRASTRUCTURE",
    "FLOODING",
    "FIRE",
    "MEDICAL",
    "EVACUATION",
    "POWER",
    "COMMUNICATIONS",
    "RESOURCE_SHORTAGE",
    "EARTHQUAKE",
    "SEVERE_WEATHER",
    "ROAD_BLOCKAGE",
    "OTHER",
}

# AccessStatus::schema_values (intelligence.rs:204-209)
ACCESS_STATUS_VALUES = {"OPEN", "RESTRICTED", "BLOCKED", "UNKNOWN"}

# Severity::ALL / as_str (src-tauri/src/domain/incident.rs:28-42)
SEVERITY_VALUES = {"LOW", "MEDIUM", "HIGH", "CRITICAL"}

# Bounds from intelligence.rs:41-44
MAX_SUMMARY_CHARS = 1_000
MAX_FIELD_CHARS = 200
MAX_ENTITIES = 32

# Bound on the report text itself, from src-tauri/src/ai/prompt.rs:31
# (MAX_INCIDENT_CHARS — the longest text ever handed to the model).
MAX_REPORT_CHARS = 4_000

# The exact optional-field set RawAnalysis accepts (intelligence.rs:239-254).
# `deny_unknown_fields` on the Rust struct means a model — or a training
# record — cannot smuggle in a field the runtime doesn't know about; this
# validator enforces the same closed set.
TARGET_FIELDS = {
    "category",
    "severity",
    "summary",
    "asset",
    "cause",
    "access_status",
    "entities",
    "affected_resources",
    "location_hint",
    "confidence",
}
# summary and severity are required by RawAnalysis::validate (intelligence.rs:276,291).
TARGET_REQUIRED_FIELDS = {"summary", "severity"}

# --- Provenance discipline (new to this pipeline, not in the Rust schema) --

ALLOWED_PROVENANCE = {"synthetic", "real", "operator_corrected"}

# Names a hand-authored / synthetic example must never claim, mirroring the
# discipline already applied to the operational knowledge pack
# (ARCHITECTURE.md 6d.4: "Not sourced from NDMA, NDRF, FEMA, the IFRC or any
# other authority"). A synthetic training example that quotes a real agency
# as its source would be indistinguishable from real doctrine once it starts
# shaping model behaviour, which is exactly what must never happen.
DISALLOWED_AUTHORITY_CLAIMS = [
    "ndma",
    "ndrf",
    "fema",
    "ifrc",
    "government of india",
    "ministry of home affairs",
    "official government",
    "national disaster management",
]

TOP_LEVEL_REQUIRED_FIELDS = {"id", "report_text", "target", "provenance", "scenario_group"}
TOP_LEVEL_OPTIONAL_FIELDS = {"variation", "notes", "source"}
TOP_LEVEL_FIELDS = TOP_LEVEL_REQUIRED_FIELDS | TOP_LEVEL_OPTIONAL_FIELDS

# --- Corpus-level quality thresholds --------------------------------------
#
# These govern checks that cannot be made against a single record: whether
# two records are near-duplicates of each other, and whether the corpus as a
# whole is balanced enough to train on. They are reported by
# `--corpus-checks` and are advisory by default, because "imbalanced" is a
# judgement about a dataset rather than a defect in a record.

# Jaccard similarity over word shingles above which two reports from
# *different* scenario groups are treated as near-duplicates. Records inside
# one group are expected to be similar — that is what a scenario family is —
# so the check deliberately only looks across groups.
NEAR_DUPLICATE_THRESHOLD = 0.80

# How far a category's share may sit below a perfectly even split before it
# is reported as under-represented. 0.5 means "no category may hold less
# than half the share it would have if every category were equal".
MIN_SHARE_OF_EVEN = 0.5

# A sentence appearing verbatim across more than this many distinct scenario
# groups suggests a template leaking between families rather than genuinely
# independent authorship.
MAX_GROUPS_SHARING_A_SENTENCE = 3

# Number words accepted as support for a numeric claim, so a target saying
# "2 unresponsive" is supported by a report saying "two unresponsive".
NUMBER_WORDS = {
    "one": 1, "two": 2, "three": 3, "four": 4, "five": 5, "six": 6,
    "seven": 7, "eight": 8, "nine": 9, "ten": 10, "eleven": 11, "twelve": 12,
    "thirteen": 13, "fourteen": 14, "fifteen": 15, "sixteen": 16,
    "seventeen": 17, "eighteen": 18, "nineteen": 19, "twenty": 20,
    "thirty": 30, "forty": 40, "fifty": 50, "sixty": 60, "eighty": 80,
    "hundred": 100,
}


class ValidationError(list):
    """A list of human-readable problems with one record. Empty means valid."""


def _bounded_text_ok(value: Any, limit: int, field: str, errors: list[str]) -> None:
    if value is None:
        return
    if not isinstance(value, str):
        errors.append(f"{field}: must be a string or null, got {type(value).__name__}")
        return
    if len(value) > limit:
        errors.append(f"{field}: {len(value)} chars, over the {limit}-char limit")
    if value.strip() == "" and value != "":
        errors.append(f"{field}: whitespace-only string; use null instead")


def _bounded_list_ok(value: Any, field: str, errors: list[str]) -> None:
    if value is None:
        return
    if not isinstance(value, list):
        errors.append(f"{field}: must be a list, got {type(value).__name__}")
        return
    if len(value) > MAX_ENTITIES:
        errors.append(f"{field}: {len(value)} items, over the {MAX_ENTITIES}-item limit")
    for i, item in enumerate(value):
        if not isinstance(item, str):
            errors.append(f"{field}[{i}]: must be a string, got {type(item).__name__}")
        elif len(item) > MAX_FIELD_CHARS:
            errors.append(f"{field}[{i}]: {len(item)} chars, over the {MAX_FIELD_CHARS}-char limit")


def validate_target(target: Any) -> list[str]:
    """Checks one `target` object against the RawAnalysis shape.

    Mirrors `RawAnalysis::validate` in intelligence.rs: summary and severity
    are required and must be well-formed; category and access_status must be
    members of the closed sets the schema offers the model (not the looser
    `FromStr` fallback the Rust side uses for *model* output, since a
    training target must be exactly right, not merely recoverable).
    """
    errors: list[str] = []
    if not isinstance(target, dict):
        return [f"target: must be an object, got {type(target).__name__}"]

    unknown = set(target.keys()) - TARGET_FIELDS
    if unknown:
        errors.append(f"target: unknown field(s) {sorted(unknown)} — not in RawAnalysis")

    missing = TARGET_REQUIRED_FIELDS - set(target.keys())
    if missing:
        errors.append(f"target: missing required field(s) {sorted(missing)}")

    summary = target.get("summary")
    if "summary" in target:
        if not isinstance(summary, str) or summary.strip() == "":
            errors.append("target.summary: required, must be a non-empty string")
        elif len(summary) > MAX_SUMMARY_CHARS:
            errors.append(f"target.summary: {len(summary)} chars, over {MAX_SUMMARY_CHARS}")

    severity = target.get("severity")
    if "severity" in target:
        if not isinstance(severity, str) or severity.upper() not in SEVERITY_VALUES:
            errors.append(f"target.severity: must be one of {sorted(SEVERITY_VALUES)}, got {severity!r}")

    category = target.get("category")
    if category is not None:
        if not isinstance(category, str) or category.upper() not in CATEGORY_VALUES:
            errors.append(f"target.category: must be one of {sorted(CATEGORY_VALUES)}, got {category!r}")

    access_status = target.get("access_status")
    if access_status is not None:
        if not isinstance(access_status, str) or access_status.upper() not in ACCESS_STATUS_VALUES:
            errors.append(
                f"target.access_status: must be one of {sorted(ACCESS_STATUS_VALUES)}, got {access_status!r}"
            )

    _bounded_text_ok(target.get("asset"), MAX_FIELD_CHARS, "target.asset", errors)
    _bounded_text_ok(target.get("cause"), MAX_FIELD_CHARS, "target.cause", errors)
    _bounded_text_ok(target.get("location_hint"), MAX_FIELD_CHARS, "target.location_hint", errors)
    _bounded_list_ok(target.get("entities"), "target.entities", errors)
    _bounded_list_ok(target.get("affected_resources"), "target.affected_resources", errors)

    confidence = target.get("confidence")
    if confidence is not None:
        if not isinstance(confidence, (int, float)) or isinstance(confidence, bool):
            errors.append("target.confidence: must be a number")
        elif not (0.0 <= float(confidence) <= 1.0):
            errors.append(f"target.confidence: {confidence} is outside 0.0..=1.0")

    return errors


def _numbers_in(text: str) -> set[int]:
    """Integers written as digits. `4th` and `19100` inside a word are not
    matches; only standalone numbers count."""
    return {int(match) for match in re.findall(r"\b(\d+)\b", text)}


def supported_numbers(report_text: str) -> set[int]:
    """Numbers a report actually establishes, in digits or in words.

    Deliberately more generous than `_numbers_in`: any run of digits counts,
    including ones fused to a unit ("90kmh", "3hrs") or inside a message
    timestamp. The asymmetry is the point — be liberal about what a noisy
    field report can be read as stating, and strict about what a target is
    allowed to claim.
    """
    numbers = {int(match) for match in re.findall(r"\d+", report_text)}
    lowered = report_text.lower()
    for word, value in NUMBER_WORDS.items():
        if re.search(rf"\b{word}\b", lowered):
            numbers.add(value)
    return numbers


def unsupported_number_claims(record: dict) -> list[str]:
    """Flags a target asserting a quantity its report never gave.

    This is the fabricated-precision failure: a report saying "several people
    are hurt" and a target saying "12 people injured". The target would look
    perfectly well-formed to every other check here, and training on it
    teaches a model to invent figures — the opposite of what an emergency
    record needs.

    **Scoped to numbers of two or more, deliberately.** English expresses
    "one" without a numeral constantly — "someone collapsed", "a driver is
    out of contact", "the team is overdue" — so mechanically demanding a
    literal `1` in the text produces a flood of false positives while
    catching no real fabrication. Quantities of two and above are where
    invented precision actually appears, and those are checked strictly.
    """
    target = record.get("target")
    report_text = record.get("report_text")
    if not isinstance(target, dict) or not isinstance(report_text, str):
        return []

    available = supported_numbers(report_text)
    problems = []

    claimed: list[tuple[str, int]] = []
    for entity in target.get("entities", []) or []:
        if isinstance(entity, str):
            claimed.extend((f"entities ({entity!r})", n) for n in _numbers_in(entity))
    summary = target.get("summary")
    if isinstance(summary, str):
        claimed.extend(("summary", n) for n in _numbers_in(summary))

    for where, number in claimed:
        if number >= 2 and number not in available:
            problems.append(
                f"target.{where}: claims the quantity {number}, which the report never states"
            )
    return problems


def validate_record(record: Any) -> list[str]:
    """Checks one full training record: envelope, provenance, and target."""
    errors: list[str] = []
    if not isinstance(record, dict):
        return [f"record: must be a JSON object, got {type(record).__name__}"]

    unknown = set(record.keys()) - TOP_LEVEL_FIELDS
    if unknown:
        errors.append(f"record: unknown top-level field(s) {sorted(unknown)}")

    missing = TOP_LEVEL_REQUIRED_FIELDS - set(record.keys())
    if missing:
        errors.append(f"record: missing required field(s) {sorted(missing)}")
        return errors  # Nothing further can be checked meaningfully.

    if not isinstance(record.get("id"), str) or not record["id"].strip():
        errors.append("id: must be a non-empty string")

    report_text = record.get("report_text")
    if not isinstance(report_text, str) or not report_text.strip():
        errors.append("report_text: must be a non-empty string")
    elif len(report_text) > MAX_REPORT_CHARS:
        errors.append(f"report_text: {len(report_text)} chars, over the {MAX_REPORT_CHARS}-char limit")

    scenario_group = record.get("scenario_group")
    if not isinstance(scenario_group, str) or not scenario_group.strip():
        errors.append("scenario_group: must be a non-empty string (used to prevent split leakage)")

    provenance = record.get("provenance")
    if provenance not in ALLOWED_PROVENANCE:
        errors.append(f"provenance: must be one of {sorted(ALLOWED_PROVENANCE)}, got {provenance!r}")

    # A record claiming to be real, or a correction of a real analysis, must
    # say where it actually came from. Without this, "real" is just a label
    # anyone could type, which is the failure mode this field exists to rule
    # out (see docs/ai/FINETUNING.md, "Provenance is load-bearing").
    if provenance in ("real", "operator_corrected"):
        source = record.get("source")
        if not isinstance(source, str) or not source.strip():
            errors.append(
                f"source: required and must be non-empty when provenance is {provenance!r}"
            )

    # A synthetic (hand-authored) record must never read as though it carries
    # real agency authority — see ARCHITECTURE.md 6d.4 for the same rule
    # applied to the operational knowledge pack.
    if provenance == "synthetic":
        haystack = " ".join(
            str(record.get(field, "") or "")
            for field in ("report_text",)
        ).lower()
        target = record.get("target") or {}
        if isinstance(target, dict):
            haystack += " " + str(target.get("summary", "") or "").lower()
        for claim in DISALLOWED_AUTHORITY_CLAIMS:
            if claim in haystack:
                errors.append(
                    f"provenance is 'synthetic' but text claims real authority ({claim!r}); "
                    "synthetic data must never be represented as real-world doctrine or fact"
                )

    variation = record.get("variation")
    if variation is not None and not (
        isinstance(variation, list) and all(isinstance(v, str) for v in variation)
    ):
        errors.append("variation: must be a list of strings if present")

    if "notes" in record and record["notes"] is not None and not isinstance(record["notes"], str):
        errors.append("notes: must be a string if present")

    errors.extend(validate_target(record.get("target")))
    errors.extend(unsupported_number_claims(record))
    return errors


# --- Corpus-level checks --------------------------------------------------
#
# Everything above judges one record in isolation. These judge the corpus:
# duplication between records, leakage between scenario groups, and whether
# the label distributions are balanced enough to train on.


def normalise_for_comparison(text: str) -> str:
    """Strips the surface noise style transforms add, so a near-duplicate is
    detected by what it says rather than by how it was mistyped."""
    lowered = text.lower()
    lowered = re.sub(r"\[\d{3,4}\]|\b\d{3,4}\s*-\s*", " ", lowered)  # message timestamps
    lowered = re.sub(r"[^a-z0-9\s]", " ", lowered)
    return re.sub(r"\s+", " ", lowered).strip()


def shingles(text: str, size: int = 3) -> set[str]:
    words = normalise_for_comparison(text).split()
    if len(words) < size:
        return {" ".join(words)} if words else set()
    return {" ".join(words[i : i + size]) for i in range(len(words) - size + 1)}


def jaccard(left: set[str], right: set[str]) -> float:
    if not left or not right:
        return 0.0
    intersection = len(left & right)
    if not intersection:
        return 0.0
    return intersection / len(left | right)


def find_near_duplicates(records: list[dict], threshold: float = NEAR_DUPLICATE_THRESHOLD):
    """Finds highly similar reports that sit in *different* scenario groups.

    Similarity inside a group is expected and wanted — a scenario family is
    deliberately several phrasings of one situation, and `build_dataset.py`
    keeps a whole group inside one split. Similarity *across* groups is the
    dangerous kind, because those records can be separated into train and
    test, which would make a test score partly a memory test.

    Candidates are narrowed through an inverted index on shingles, so this
    does not compare every record against every other.
    """
    prepared = [(record, shingles(record.get("report_text", ""))) for record in records]

    index: dict[str, list[int]] = defaultdict(list)
    for position, (_record, shingle_set) in enumerate(prepared):
        for shingle in shingle_set:
            index[shingle].append(position)

    findings = []
    compared: set[tuple[int, int]] = set()
    for position, (record, shingle_set) in enumerate(prepared):
        candidates: set[int] = set()
        for shingle in shingle_set:
            candidates.update(index[shingle])
        for other in candidates:
            if other <= position:
                continue
            pair = (position, other)
            if pair in compared:
                continue
            compared.add(pair)
            other_record, other_shingles = prepared[other]
            if record.get("scenario_group") == other_record.get("scenario_group"):
                continue
            score = jaccard(shingle_set, other_shingles)
            if score >= threshold:
                findings.append(
                    (record.get("id"), other_record.get("id"), round(score, 3))
                )
    return findings


def find_exact_duplicates(records: list[dict]) -> list[tuple[str, list[str]]]:
    by_text: dict[str, list[str]] = defaultdict(list)
    for record in records:
        text = normalise_for_comparison(record.get("report_text", ""))
        by_text[text].append(record.get("id", "?"))
    return [(text, ids) for text, ids in by_text.items() if len(ids) > 1]


def find_shared_sentences(records: list[dict]) -> list[tuple[str, int]]:
    """Finds wording repeated verbatim across many distinct scenario groups.

    A sentence shared by a handful of groups is unremarkable ("road cleared,
    back to normal"). One shared by many suggests a template has leaked
    between families that are supposed to be independently authored.
    """
    groups_by_sentence: dict[str, set[str]] = defaultdict(set)
    for record in records:
        text = record.get("report_text", "")
        group = record.get("scenario_group", "?")
        for sentence in re.split(r"[.\n]", text):
            cleaned = normalise_for_comparison(sentence)
            if len(cleaned.split()) >= 5:
                groups_by_sentence[cleaned].add(group)
    return sorted(
        (
            (sentence, len(groups))
            for sentence, groups in groups_by_sentence.items()
            if len(groups) > MAX_GROUPS_SHARING_A_SENTENCE
        ),
        key=lambda item: -item[1],
    )


def find_imbalance(records: list[dict], field: str, allowed: set[str]) -> list[str]:
    """Reports label values that are absent or well below an even share."""
    counts = Counter(
        str(record.get("target", {}).get(field, "ABSENT")) for record in records
    )
    total = sum(counts.values())
    if not total:
        return []

    even_share = 1.0 / len(allowed)
    floor = even_share * MIN_SHARE_OF_EVEN
    problems = []
    for value in sorted(allowed):
        share = counts.get(value, 0) / total
        if share < floor:
            problems.append(
                f"{field}={value}: {counts.get(value, 0)} records ({share:.1%}), "
                f"below {floor:.1%} of the corpus"
            )
    return problems


def run_corpus_checks(records: list[dict]) -> tuple[list[str], list[str]]:
    """Runs every corpus-level check.

    Returns (errors, warnings). Duplication and cross-group near-duplication
    are errors — they corrupt the train/test separation. Imbalance is a
    warning: it is a judgement about the shape of a dataset, not a defect in
    it, and the right response is usually to author more scenarios rather
    than to reject what exists.
    """
    errors: list[str] = []
    warnings: list[str] = []

    for text, ids in find_exact_duplicates(records):
        errors.append(f"duplicate report_text across {ids}: {text[:70]!r}")

    for left, right, score in find_near_duplicates(records):
        errors.append(
            f"near-duplicate across scenario groups: {left} ~ {right} (similarity {score})"
        )

    for sentence, group_count in find_shared_sentences(records):
        warnings.append(
            f"wording shared by {group_count} scenario groups: {sentence[:70]!r}"
        )

    for field, allowed in (
        ("category", CATEGORY_VALUES),
        ("severity", SEVERITY_VALUES),
        ("access_status", ACCESS_STATUS_VALUES),
    ):
        warnings.extend(find_imbalance(records, field, allowed))

    # The anti-shortcut check: if severity is predictable from category, the
    # corpus still rewards the behaviour Phase 1 accidentally taught. Report
    # any category whose records are dominated by a single severity.
    by_category: dict[str, Counter] = defaultdict(Counter)
    for record in records:
        target = record.get("target", {})
        by_category[str(target.get("category"))][str(target.get("severity"))] += 1
    for category, severities in sorted(by_category.items()):
        total = sum(severities.values())
        if total < 8:
            continue
        top_severity, top_count = severities.most_common(1)[0]
        if top_count / total > 0.6:
            warnings.append(
                f"category {category} is {top_count / total:.0%} {top_severity} "
                f"({top_count}/{total}) — severity may be predictable from category alone"
            )

    return errors, warnings


def load_jsonl(path: Path) -> Iterable[tuple[int, str, Any]]:
    """Yields (line_number, raw_line, parsed_or_exception) for each non-blank line."""
    with path.open("r", encoding="utf-8") as handle:
        for lineno, line in enumerate(handle, start=1):
            stripped = line.strip()
            if not stripped:
                continue
            try:
                yield lineno, stripped, json.loads(stripped)
            except json.JSONDecodeError as exc:
                yield lineno, stripped, exc


def validate_file(path: Path) -> tuple[list[dict], list[tuple[int, str, str]]]:
    """Validates one JSONL file.

    Returns (valid_records, errors). `errors` is a list of
    (line_number, record_id_or_'?', message).
    """
    valid_records: list[dict] = []
    errors: list[tuple[int, str, str]] = []
    seen_ids: dict[str, int] = {}

    for lineno, _raw, parsed in load_jsonl(path):
        if isinstance(parsed, json.JSONDecodeError):
            errors.append((lineno, "?", f"invalid JSON: {parsed}"))
            continue

        record_id = parsed.get("id", "?") if isinstance(parsed, dict) else "?"
        problems = validate_record(parsed)

        if isinstance(parsed, dict) and isinstance(parsed.get("id"), str):
            rid = parsed["id"]
            if rid in seen_ids:
                problems.append(f"id: duplicate of line {seen_ids[rid]}")
            else:
                seen_ids[rid] = lineno

        if problems:
            for problem in problems:
                errors.append((lineno, record_id, problem))
        else:
            valid_records.append(parsed)

    return valid_records, errors


def scenario_bucket(scenario_group: str, boundaries: tuple[int, int] = (70, 85)) -> str:
    """Deterministically assigns a scenario_group to train/val/test.

    Hash-based rather than random-shuffle-based: the same scenario_group
    always lands in the same split regardless of file order, insertion order,
    or how many times this runs, which is what makes leakage prevention a
    property of the data rather than of a particular run. See
    `build_dataset.py` for how this is used and re-verified.
    """
    digest = hashlib.sha256(scenario_group.encode("utf-8")).hexdigest()
    bucket = int(digest[:8], 16) % 100
    train_max, val_max = boundaries
    if bucket < train_max:
        return "train"
    if bucket < val_max:
        return "val"
    return "test"


def _fix_windows_console_encoding() -> None:
    """Avoids mojibake when this prints an em dash to a cp1252 console."""
    for stream in (sys.stdout, sys.stderr):
        reconfigure = getattr(stream, "reconfigure", None)
        if reconfigure is not None:
            try:
                reconfigure(encoding="utf-8")
            except (ValueError, OSError):
                pass


def main() -> int:
    _fix_windows_console_encoding()
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("paths", nargs="+", type=Path, help="One or more JSONL files to validate")
    parser.add_argument(
        "--quiet", action="store_true", help="Print only the summary line, not every error"
    )
    parser.add_argument(
        "--corpus-checks",
        action="store_true",
        help="Also run duplication, leakage and balance checks across all the "
        "files given, treated as one corpus",
    )
    parser.add_argument(
        "--strict",
        action="store_true",
        help="Treat corpus-level warnings (imbalance, shared wording) as failures",
    )
    args = parser.parse_args()

    total_valid = 0
    total_errors = 0
    all_valid: list[dict] = []

    for path in args.paths:
        if not path.exists():
            print(f"ERROR: {path} does not exist")
            total_errors += 1
            continue

        valid_records, errors = validate_file(path)
        total_valid += len(valid_records)
        total_errors += len(errors)
        all_valid.extend(valid_records)

        if not args.quiet:
            for lineno, record_id, message in errors:
                print(f"{path}:{lineno} [{record_id}] {message}")

        print(f"{path}: {len(valid_records)} valid, {len(errors)} error(s)")

    print(f"TOTAL: {total_valid} valid record(s), {total_errors} error(s)")

    if args.corpus_checks and all_valid:
        print(f"\n--- corpus checks over {len(all_valid)} record(s) ---")
        corpus_errors, corpus_warnings = run_corpus_checks(all_valid)

        for message in corpus_errors:
            print(f"CORPUS ERROR: {message}")
        for message in corpus_warnings:
            print(f"CORPUS WARNING: {message}")

        if not corpus_errors and not corpus_warnings:
            print("no duplication, leakage, or imbalance findings")
        else:
            print(
                f"\n{len(corpus_errors)} corpus error(s), {len(corpus_warnings)} warning(s)"
            )

        total_errors += len(corpus_errors)
        if args.strict:
            total_errors += len(corpus_warnings)

    return 1 if total_errors else 0


if __name__ == "__main__":
    sys.exit(main())
