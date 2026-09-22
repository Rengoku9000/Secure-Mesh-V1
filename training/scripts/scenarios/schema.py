"""Authoring format for SecureMesh-SLM scenario families.

# What a family is

A *scenario family* is one underlying situation, authored once, with several
**evidence levels**. The levels are the point: they are the same incident
observed with different amounts of established fact, and their severity
differs *because the evidence differs*, not because of the category.

    "burning smell near the store, nothing visible"      -> LOW
    "visible smoke from the store, area cleared"         -> HIGH
    "confirmed fire, flames on two floors, 4 inside"     -> CRITICAL

All three are FIRE. A model that learns "FIRE means CRITICAL" gets two of
them wrong, which is exactly the shortcut the Phase 1 dataset accidentally
rewarded (measured: MEDICAL was 3/5 CRITICAL, OTHER 4/5 LOW).

# One family is one scenario_group

Every record generated from a family carries that family's `group` as its
`scenario_group`, so `build_dataset.py` keeps the whole family — including
every contrastive variant — inside a single split. A paraphrase of a
training example therefore cannot appear in validation or test, while the
contrastive pairs the model needs to learn from stay together where they are
useful.

# Slots are surface variation, never label variation

`{asset}`, `{loc}`, `{count}` and friends are filled deterministically per
record. They change what the report *says*, never what the correct answer
*is* — the same level always produces the same severity, category and access
status whatever the slot values. Anything that would change the label
belongs in a new level, not a new slot value.
"""

from __future__ import annotations

from dataclasses import dataclass, field

# Kinds of level, recorded on each generated record's `variation` tags so the
# statistics report can count them and a reader can find them.
KIND_LADDER = "ladder"
KIND_AMBIGUOUS = "ambiguous"
KIND_CONFLICTING = "conflicting"
KIND_FALSE_POSITIVE = "false_positive"
KIND_MULTI_MESSAGE = "multi_message"
KIND_INFO = "non_emergency_info"

ALL_KINDS = {
    KIND_LADDER,
    KIND_AMBIGUOUS,
    KIND_CONFLICTING,
    KIND_FALSE_POSITIVE,
    KIND_MULTI_MESSAGE,
    KIND_INFO,
}


@dataclass
class Level:
    """One evidence level of a scenario family.

    `phrasings` are independently authored ways the same level gets reported
    — not paraphrases produced by transforming one another. Style variants
    (abbreviation, typos, terseness) are applied on top by the generator and
    are not authored here.
    """

    severity: str
    access_status: str
    phrasings: list[str]
    summary: str
    cause: str | None = None
    asset: str | None = None
    entities: list[str] = field(default_factory=list)
    resources: list[str] = field(default_factory=list)
    hint: str | None = None
    kind: str = KIND_LADDER
    note: str | None = None


@dataclass
class Family:
    """One authored scenario, its evidence levels, and its slot vocabulary."""

    group: str
    category: str
    levels: list[Level]
    slots: dict[str, list[str]] = field(default_factory=dict)
    tags: list[str] = field(default_factory=list)


def L(
    severity: str,
    access_status: str,
    phrasings: list[str],
    summary: str,
    *,
    cause: str | None = None,
    asset: str | None = None,
    entities: tuple[str, ...] | list[str] = (),
    resources: tuple[str, ...] | list[str] = (),
    hint: str | None = None,
    kind: str = KIND_LADDER,
    note: str | None = None,
) -> Level:
    """Terse constructor for a level, so an authored family stays readable."""
    return Level(
        severity=severity,
        access_status=access_status,
        phrasings=list(phrasings),
        summary=summary,
        cause=cause,
        asset=asset,
        entities=list(entities),
        resources=list(resources),
        hint=hint,
        kind=kind,
        note=note,
    )


def F(
    group: str,
    category: str,
    levels: list[Level],
    *,
    slots: dict[str, list[str]] | None = None,
    tags: tuple[str, ...] | list[str] = (),
) -> Family:
    """Terse constructor for a family."""
    return Family(
        group=group,
        category=category,
        levels=list(levels),
        slots=dict(slots or {}),
        tags=list(tags),
    )


# Slot values available to every family unless it overrides them.
#
# Deliberately generic and non-identifying: these name a kind of place, not a
# real one. Nothing here refers to an actual location, and no generated
# record describes an actual event — see the provenance rules in
# training/README.md.
GLOBAL_SLOTS: dict[str, list[str]] = {
    "loc": [
        "Sector 4",
        "Block C",
        "the north camp",
        "Zone 2",
        "the east gate",
        "the lower village",
        "the market area",
        "the school compound",
        "the ridge road",
        "the old depot",
        "the river crossing",
        "the relief camp",
    ],
    "count": ["2", "3", "4", "5", "6", "8", "12", "15", "20", "30", "45", "60"],
    "smallcount": ["2", "3", "4", "5"],
    "bigcount": ["40", "60", "80", "120", "150", "200"],
    "hours": ["2", "3", "6", "8", "12", "18", "24", "36"],
    "team": ["team 2", "the field team", "the west patrol", "the night shift", "team 5"],
}
