"""Authored scenario families for SecureMesh-SLM.

Families are grouped into modules by the kind of situation they describe,
purely for readability — nothing depends on which module a family lives in.
`ALL_FAMILIES` is the single list the generator consumes.

See `schema.py` for what a family is and why evidence levels, rather than
categories, carry the severity decision.
"""

from __future__ import annotations

from .detailed import DETAILED_FAMILIES
from .hazard import HAZARD_FAMILIES
from .infra import INFRA_FAMILIES
from .people import PEOPLE_FAMILIES
from .schema import ALL_KINDS, Family, GLOBAL_SLOTS, Level

ALL_FAMILIES: list[Family] = (
    HAZARD_FAMILIES + PEOPLE_FAMILIES + INFRA_FAMILIES + DETAILED_FAMILIES
)

__all__ = ["ALL_FAMILIES", "ALL_KINDS", "Family", "GLOBAL_SLOTS", "Level"]
