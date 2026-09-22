#!/usr/bin/env python3
"""Expands authored scenario families into training records.

# Authored scenarios, controlled variation — not blind generation

Every record traces back to a hand-written scenario family in
`scenarios/`. This script does not invent situations; it fills slots and
applies surface-style transforms to text a human wrote, which is the only
kind of generation that can be checked by reading the source.

Two things are deliberately separated:

- **Meaning** comes from the authored level (`severity`, `access_status`,
  `category`, and the fields of the target). A generated record can never
  have a label its family did not author.
- **Surface form** comes from slot fills and style transforms. These change
  how a report reads — abbreviations, typos, terseness, informal markers —
  and never change what the correct answer is.

That separation is what makes noise safe to add in bulk: a typo cannot
silently relabel an example, because the label was never derived from the
text in the first place.

# Determinism

Slot choices come from a SplitMix64 PRNG seeded by a stable hash of
`(group, level, phrasing, style)`, so the whole corpus reproduces byte for
byte on any machine, and a diff between two runs means the *authored
scenarios* changed. This mirrors `src-tauri/src/ai/dataset.rs`, which
specifies its own PRNG for exactly this reason rather than depending on one
whose output could drift with a library update.

# Offline

Standard library only. Reads authored Python modules, writes one JSONL file.
No network access, no model, no downloads.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
from pathlib import Path

SCRIPT_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPT_DIR))

from scenarios import ALL_FAMILIES, GLOBAL_SLOTS  # noqa: E402
from scenarios.schema import KIND_LADDER, KIND_MULTI_MESSAGE, Family, Level  # noqa: E402

TRAINING_ROOT = SCRIPT_DIR.parent
DEFAULT_OUT = TRAINING_ROOT / "data" / "raw" / "incidents_v2_generated.jsonl"

SLOT_PATTERN = re.compile(r"\{(\w+)\}")


class Rng:
    """SplitMix64, specified here so its output cannot drift.

    Same algorithm as `Rng` in src-tauri/src/ai/dataset.rs, for the same
    reason: a reproducible corpus is worth more than a convenient import.
    """

    def __init__(self, seed: int) -> None:
        self.state = (seed + 0x9E3779B97F4A7C15) & 0xFFFFFFFFFFFFFFFF

    def next(self) -> int:
        self.state = (self.state + 0x9E3779B97F4A7C15) & 0xFFFFFFFFFFFFFFFF
        z = self.state
        z = ((z ^ (z >> 30)) * 0xBF58476D1CE4E5B9) & 0xFFFFFFFFFFFFFFFF
        z = ((z ^ (z >> 27)) * 0x94D049BB133111EB) & 0xFFFFFFFFFFFFFFFF
        return z ^ (z >> 31)

    def pick(self, options: list[str]) -> str:
        return options[self.next() % len(options)]


def stable_seed(*parts: object) -> int:
    """A reproducible 64-bit seed from arbitrary parts.

    `hash()` is deliberately not used: Python randomises string hashing per
    process, which would make the corpus differ between runs.
    """
    joined = "|".join(str(p) for p in parts)
    digest = hashlib.sha256(joined.encode("utf-8")).digest()
    return int.from_bytes(digest[:8], "big")


# --- Style transforms -----------------------------------------------------
#
# Applied to `report_text` only. The target is always written in clean,
# complete language, which is the asymmetry the model has to learn: read
# whatever the field sends, write something a schema will accept.

ABBREVIATIONS = {
    "people": "ppl",
    "person": "persn",
    "near": "nr",
    "building": "bldg",
    "buildings": "bldgs",
    "before": "b4",
    "road": "rd",
    "about": "abt",
    "without": "w/o",
    "with": "w/",
    "approximately": "approx",
    "hours": "hrs",
    "hour": "hr",
    "minutes": "mins",
    "minute": "min",
    "tomorrow": "tmrw",
    "tonight": "tonite",
    "because": "bc",
    "please": "pls",
    "information": "info",
    "equipment": "equip",
    "temperature": "temp",
    "vehicle": "veh",
    "vehicles": "vehs",
    "evacuation": "evac",
    "evacuating": "evac",
    "evacuated": "evac'd",
    "department": "dept",
    "something": "smth",
    "another": "anothr",
    "against": "agnst",
    "through": "thru",
    "though": "tho",
    "right now": "rn",
    "and": "n",
    "are": "r",
    "you": "u",
    "your": "ur",
    "number": "no.",
    "kilometres": "km",
    "metres": "m",
    "confirmed": "confd",
    "structural": "struct",
    "restricted": "restr",
    "immediately": "immed",
    "situation": "sitrep",
    "casualty": "cas",
    "casualties": "cas",
}

# Neutral radio-traffic fillers. Deliberately excludes anything that would
# assert urgency ("urgent", "emergency", "mayday"): a filler that implied a
# severity would let style leak into the label, which is the one thing these
# transforms must never do.
NEUTRAL_MARKERS = ["fyi", "over", "pls advise", "any update", "copy", "ends", "msg ends"]

# Intensity punctuation is applied independently of severity on purpose.
# Real field traffic punctuates inconsistently, and a model that reads "!!"
# as CRITICAL is reading the sender's mood rather than the evidence.
PUNCTUATION_NOISE = ["!!", "!", "...", ""]


def apply_abbreviations(text: str) -> str:
    def replace(match: re.Match[str]) -> str:
        word = match.group(0)
        lowered = word.lower()
        if lowered in ABBREVIATIONS:
            return ABBREVIATIONS[lowered]
        return word

    return re.sub(r"\b[a-zA-Z]+\b", replace, text)


def apply_typos(text: str, rng: Rng) -> str:
    """Perturbs a deterministic subset of words.

    Words containing digits are never touched: a corrupted number would turn
    a correct extraction target into a fabricated one, which is precisely the
    failure this dataset exists to train against.
    """
    words = text.split(" ")
    out = []
    for word in words:
        core = word.strip(".,!?;:")
        if len(core) >= 5 and core.isalpha() and rng.next() % 3 == 0:
            choice = rng.next() % 3
            if choice == 0:  # drop a vowel
                for i, ch in enumerate(core[1:], start=1):
                    if ch in "aeiou":
                        core = core[:i] + core[i + 1 :]
                        break
            elif choice == 1:  # swap two adjacent characters
                i = 1 + (rng.next() % max(1, len(core) - 2))
                core = core[:i] + core[i + 1] + core[i] + core[i + 2 :]
            else:  # double a letter
                i = rng.next() % len(core)
                core = core[:i] + core[i] + core[i:]
            word = core
        out.append(word)
    return " ".join(out)


def style_plain(text: str, _rng: Rng) -> str:
    return text


def style_abbrev(text: str, rng: Rng) -> str:
    abbreviated = apply_abbreviations(text)
    marker = rng.pick(NEUTRAL_MARKERS)
    # A sentence with nothing abbreviatable would otherwise come out
    # identical to its plain variant, producing a duplicate record that
    # teaches nothing. Appending the marker guarantees it differs.
    if abbreviated == text or rng.next() % 2 == 0:
        return f"{abbreviated} {marker}"
    return abbreviated


def style_noisy(text: str, rng: Rng) -> str:
    """Abbreviated, mistyped, punctuated at random, and often clipped short.

    The clipping is what produces genuinely terse traffic — the Phase 1 set
    had none, every report sitting in a narrow 77-130 character band.
    """
    text = apply_abbreviations(text)
    text = text.replace("'", "") if rng.next() % 2 == 0 else text
    text = apply_typos(text, rng)
    if rng.next() % 3 == 0:
        # Drop articles and low-content words for a clipped, radio-like feel.
        text = re.sub(r"\b(the|a|an|that|there is|there are)\b", "", text)
        text = re.sub(r"\s{2,}", " ", text).strip()
    text = text.lower()
    punctuation = rng.pick(PUNCTUATION_NOISE)
    if punctuation:
        text = text.rstrip(".") + punctuation
    if rng.next() % 3 == 0:
        text = f"{text} {rng.pick(NEUTRAL_MARKERS)}"
    return text


STYLES = {
    "plain": style_plain,
    "abbrev": style_abbrev,
    "noisy": style_noisy,
}

# Ladder levels are the contrastive backbone and get the widest surface
# variation; the special kinds (ambiguous, conflicting, false-positive,
# multi-message, info) get fewer because their value is in what they say,
# not in how many ways it can be spelled.
LADDER_STYLES = ["plain", "abbrev", "noisy"]
SPECIAL_STYLES = ["plain", "noisy"]


# --- Slot filling ---------------------------------------------------------


def slot_names(*texts: object) -> set[str]:
    names: set[str] = set()
    for text in texts:
        if isinstance(text, str):
            names.update(SLOT_PATTERN.findall(text))
        elif isinstance(text, (list, tuple)):
            names.update(slot_names(*text))
    return names


def resolve_slots(family: Family, level: Level, phrasing: str, rng: Rng) -> dict[str, str]:
    """Chooses one value per slot used anywhere in this record.

    Resolved in sorted name order so the assignment does not depend on which
    field happens to be formatted first — without that, `{loc}` in the report
    and `{loc}` in the summary could diverge.
    """
    available = {**GLOBAL_SLOTS, **family.slots}
    used = slot_names(
        phrasing, level.summary, level.cause, level.asset, level.hint, level.entities, level.resources
    )
    resolved: dict[str, str] = {}
    for name in sorted(used):
        options = available.get(name)
        if not options:
            raise KeyError(f"{family.group}: slot {{{name}}} has no values defined")
        resolved[name] = rng.pick(options)
    return resolved


DOUBLED_ARTICLE = re.compile(r"\b(the|a|an)\s+the\b", re.IGNORECASE)


def fill(text: str | None, slots: dict[str, str]) -> str | None:
    """Substitutes slots and repairs the one grammatical seam they create.

    Half the `loc` values begin with "the" ("the north camp"), and many
    authored templates place a slot after an article ("in the {loc} office").
    Collapsing the doubled article is cheaper and reads better than
    maintaining two parallel vocabularies of place names.
    """
    if text is None:
        return None
    filled = text.format(**slots)
    return DOUBLED_ARTICLE.sub(lambda m: m.group(1), filled)


# --- Feature tagging ------------------------------------------------------


def feature_tags(report_text: str, target: dict) -> list[str]:
    """Tags recording what a record actually exercises.

    Used by `dataset_stats.py` to report coverage of the capabilities this
    dataset is meant to teach, rather than asserting that coverage exists.
    """
    tags = []
    if any(character.isdigit() for character in report_text):
        tags.append("has_numbers")
    if target.get("entities"):
        tags.append("has_entities")
        if any(any(c.isdigit() for c in entity) for entity in target["entities"]):
            tags.append("has_people_count")
    if target.get("affected_resources"):
        tags.append("has_resource_request")
    if target.get("location_hint"):
        tags.append("has_location")
    if "\n" in report_text:
        tags.append("multi_message")
    if len(report_text) < 70:
        tags.append("short_report")
    elif len(report_text) > 160:
        tags.append("long_report")
    return tags


def build_record(
    family: Family, level_index: int, level: Level, phrasing_index: int, phrasing: str, style: str
) -> dict:
    seed = stable_seed(family.group, level_index, phrasing_index, style)
    rng = Rng(seed)
    slots = resolve_slots(family, level, phrasing, rng)

    report_text = STYLES[style](fill(phrasing, slots) or "", rng).strip()

    target: dict = {
        "category": family.category,
        "severity": level.severity,
        "summary": fill(level.summary, slots),
        "access_status": level.access_status,
    }
    for key, value in (
        ("asset", fill(level.asset, slots)),
        ("cause", fill(level.cause, slots)),
        ("location_hint", fill(level.hint, slots)),
    ):
        if value:
            target[key] = value
    if level.entities:
        target["entities"] = [fill(entity, slots) for entity in level.entities]
    if level.resources:
        target["affected_resources"] = [fill(resource, slots) for resource in level.resources]

    variation = [style, level.kind, *family.tags, *feature_tags(report_text, target)]
    # Style names double as the variation vocabulary the Phase 1 set used, so
    # statistics over both generations are comparable.
    if style == "noisy":
        variation.extend(["typo", "abbreviation", "informal"])
    elif style == "abbrev":
        variation.append("abbreviation")

    record = {
        "id": f"gen-{family.group}-l{level_index}-p{phrasing_index}-{style}",
        "report_text": report_text,
        "target": target,
        "provenance": "synthetic",
        "scenario_group": family.group,
        "variation": sorted(set(variation)),
    }
    if level.note:
        record["notes"] = level.note
    return record


def generate() -> list[dict]:
    records = []
    seen_ids = set()
    # Safety net for duplicate report text. Style transforms are applied to
    # authored sentences, and two transforms can coincide on a sentence that
    # offers them nothing to change. A duplicate record adds training weight
    # without adding information, so it is dropped rather than emitted.
    seen_texts: set[str] = set()
    for family in ALL_FAMILIES:
        for level_index, level in enumerate(family.levels):
            styles = LADDER_STYLES if level.kind == KIND_LADDER else SPECIAL_STYLES
            for phrasing_index, phrasing in enumerate(level.phrasings):
                for style in styles:
                    # Multi-message reports are already long and structured;
                    # abbreviating and clipping them as well would obscure the
                    # message boundaries that make them worth having.
                    if level.kind == KIND_MULTI_MESSAGE and style == "noisy":
                        style = "abbrev"
                    record = build_record(
                        family, level_index, level, phrasing_index, phrasing, style
                    )
                    text_key = record["report_text"].lower().strip()
                    if record["id"] in seen_ids or text_key in seen_texts:
                        continue
                    seen_ids.add(record["id"])
                    seen_texts.add(text_key)
                    records.append(record)
    return records


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", type=Path, default=DEFAULT_OUT)
    args = parser.parse_args()

    records = generate()
    args.out.parent.mkdir(parents=True, exist_ok=True)
    with args.out.open("w", encoding="utf-8") as handle:
        for record in records:
            handle.write(json.dumps(record, ensure_ascii=False, sort_keys=True))
            handle.write("\n")

    families = len(ALL_FAMILIES)
    levels = sum(len(f.levels) for f in ALL_FAMILIES)
    print(f"{families} authored families, {levels} evidence levels")
    print(f"{len(records)} records written to {args.out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
