"""Scenario families for the Phase 8 fresh held-out evaluation set.

# Why this file exists separately

Phase 7 removed `confidence` from `ai::prompt::analysis_schema()`, which
changes the constrained-decoding grammar. The Phase 5/6 numbers were measured
under the previous schema and no longer describe the current configuration, so
a fresh measurement is needed.

There was nowhere to take it from. All 168 scenario groups in the corpus are
allocated: 120 to train, 24 to val, 24 to test. Train was learned from, val
selected checkpoint-66, and test is consumed. A "held-out" split carved from
any of them would not be held out.

So these are **new families the model has never seen**, authored in the
existing `scenarios.schema` format and generated through the existing
`build_record()`. They are deliberately **not** added to
`scenarios/__init__.py::ALL_FAMILIES`: that list defines the frozen training
corpus, and appending to it would change what `generate_dataset.py` produces
and break the reproducibility of `incidents_v2_generated.jsonl`.

# Honest statement of bias risk

These were authored *after* the Phase 5/6 results were known. That is a real
risk of unconscious selection toward cases the fine-tuned model handles well.
Three things bound it, and none of them eliminate it:

- the families were written and frozen **before** either model was run;
- the label for each level follows the evidence stated in that level's text,
  by the same rule the training families use, not by what a model might say;
- nothing here is revised after seeing scores. If a family turns out to be
  badly labelled, the finding is reported, not quietly corrected.

A reader must not mistake this for a pre-existing holdout. It is a newly
authored set, and it is only as good as the authoring.

# Coverage

Twenty-four families, two per category across all twelve categories, with the
group prefix `h3_` so no name can collide with the 168 existing groups.
Severity is carried by evidence level rather than by category — the same
contrastive principle the training set uses, so a model that learned
"MEDICAL means CRITICAL" is caught here too.

Phrasings carry no `{slot}` placeholders: `resolve_slots` raises on a slot
with no values defined, and literal text removes that failure mode. Surface
variation still comes from the `plain` / `abbrev` / `noisy` styles.

Any number appearing in a target field also appears in the report text, so
`validate_dataset.unsupported_number_claims` passes — a target may not claim
a count the report never stated.
"""

from __future__ import annotations

from scenarios.schema import (
    KIND_AMBIGUOUS,
    KIND_CONFLICTING,
    KIND_FALSE_POSITIVE,
    KIND_INFO,
    KIND_MULTI_MESSAGE,
    F,
    Family,
    L,
)

# --- INFRASTRUCTURE --------------------------------------------------------

_INFRA = [
    F(
        "h3_infra_stair_tower",
        "INFRASTRUCTURE",
        [
            L(
                "LOW",
                "OPEN",
                ["hairline crack noticed on the outside stair tower, nothing moving"],
                "Hairline crack reported on an external stair tower.",
                asset="stair tower",
                hint="outside stair tower",
            ),
            L(
                "MEDIUM",
                "RESTRICTED",
                ["crack on the stair tower has opened wider since morning, we taped it off"],
                "Widening crack on a stair tower; area taped off.",
                asset="stair tower",
                cause="progressive cracking",
                resources=["stair tower"],
            ),
            L(
                "CRITICAL",
                "BLOCKED",
                [
                    "the stair tower has separated from the building and is leaning, 3 people "
                    "are still on the upper landing and cannot come down"
                ],
                "Stair tower separating from the building with 3 people stranded above.",
                asset="stair tower",
                cause="structural separation",
                entities=["3 people stranded"],
                resources=["stair tower", "upper landing"],
            ),
        ],
    ),
    F(
        "h3_infra_retaining_wall",
        "INFRASTRUCTURE",
        [
            L(
                "MEDIUM",
                "RESTRICTED",
                ["retaining wall behind the store is bulging outward, we moved the stock away"],
                "Retaining wall bulging behind a store; stock relocated.",
                asset="retaining wall",
            ),
            L(
                "HIGH",
                "BLOCKED",
                [
                    "part of the retaining wall has come down across the service lane, no one "
                    "was under it but the lane is impassable"
                ],
                "Retaining wall partially collapsed across a service lane; no casualties.",
                asset="retaining wall",
                cause="wall collapse",
                resources=["service lane"],
            ),
            L(
                "LOW",
                "OPEN",
                ["engineer looked at the retaining wall and signed it off as safe, no action needed"],
                "Retaining wall inspected and cleared by an engineer.",
                asset="retaining wall",
                kind=KIND_INFO,
            ),
        ],
    ),
]

# --- FLOODING --------------------------------------------------------------

_FLOOD = [
    F(
        "h3_flood_pump_station",
        "FLOODING",
        [
            L(
                "LOW",
                "OPEN",
                ["small puddle forming near the pump station inlet, drains still coping"],
                "Minor pooling near a pump station inlet.",
                hint="pump station",
            ),
            L(
                "HIGH",
                "RESTRICTED",
                [
                    "water is over the pump station floor and rising, the control panel is "
                    "about knee height and staff have pulled back"
                ],
                "Pump station flooding with water approaching the control panel.",
                cause="rising water",
                asset="pump station",
                resources=["control panel"],
            ),
            L(
                "CRITICAL",
                "BLOCKED",
                [
                    "pump station is fully submerged, 2 operators were inside when it went "
                    "under and we have not heard from them"
                ],
                "Pump station submerged with 2 operators unaccounted for.",
                cause="submersion",
                entities=["2 operators unaccounted for"],
                resources=["pump station"],
            ),
        ],
    ),
    F(
        "h3_flood_depth_dispute",
        "FLOODING",
        [
            L(
                "MEDIUM",
                "RESTRICTED",
                [
                    "first team says water on the approach is ankle deep, second team says it "
                    "is up to the knee at the same spot, nobody has waded it to settle it"
                ],
                "Conflicting depth reports for the same flooded approach.",
                cause="flooding",
                hint="the approach",
                kind=KIND_CONFLICTING,
                note="Two incompatible depths; neither confirmed.",
            ),
            L(
                "LOW",
                "OPEN",
                ["someone mentioned water near the gate but nobody has actually seen it"],
                "Unconfirmed second-hand mention of water near a gate.",
                kind=KIND_AMBIGUOUS,
            ),
        ],
    ),
]

# --- FIRE ------------------------------------------------------------------

_FIRE = [
    F(
        "h3_fire_battery_store",
        "FIRE",
        [
            L(
                "MEDIUM",
                "RESTRICTED",
                ["burning smell coming from the battery store, no flame visible yet, door shut"],
                "Burning smell from a battery store with no visible flame.",
                asset="battery store",
            ),
            L(
                "HIGH",
                "BLOCKED",
                [
                    "smoke is now pouring from the battery store vents and the smell is acrid, "
                    "building cleared and nobody went back in"
                ],
                "Heavy smoke from a battery store; building evacuated.",
                cause="suspected battery fire",
                asset="battery store",
                resources=["battery store"],
            ),
            L(
                "CRITICAL",
                "BLOCKED",
                [
                    "flames through the battery store roof, cells are popping and throwing "
                    "sparks, 2 staff have burns and are being moved back"
                ],
                "Battery store fully alight with 2 staff injured.",
                cause="battery fire",
                entities=["2 staff with burns"],
                resources=["battery store"],
            ),
        ],
    ),
    F(
        "h3_fire_alarm_kitchen",
        "FIRE",
        [
            L(
                "LOW",
                "OPEN",
                [
                    "kitchen alarm went off, turned out to be steam from the boiler, no fire, "
                    "alarm reset and everyone back inside"
                ],
                "Kitchen alarm triggered by boiler steam; no fire.",
                cause="steam",
                kind=KIND_FALSE_POSITIVE,
                note="Alarm with a confirmed benign cause.",
            ),
            L(
                "MEDIUM",
                "RESTRICTED",
                ["small pan fire in the kitchen, put out with a blanket, area smoky but clear"],
                "Small pan fire extinguished with a fire blanket.",
                cause="cooking fire",
                asset="kitchen",
            ),
        ],
    ),
]

# --- MEDICAL ---------------------------------------------------------------

_MEDICAL = [
    F(
        "h3_medical_dehydration",
        "MEDICAL",
        [
            L(
                "LOW",
                "OPEN",
                ["one of the volunteers felt faint in the heat, sat down, drinking water now"],
                "Volunteer felt faint from heat and is recovering.",
                entities=["1 volunteer"],
            ),
            L(
                "MEDIUM",
                "OPEN",
                [
                    "4 people from the queue have gone down with heat exhaustion, we are short "
                    "of oral rehydration salts"
                ],
                "4 cases of heat exhaustion; rehydration salts running short.",
                cause="heat exhaustion",
                entities=["4 affected"],
                resources=["oral rehydration salts"],
            ),
            L(
                "CRITICAL",
                "RESTRICTED",
                [
                    "elderly man collapsed at the water point and is not responding properly, "
                    "breathing is shallow, we need an ambulance now"
                ],
                "Unresponsive elderly man with shallow breathing; ambulance required.",
                cause="collapse",
                entities=["1 unresponsive"],
                resources=["ambulance"],
            ),
        ],
    ),
    F(
        "h3_medical_count_dispute",
        "MEDICAL",
        [
            L(
                "HIGH",
                "RESTRICTED",
                [
                    "radio said 6 injured at the site, the team on the ground counts 2 injured "
                    "and 1 walking wounded, we cannot reconcile the numbers yet"
                ],
                "Casualty counts disagree between radio and on-scene team.",
                entities=["2 injured", "1 walking wounded"],
                kind=KIND_CONFLICTING,
                note="Reported 6 versus counted 2 plus 1.",
            ),
            L(
                "MEDIUM",
                "UNKNOWN",
                [
                    "message came through about someone hurt near the depot but the sender did "
                    "not say how badly or whether help arrived"
                ],
                "Unclear report of an injury near a depot.",
                kind=KIND_AMBIGUOUS,
            ),
        ],
    ),
]

# --- EVACUATION ------------------------------------------------------------

_EVAC = [
    F(
        "h3_evac_riverside_row",
        "EVACUATION",
        [
            L(
                "MEDIUM",
                "OPEN",
                ["moving the riverside row to the community hall as a precaution, route is clear"],
                "Precautionary evacuation of riverside housing to a community hall.",
                hint="riverside row",
            ),
            L(
                "HIGH",
                "RESTRICTED",
                [
                    "evacuation of the riverside row is under way, 40 moved so far, the lower "
                    "path is already wet so everyone is going the long way round"
                ],
                "Evacuation under way with 40 moved; lower path unusable.",
                entities=["40 moved"],
                resources=["community hall"],
            ),
            L(
                "LOW",
                "OPEN",
                ["everyone from the riverside row is back home, hall is closed up again"],
                "Riverside residents returned home; shelter closed.",
                kind=KIND_INFO,
            ),
        ],
    ),
    F(
        "h3_evac_multi_stage",
        "EVACUATION",
        [
            L(
                "HIGH",
                "RESTRICTED",
                [
                    "0900 starting to move the north block\n"
                    "0940 about half out, two families refusing to leave\n"
                    "1010 all but one household clear, still working on the last one"
                ],
                "Staged evacuation of the north block with one household remaining.",
                hint="north block",
                entities=["1 household remaining"],
                kind=KIND_MULTI_MESSAGE,
            ),
        ],
    ),
]

# --- POWER -----------------------------------------------------------------

_POWER = [
    F(
        "h3_power_substation",
        "POWER",
        [
            L(
                "LOW",
                "OPEN",
                ["lights dipped twice this evening, came back on their own both times"],
                "Brief voltage dips that self-resolved.",
            ),
            L(
                "HIGH",
                "RESTRICTED",
                [
                    "substation is out and the whole east side has no supply, there is a smell "
                    "of hot insulation near the fence"
                ],
                "Substation failure cutting supply to the east side.",
                cause="substation failure",
                asset="substation",
                resources=["east side supply"],
            ),
            L(
                "CRITICAL",
                "BLOCKED",
                [
                    "live cable down across the yard behind the substation and it is arcing, "
                    "we have pushed everyone back 30 metres and nobody is going near it"
                ],
                "Live downed cable arcing behind a substation; area cleared.",
                cause="downed conductor",
                asset="cable",
                resources=["yard"],
            ),
        ],
    ),
    F(
        "h3_power_generator_fuel",
        "POWER",
        [
            L(
                "MEDIUM",
                "OPEN",
                ["clinic generator is running but we are down to about 4 hours of diesel"],
                "Clinic generator running with roughly 4 hours of fuel left.",
                asset="generator",
                resources=["diesel"],
            ),
            L(
                "LOW",
                "OPEN",
                ["mains came back at the clinic, generator shut down and topped up"],
                "Mains power restored at the clinic; generator stood down.",
                kind=KIND_INFO,
            ),
        ],
    ),
]

# --- COMMUNICATIONS --------------------------------------------------------

_COMMS = [
    F(
        "h3_comms_repeater_site",
        "COMMUNICATIONS",
        [
            L(
                "MEDIUM",
                "OPEN",
                ["repeater is noisy, we can hear the far side but it breaks up every few words"],
                "Degraded repeater audio with frequent break-up.",
                asset="repeater",
            ),
            L(
                "HIGH",
                "RESTRICTED",
                [
                    "repeater is off air completely, the hill teams have no way to reach us "
                    "except by runner"
                ],
                "Repeater off air, leaving hill teams reachable only by runner.",
                cause="repeater failure",
                asset="repeater",
                resources=["radio link"],
            ),
            L(
                "LOW",
                "OPEN",
                ["repeater is back up, everyone checked in fine"],
                "Repeater restored and all stations checked in.",
                kind=KIND_INFO,
            ),
        ],
    ),
    F(
        "h3_comms_silent_team",
        "COMMUNICATIONS",
        [
            L(
                "HIGH",
                "UNKNOWN",
                [
                    "the survey team missed two scheduled check ins and we cannot raise them on "
                    "either channel, last known position was the upper track"
                ],
                "Survey team has missed two check-ins and cannot be raised.",
                cause="missed check-in",
                hint="upper track",
                entities=["1 team unreachable"],
            ),
            L(
                "LOW",
                "OPEN",
                [
                    "survey team called in late, they had walked into a dead spot, all accounted "
                    "for and on their way back"
                ],
                "Survey team reported late after a radio dead spot; all accounted for.",
                kind=KIND_FALSE_POSITIVE,
            ),
        ],
    ),
]

# --- RESOURCE_SHORTAGE -----------------------------------------------------

_RESOURCE = [
    F(
        "h3_resource_water_bowser",
        "RESOURCE_SHORTAGE",
        [
            L(
                "MEDIUM",
                "OPEN",
                ["water bowser at the camp is about a third full, enough for tonight"],
                "Camp water bowser about a third full.",
                resources=["drinking water"],
            ),
            L(
                "HIGH",
                "RESTRICTED",
                [
                    "bowser is nearly dry and the resupply truck has not arrived, roughly 200 "
                    "people here and no other source"
                ],
                "Camp water nearly exhausted for roughly 200 people with resupply overdue.",
                cause="delayed resupply",
                entities=["200 people"],
                resources=["drinking water"],
            ),
        ],
    ),
    F(
        "h3_resource_stock_dispute",
        "RESOURCE_SHORTAGE",
        [
            L(
                "MEDIUM",
                "OPEN",
                [
                    "store list says 12 blanket packs left, the shelf count came to 5, someone "
                    "has moved stock without logging it"
                ],
                "Blanket stock records disagree with the physical count.",
                entities=["5 counted", "12 on the list"],
                resources=["blanket packs"],
                kind=KIND_CONFLICTING,
            ),
            L(
                "LOW",
                "OPEN",
                ["resupply arrived, stores are topped up and the list has been corrected"],
                "Resupply received and stock records corrected.",
                kind=KIND_INFO,
            ),
        ],
    ),
]

# --- EARTHQUAKE ------------------------------------------------------------

_QUAKE = [
    F(
        "h3_quake_aftershock_series",
        "EARTHQUAKE",
        [
            L(
                "LOW",
                "OPEN",
                ["felt a short tremor, nothing fell, people carried on"],
                "Brief tremor felt with no damage.",
            ),
            L(
                "HIGH",
                "RESTRICTED",
                [
                    "second aftershock in an hour, plaster has come down in the hall and the "
                    "back wall has a new crack, everyone is outside"
                ],
                "Aftershock causing fresh cracking; occupants outside.",
                cause="aftershock",
                asset="hall",
                resources=["hall"],
            ),
            L(
                "CRITICAL",
                "BLOCKED",
                [
                    "the old block has partly come down after the last shake, we can hear "
                    "someone calling from under the slab and cannot reach them"
                ],
                "Partial collapse with a person audible and unreachable beneath a slab.",
                cause="structural collapse",
                entities=["1 person trapped"],
                resources=["old block"],
            ),
        ],
    ),
    F(
        "h3_quake_felt_unclear",
        "EARTHQUAKE",
        [
            L(
                "LOW",
                "UNKNOWN",
                ["someone thought they felt shaking but nobody else noticed anything"],
                "Possible tremor reported by one person only.",
                kind=KIND_AMBIGUOUS,
            ),
            L(
                "MEDIUM",
                "OPEN",
                [
                    "1420 shaking felt across the yard\n"
                    "1435 walked the buildings, no visible damage\n"
                    "1450 staff back inside, monitoring for aftershocks"
                ],
                "Tremor felt and buildings checked with no visible damage.",
                kind=KIND_MULTI_MESSAGE,
            ),
        ],
    ),
]

# --- SEVERE_WEATHER --------------------------------------------------------

_WEATHER = [
    F(
        "h3_weather_squall_line",
        "SEVERE_WEATHER",
        [
            L(
                "MEDIUM",
                "OPEN",
                ["wind picking up and the sky has gone dark, rain starting"],
                "Rising wind and darkening sky with rain beginning.",
            ),
            L(
                "HIGH",
                "RESTRICTED",
                [
                    "squall hit hard, roofing sheets are lifting off the sheds and visibility "
                    "is almost nothing, everyone told to stay indoors"
                ],
                "Severe squall lifting roofing sheets with near-zero visibility.",
                cause="squall",
                resources=["sheds"],
            ),
        ],
    ),
    F(
        "h3_weather_forecast_note",
        "SEVERE_WEATHER",
        [
            L(
                "LOW",
                "OPEN",
                ["forecast mentions strong wind tomorrow afternoon, nothing happening yet"],
                "Forecast of strong wind for the following afternoon.",
                kind=KIND_INFO,
            ),
            L(
                "MEDIUM",
                "UNKNOWN",
                ["heard the storm might turn this way but the report was second hand"],
                "Unconfirmed second-hand report of a storm changing direction.",
                kind=KIND_AMBIGUOUS,
            ),
        ],
    ),
]

# --- ROAD_BLOCKAGE ---------------------------------------------------------

_ROAD = [
    F(
        "h3_road_culvert_washout",
        "ROAD_BLOCKAGE",
        [
            L(
                "MEDIUM",
                "RESTRICTED",
                ["edge of the road has crumbled where the culvert runs under, one lane usable"],
                "Road edge crumbling above a culvert; single lane usable.",
                asset="culvert",
                resources=["road"],
            ),
            L(
                "HIGH",
                "BLOCKED",
                [
                    "the culvert has gone and taken the road with it, there is a gap right "
                    "across and nothing can pass"
                ],
                "Culvert washed out leaving a full-width gap in the road.",
                cause="washout",
                asset="culvert",
                resources=["road"],
            ),
            L(
                "LOW",
                "OPEN",
                ["temporary deck is in over the culvert, road open to light vehicles again"],
                "Temporary deck installed; road reopened to light vehicles.",
                kind=KIND_INFO,
            ),
        ],
    ),
    F(
        "h3_road_slip_progress",
        "ROAD_BLOCKAGE",
        [
            L(
                "HIGH",
                "BLOCKED",
                [
                    "0700 small slip onto the hill road, cars squeezing past\n"
                    "0830 more material come down, road now fully blocked\n"
                    "0915 machine requested, nobody hurt"
                ],
                "Landslip worsening until the hill road is fully blocked; no injuries.",
                cause="landslip",
                resources=["hill road"],
                kind=KIND_MULTI_MESSAGE,
            ),
        ],
    ),
]

# --- OTHER -----------------------------------------------------------------

_OTHER = [
    F(
        "h3_other_fuel_theft",
        "OTHER",
        [
            L(
                "MEDIUM",
                "OPEN",
                [
                    "someone has been siphoning fuel from the parked vehicles overnight, two "
                    "tanks are down and the caps are off"
                ],
                "Fuel siphoned overnight from two parked vehicles.",
                cause="theft",
                entities=["2 vehicles affected"],
                resources=["vehicle fuel"],
            ),
            L(
                "LOW",
                "OPEN",
                ["fuel caps now locked and a night watch is on, nothing further overnight"],
                "Fuel security measures in place with no further loss.",
                kind=KIND_INFO,
            ),
        ],
    ),
    F(
        "h3_other_admin_log",
        "OTHER",
        [
            L(
                "LOW",
                "OPEN",
                [
                    "shift handover done, 3 volunteers signed off and 4 signed on, nothing "
                    "outstanding from the night"
                ],
                "Routine shift handover with nothing outstanding.",
                entities=["3 signed off", "4 signed on"],
                kind=KIND_INFO,
            ),
            L(
                "LOW",
                "UNKNOWN",
                ["got a garbled message about something at the gate, could not make it out"],
                "Unintelligible message referring to the gate.",
                kind=KIND_AMBIGUOUS,
            ),
        ],
    ),
]

HOLDOUT_FAMILIES: list[Family] = (
    _INFRA + _FLOOD + _FIRE + _MEDICAL + _EVAC + _POWER
    + _COMMS + _RESOURCE + _QUAKE + _WEATHER + _ROAD + _OTHER
)

__all__ = ["HOLDOUT_FAMILIES"]
