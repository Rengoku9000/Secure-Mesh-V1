"""Scenario families for the people-centred categories: MEDICAL, EVACUATION,
and OTHER.

MEDICAL carried the worst category/severity correlation in the Phase 1 set
(3 of 5 examples were CRITICAL), so its ladders deliberately bottom out at
LOW and MEDIUM as often as they top out at CRITICAL. OTHER had the opposite
problem (4 of 5 LOW) and is given genuine mid and high severity content here
— a serious situation that fits none of the eleven named categories is still
a serious situation.
"""

from __future__ import annotations

from .schema import (
    F,
    KIND_AMBIGUOUS,
    KIND_CONFLICTING,
    KIND_FALSE_POSITIVE,
    KIND_INFO,
    KIND_MULTI_MESSAGE,
    L,
)

MEDICAL_FAMILIES = [
    F(
        "medical_wound_ladder",
        "MEDICAL",
        [
            L(
                "LOW", "OPEN",
                ["someone at {loc} cut their hand on {object}, cleaned and dressed it, they're fine",
                 "minor cut from {object} at {loc}, dressed on site, no further help needed"],
                "A minor hand wound from {object} at {loc} was cleaned and dressed on site.",
                cause="{object}", entities=["1 minor injury"], resources=[], hint="{loc}",
            ),
            L(
                "HIGH", "OPEN",
                ["deep laceration at {loc} from {object}, bleeding slowed with pressure but it needs stitching",
                 "bad cut at {loc} from {object}, pressure holding it, needs proper treatment"],
                "A deep laceration from {object} at {loc} is controlled by pressure but requires further treatment.",
                cause="{object}", entities=["1 person with a deep laceration"],
                resources=["medical transport", "suturing"], hint="{loc}",
            ),
            L(
                "CRITICAL", "OPEN",
                ["arterial bleed at {loc} from {object}, tourniquet on, patient going pale, need transport now",
                 "serious bleed at {loc} from {object}, tourniquet applied, casualty deteriorating fast"],
                "An arterial bleed from {object} at {loc} is under a tourniquet with the casualty deteriorating and needing immediate transport.",
                cause="{object}", entities=["1 critical casualty"],
                resources=["ambulance", "blood products"], hint="{loc}",
            ),
        ],
        slots={"object": ["broken glass", "a sheet metal edge", "a chainsaw", "a falling tool"]},
    ),
    F(
        "medical_unwell_ladder",
        "MEDICAL",
        [
            L(
                "LOW", "OPEN",
                ["someone at {loc} feeling a bit off, sat down with water, already improving",
                 "one person light headed at {loc}, resting with fluids, coming round fine"],
                "A person at {loc} felt unwell, rested with fluids, and is improving.",
                cause="unwell, cause unclear", entities=["1 person unwell"], resources=[], hint="{loc}",
                note="MEDICAL does not mean urgent. This is the category's low end and must be learnable.",
            ),
            L(
                "MEDIUM", "OPEN",
                ["person at {loc} has had chest tightness for an hour, conscious and talking, but it isn't settling",
                 "someone at {loc} reporting chest discomfort that won't ease, alert but uncomfortable"],
                "A person at {loc} has persistent chest discomfort while remaining conscious and alert.",
                cause="chest pain, undiagnosed", entities=["1 person with chest pain"],
                resources=["medical assessment"], hint="{loc}",
            ),
            L(
                "CRITICAL", "OPEN",
                ["collapse at {loc}, no pulse, CPR started, need everything you have",
                 "cardiac arrest at {loc}, compressions in progress, send whatever's closest"],
                "A person at {loc} is in cardiac arrest with CPR in progress.",
                cause="cardiac arrest", entities=["1 person in cardiac arrest"],
                resources=["ambulance", "defibrillator"], hint="{loc}",
            ),
        ],
    ),
    F(
        "medical_heat_ladder",
        "MEDICAL",
        [
            L(
                "LOW", "OPEN",
                ["couple of people at {loc} overheating, moved into the shade with water, fine now",
                 "two at {loc} got too hot, shade and fluids sorted them out"],
                "A small number of people at {loc} overheated and recovered with shade and fluids.",
                cause="heat exposure",
                # "a couple of people", not "2 people": one phrasing says
                # "couple" and never gives a figure, and inventing the
                # precision the report withheld is the habit this dataset
                # exists to train against.
                entities=["a couple of people affected by heat"], resources=[], hint="{loc}",
            ),
            L(
                "MEDIUM", "OPEN",
                ["{count} people at {loc} down with heat now, we're running out of shade and cold water",
                 "heat is getting {count} of them at {loc}, shade and cool water running short"],
                "Heat has affected {count} people at {loc}, with shade and cold water running short.",
                cause="heat exposure", entities=["{count} people affected by heat"],
                resources=["drinking water", "shade", "cooling"], hint="{loc}",
            ),
            L(
                "HIGH", "OPEN",
                ["{count} down with heat at {loc} and one is confused and not sweating, that's the dangerous kind",
                 "heat casualties at {loc} up to {count}, one confused and dry skinned, needs cooling urgently"],
                "Heat has affected {count} people at {loc}, one showing confusion and absent sweating requiring urgent cooling.",
                cause="heat illness", entities=["{count} people affected", "1 with altered consciousness"],
                resources=["active cooling", "medical transport"], hint="{loc}",
            ),
        ],
    ),
    F(
        "medical_group_illness_ladder",
        "MEDICAL",
        [
            L(
                "MEDIUM", "OPEN",
                ["{smallcount} people at {loc} with upset stomachs since this morning, all mild so far",
                 "handful of stomach complaints at {loc}, {smallcount} so far, nothing severe"],
                "{smallcount} people at {loc} have mild stomach illness since this morning.",
                cause="suspected foodborne illness", entities=["{smallcount} people unwell"],
                resources=["oral rehydration"], hint="{loc}",
            ),
            L(
                "HIGH", "RESTRICTED",
                ["now {count} sick at {loc} with the same symptoms, all ate at the same place",
                 "{count} people ill at {loc}, same presentation, common meal between them"],
                "{count} people at {loc} share the same illness and a common meal, suggesting a point source.",
                cause="suspected point-source outbreak", entities=["{count} people ill"],
                resources=["oral rehydration", "food source investigation"], hint="{loc}",
            ),
            L(
                "CRITICAL", "BLOCKED",
                ["{bigcount} ill at {loc} now, several can't keep fluids down and two are unresponsive",
                 "outbreak at {loc} up to {bigcount}, some unable to hold fluids, two unresponsive"],
                "An outbreak at {loc} has reached {bigcount} people, with several unable to retain fluids and two unresponsive.",
                cause="outbreak of unknown cause",
                entities=["{bigcount} people ill", "2 unresponsive"],
                resources=["intravenous fluids", "medical team", "isolation space"], hint="{loc}",
            ),
        ],
    ),
    F(
        "medical_secondhand_ambiguous",
        "MEDICAL",
        [
            L(
                "MEDIUM", "UNKNOWN",
                ["someone ran past saying a person is hurt near {loc}, no idea who or how badly",
                 "passing report of an injury at {loc}, nothing on who, what happened or how bad"],
                "A second-hand report describes an injured person near {loc} with no detail on identity or severity.",
                cause="unknown", resources=["someone to attend and assess"], hint="{loc}",
                kind=KIND_AMBIGUOUS,
                note="No casualty count may be asserted: 'a person' is all that was established.",
            ),
            L(
                "LOW", "OPEN",
                ["found the reported casualty at {loc}, it's a grazed knee, they didn't even want it looked at",
                 "located the injury at {loc}, minor graze only, person declined treatment"],
                "The reported injury at {loc} was a minor graze and the person declined treatment.",
                cause="minor fall", entities=["1 minor injury"], resources=[], hint="{loc}",
                kind=KIND_AMBIGUOUS,
            ),
        ],
    ),
    F(
        "medical_casualty_count_conflicting",
        "MEDICAL",
        [
            L(
                "HIGH", "UNKNOWN",
                ["casualty numbers at {loc} don't match, one report says {smallcount} hurt and another says {count}",
                 "two different casualty counts from {loc}, {smallcount} in one and {count} in the other"],
                "Casualty counts from {loc} conflict between {smallcount} and {count} injured.",
                cause="incident with disputed casualty count",
                resources=["triage", "accurate headcount"], hint="{loc}",
                kind=KIND_CONFLICTING,
            ),
            L(
                "MEDIUM", "RESTRICTED",
                ["triage at {loc} done properly, {smallcount} injured, the higher number counted people who were just shaken",
                 "confirmed {loc} casualties at {smallcount}, the larger figure included uninjured bystanders"],
                "Triage at {loc} confirms {smallcount} injured; the higher figure had included uninjured bystanders.",
                cause="incident, casualties confirmed", entities=["{smallcount} people injured"],
                resources=["medical transport"], hint="{loc}",
                kind=KIND_CONFLICTING,
            ),
        ],
    ),
    F(
        "medical_false_alarm",
        "MEDICAL",
        [
            L(
                "LOW", "OPEN",
                ["reported collapse at {loc} was someone asleep in the shade, they're annoyed but unharmed",
                 "the {loc} collapse call was a false one, person was just sleeping, no issue"],
                "A reported collapse at {loc} was a person sleeping and required no medical response.",
                cause="false alarm", resources=[], hint="{loc}",
                kind=KIND_FALSE_POSITIVE,
            ),
            L(
                "CRITICAL", "OPEN",
                ["the person reported asleep at {loc} is actually unresponsive, starting CPR",
                 "checked the sleeping person at {loc}, they're not breathing, CPR underway"],
                "A person initially believed asleep at {loc} is unresponsive and not breathing, with CPR underway.",
                cause="cardiac arrest", entities=["1 person unresponsive"],
                resources=["ambulance", "defibrillator"], hint="{loc}",
                kind=KIND_FALSE_POSITIVE,
                note="The inverse error to the LOW case: assuming benign is as wrong as assuming worst.",
            ),
        ],
    ),
    F(
        "medical_multi_message_triage",
        "MEDICAL",
        [
            L(
                "CRITICAL", "OPEN",
                ["[1422] person down at {loc}\n[1424] breathing but unresponsive\n[1431] stopped breathing, CPR started",
                 "0812 casualty at {loc}\n0815 unresponsive, breathing\n0821 respiratory arrest, CPR in progress"],
                "Messages from {loc} track a casualty from unresponsive but breathing to respiratory arrest with CPR in progress.",
                cause="deterioration to respiratory arrest", entities=["1 critical casualty"],
                resources=["ambulance", "defibrillator"], hint="{loc}",
                kind=KIND_MULTI_MESSAGE,
            ),
            L(
                "MEDIUM", "OPEN",
                ["[1030] person unwell at {loc}\n[1045] sitting up, talking\n[1110] eating, wants to go back to work",
                 "1500 casualty {loc}\n1520 alert and talking\n1545 recovered, asking to resume"],
                "A person taken unwell at {loc} recovered over successive messages and is now alert and eating.",
                cause="transient illness, resolved", entities=["1 person recovered"],
                resources=["observation"], hint="{loc}",
                kind=KIND_MULTI_MESSAGE,
            ),
        ],
    ),
]

EVACUATION_FAMILIES = [
    F(
        "evac_scale_ladder",
        "EVACUATION",
        [
            L(
                "LOW", "OPEN",
                ["advising people at {loc} to be ready to move, nothing happening yet, just preparation",
                 "told {loc} residents to pack a bag in case, no movement ordered"],
                "Residents at {loc} have been advised to prepare for possible evacuation; no movement has begun.",
                cause="precautionary advisory", resources=[], hint="{loc}",
            ),
            L(
                "MEDIUM", "RESTRICTED",
                ["moving the {loc} low side out as a precaution, {count} people so far, no rush",
                 "precautionary move of {count} from the low part of {loc}, orderly, no pressure"],
                "A precautionary evacuation of the low-lying part of {loc} has moved {count} people so far.",
                cause="precautionary evacuation", entities=["{count} people moved"],
                resources=["shelter", "transport"], hint="{loc}",
            ),
            L(
                "CRITICAL", "BLOCKED",
                ["full immediate evacuation of {loc}, {bigcount} people, and {count} refusing to leave",
                 "everyone out of {loc} now, {bigcount} in total, {count} won't go"],
                "An immediate full evacuation of {loc} involves {bigcount} people, with {count} refusing to leave.",
                cause="immediate threat requiring full evacuation",
                entities=["{bigcount} people evacuating", "{count} refusing to leave"],
                resources=["transport", "shelter", "persuasion team"], hint="{loc}",
            ),
        ],
    ),
    F(
        "evac_shelter_capacity_ladder",
        "EVACUATION",
        [
            L(
                "LOW", "OPEN",
                ["shelter at {loc} open and ready, {count} in so far, plenty of room",
                 "{loc} shelter running with {count} people, lots of space left"],
                "The shelter at {loc} is open with {count} people accommodated and ample capacity remaining.",
                cause="shelter operating normally", entities=["{count} people sheltered"],
                resources=[], hint="{loc}",
            ),
            L(
                "MEDIUM", "OPEN",
                ["{loc} shelter is filling up, {bigcount} in and we're near what it holds",
                 "shelter at {loc} nearly full at {bigcount}, running out of floor"],
                "The shelter at {loc} holds {bigcount} people and is approaching capacity.",
                cause="shelter approaching capacity", entities=["{bigcount} people sheltered"],
                resources=["additional shelter space"], hint="{loc}",
            ),
            L(
                "HIGH", "RESTRICTED",
                ["{loc} shelter is over capacity, turning people away into the weather with nowhere to send them",
                 "no more room at the {loc} shelter, people being refused and nowhere else to offer"],
                "The shelter at {loc} is over capacity and turning people away with no alternative available.",
                cause="shelter over capacity",
                resources=["additional shelter space", "transport to alternatives"], hint="{loc}",
            ),
        ],
    ),
    F(
        "evac_transport_ladder",
        "EVACUATION",
        [
            L(
                "LOW", "OPEN",
                ["transport arranged for the {loc} move, buses due shortly, everything on track",
                 "vehicles sorted for {loc}, arriving soon, no problems"],
                "Transport has been arranged for the evacuation from {loc} and is due shortly.",
                cause="planned evacuation transport", resources=[], hint="{loc}",
            ),
            L(
                "HIGH", "RESTRICTED",
                ["buses for {loc} are {hours} hours late, {bigcount} people waiting outside in this weather",
                 "transport delayed {hours} hours at {loc}, {bigcount} standing in the open"],
                "Evacuation transport for {loc} is {hours} hours late, leaving {bigcount} people waiting outdoors.",
                cause="transport delay during evacuation",
                entities=["{bigcount} people waiting outdoors"],
                resources=["transport", "temporary shelter"], hint="{loc}",
            ),
            L(
                "CRITICAL", "BLOCKED",
                ["no transport is coming to {loc} and {count} of those waiting can't walk out",
                 "transport cancelled for {loc}, {count} people here physically cannot leave on foot"],
                "No transport is available at {loc} and {count} people waiting are unable to leave on foot.",
                cause="evacuation transport failure",
                entities=["{count} people unable to walk out"],
                resources=["vehicles", "stretcher capacity"], hint="{loc}",
            ),
        ],
    ),
    F(
        "evac_vulnerable_ladder",
        "EVACUATION",
        [
            L(
                "MEDIUM", "OPEN",
                ["moving the {loc} care residents early so they're not rushed later, {count} of them, all going smoothly",
                 "early relocation of {count} care residents from {loc}, unhurried, going fine"],
                "{count} care residents are being relocated early from {loc} without difficulty.",
                cause="precautionary relocation", entities=["{count} care residents"],
                resources=["accessible transport"], hint="{loc}",
            ),
            L(
                "HIGH", "RESTRICTED",
                ["the {loc} relocation is stuck, {count} residents need stretchers and we have {smallcount} available",
                 "{count} stretcher cases at {loc} and only {smallcount} stretchers, move has stalled"],
                "Relocation from {loc} has stalled with {count} residents needing stretchers and only {smallcount} available.",
                cause="insufficient stretcher capacity",
                entities=["{count} stretcher cases", "{smallcount} stretchers available"],
                resources=["stretchers", "accessible transport"], hint="{loc}",
            ),
            L(
                "CRITICAL", "BLOCKED",
                ["{count} immobile residents still inside at {loc} and the access route has just gone",
                 "{count} non-ambulatory people remain at {loc}, the way in is now cut"],
                "{count} immobile residents remain inside at {loc} with the access route now cut.",
                cause="access lost during evacuation of immobile residents",
                entities=["{count} immobile people still inside"],
                resources=["alternative access", "rescue team"], hint="{loc}",
            ),
        ],
    ),
    F(
        "evac_rumour_ambiguous",
        "EVACUATION",
        [
            L(
                "MEDIUM", "UNKNOWN",
                ["people at {loc} are saying an evacuation's been ordered, we can't find anyone who issued it",
                 "talk of an evacuation order at {loc}, no source, nobody can confirm it"],
                "Residents at {loc} believe an evacuation has been ordered, but no issuing source can be identified.",
                cause="unverified evacuation report", resources=["verification"], hint="{loc}",
                kind=KIND_AMBIGUOUS,
                note="An unverified order is neither confirmed nor dismissed; the uncertainty is the finding.",
            ),
            L(
                "LOW", "OPEN",
                ["no evacuation was ordered for {loc}, it was a drill notice being passed around wrong",
                 "the {loc} evacuation talk came from a drill notice, no real order exists"],
                "No evacuation was ordered for {loc}; the reports originated from a misread drill notice.",
                cause="misunderstood drill notice", resources=[], hint="{loc}",
                kind=KIND_AMBIGUOUS,
            ),
        ],
    ),
    F(
        "evac_headcount_conflicting",
        "EVACUATION",
        [
            L(
                "HIGH", "UNKNOWN",
                ["the {loc} lists don't agree, shelter says {bigcount} arrived and the origin list says {count} left",
                 "numbers from {loc} conflict, {count} departed but only {bigcount} logged at the shelter"],
                "Evacuation records for {loc} conflict between {count} departures and {bigcount} arrivals at the shelter.",
                cause="evacuation with unreconciled headcount",
                resources=["reconciliation of lists", "search of the route"], hint="{loc}",
                kind=KIND_CONFLICTING,
                note="A gap between departures and arrivals may mean missing people or bad bookkeeping. Both stay open.",
            ),
            L(
                "MEDIUM", "RESTRICTED",
                ["{loc} lists reconciled, everyone's accounted for, a group went to the second shelter instead",
                 "sorted the {loc} numbers, nobody missing, one group was logged at the other site"],
                "Evacuation records for {loc} have been reconciled with everyone accounted for; a group went to a second shelter.",
                cause="evacuation, headcount reconciled",
                resources=["updated shelter records"], hint="{loc}",
                kind=KIND_CONFLICTING,
            ),
        ],
    ),
    F(
        "evac_return_info",
        "EVACUATION",
        [
            L(
                "LOW", "OPEN",
                ["evacuation at {loc} is over, people going home, shelter closing",
                 "{loc} residents returning, shelter shutting down, all done"],
                "The evacuation at {loc} has ended with residents returning home and the shelter closing.",
                cause="evacuation ended", resources=[], hint="{loc}",
                kind=KIND_INFO,
            ),
            L(
                "MEDIUM", "RESTRICTED",
                ["most of {loc} can return but {count} households can't, their houses aren't safe yet",
                 "{loc} reopening except for {count} homes still assessed unsafe"],
                "Most residents of {loc} may return, but {count} households remain displaced by unsafe housing.",
                cause="partial return after evacuation", entities=["{count} households still displaced"],
                resources=["continued shelter"], hint="{loc}",
                kind=KIND_INFO,
            ),
        ],
    ),
    F(
        "evac_multi_message_order",
        "EVACUATION",
        [
            L(
                "CRITICAL", "BLOCKED",
                ["[1640] preparing to move {loc}\n[1705] evacuation ordered\n[1740] route cut, {count} still inside",
                 "1210 standby at {loc}\n1235 move ordered\n1310 exit blocked, {count} remain"],
                "Messages from {loc} escalate from standby to an ordered evacuation and then a cut route with {count} people still inside.",
                cause="evacuation with lost egress", entities=["{count} people still inside"],
                resources=["alternative route", "rescue team"], hint="{loc}",
                kind=KIND_MULTI_MESSAGE,
            ),
            L(
                "LOW", "OPEN",
                ["[0930] possible evacuation {loc}\n[1015] threat assessed as low\n[1050] stand down, nobody moved",
                 "1330 evacuation considered {loc}\n1410 risk reassessed low\n1440 stood down, no movement"],
                "A possible evacuation at {loc} was stood down after reassessment, with no one moved.",
                cause="evacuation considered and stood down", resources=[], hint="{loc}",
                kind=KIND_MULTI_MESSAGE,
            ),
        ],
    ),
]

OTHER_FAMILIES = [
    F(
        "other_disturbance_ladder",
        "OTHER",
        [
            L(
                "LOW", "OPEN",
                ["argument in the queue at {loc}, words only, sorted itself out",
                 "bit of a row at {loc} over the queue, calmed down on its own"],
                "A verbal argument in a queue at {loc} resolved without intervention.",
                cause="queue dispute", resources=[], hint="{loc}",
            ),
            L(
                "MEDIUM", "RESTRICTED",
                ["crowd at {loc} getting agitated about the wait, pushing at the front, we need more hands",
                 "tension building at {loc} over delays, pushing at the barrier, short staffed here"],
                "A crowd at {loc} is becoming agitated over delays with pushing at the front.",
                cause="crowd tension over delays", resources=["additional staff", "crowd management"], hint="{loc}",
            ),
            L(
                "HIGH", "BLOCKED",
                ["serious disturbance at {loc}, the barrier's gone over and {count} people have been knocked down",
                 "crowd surge at {loc} broke the barrier, {count} people on the ground"],
                "A crowd surge at {loc} has collapsed the barrier with {count} people knocked to the ground.",
                cause="crowd surge", entities=["{count} people knocked down"],
                resources=["crowd management", "first aid"], hint="{loc}",
                note="OTHER is not a synonym for trivial — this is the category's high end.",
            ),
        ],
    ),
    F(
        "other_equipment_ladder",
        "OTHER",
        [
            L(
                "LOW", "OPEN",
                ["the {device} at {loc} has packed up, annoying but we can work round it",
                 "{device} broken at {loc}, not urgent, workaround in place"],
                "The {device} at {loc} has failed, with a workaround in place.",
                cause="equipment failure", asset="{device}", resources=["repair or replacement"], hint="{loc}",
            ),
            L(
                "MEDIUM", "OPEN",
                ["{device} at {loc} is down and it's the only one we have, holding up the whole line",
                 "our only {device} at {loc} has failed, work has stopped behind it"],
                "The only {device} at {loc} has failed, halting dependent work.",
                cause="failure of sole equipment", asset="{device}",
                resources=["replacement {device}"], hint="{loc}",
            ),
            L(
                "CRITICAL", "OPEN",
                ["the {device} at {loc} failed and it's what keeps the cold chain running, stock will spoil within {hours} hours",
                 "{device} down at {loc}, cold chain at risk, {hours} hours before the stock is lost"],
                "Failure of the {device} at {loc} has put the cold chain at risk, with stock loss expected within {hours} hours.",
                cause="failure of cold chain equipment", asset="{device}",
                resources=["replacement {device}", "alternative cold storage"], hint="{loc}",
            ),
        ],
        slots={"device": ["printer", "laptop", "water pump", "chiller unit", "generator controller"]},
    ),
    F(
        "other_security_ladder",
        "OTHER",
        [
            L(
                "LOW", "OPEN",
                ["someone was wandering round {loc} who shouldn't be, spoke to them and they left",
                 "unauthorised person at {loc}, had a word, they've gone"],
                "An unauthorised person at {loc} left after being spoken to.",
                cause="unauthorised access", resources=[], hint="{loc}",
            ),
            L(
                "MEDIUM", "RESTRICTED",
                ["stock going missing from the {loc} store overnight, third time this week",
                 "more items gone from {loc} store, this is the third night running"],
                "Repeated overnight losses of stock have occurred from the store at {loc}.",
                cause="repeated theft", resources=["secure storage", "night watch"], hint="{loc}",
            ),
            L(
                "HIGH", "RESTRICTED",
                ["staff at {loc} have been threatened and won't work the night shift, we're short {count} people",
                 "threats made to {loc} staff, {count} refusing nights now, coverage is gone"],
                "Staff at {loc} have been threatened, leaving the night shift {count} people short.",
                cause="threats to staff", entities=["{count} staff unwilling to work nights"],
                resources=["security presence", "replacement staff"], hint="{loc}",
            ),
        ],
    ),
    F(
        "other_logistics_ladder",
        "OTHER",
        [
            L(
                "LOW", "OPEN",
                ["the delivery to {loc} is running a couple of hours behind, nothing urgent on it",
                 "supply run to {loc} delayed a bit, nothing critical aboard"],
                "A delivery to {loc} is running a few hours late with nothing urgent aboard.",
                cause="transport delay", resources=[], hint="{loc}",
                kind=KIND_INFO,
            ),
            L(
                "MEDIUM", "OPEN",
                ["delivery to {loc} is {hours} hours late and the {stock} on it was due to be handed out today",
                 "{hours} hour delay on the {loc} delivery, the {stock} was for today's distribution"],
                "A {hours}-hour delay to the {loc} delivery affects {stock} scheduled for distribution today.",
                cause="transport delay affecting distribution",
                resources=["revised distribution plan"], hint="{loc}",
            ),
            L(
                "CRITICAL", "BLOCKED",
                ["the {loc} delivery vehicle hasn't arrived and we've had no contact with the driver for {hours} hours",
                 "no sign of the {loc} delivery and the driver's been out of contact {hours} hours"],
                "A delivery vehicle for {loc} has failed to arrive with no driver contact for {hours} hours.",
                cause="vehicle and driver unaccounted for",
                entities=["1 driver out of contact"],
                resources=["search of the route", "communications attempt"], hint="{loc}",
            ),
        ],
        slots={"stock": ["food rations", "medical supplies", "blankets", "fuel"]},
    ),
    F(
        "other_garbled_ambiguous",
        "OTHER",
        [
            L(
                "MEDIUM", "UNKNOWN",
                ["got a broken transmission mentioning {loc} and something urgent, couldn't make out the rest",
                 "garbled call came through about {loc}, sounded urgent, nothing else intelligible"],
                "A broken transmission referred to {loc} and something urgent, with no further detail intelligible.",
                cause="unintelligible transmission", resources=["callback attempt"], hint="{loc}",
                kind=KIND_AMBIGUOUS,
                note="The only established facts are that a transmission happened and named a place.",
            ),
            L(
                "LOW", "OPEN",
                ["got {team} back on the radio, the garbled call about {loc} was a routine check in",
                 "reconnected with {team}, the broken {loc} message was just a routine check"],
                "Contact was restored and the earlier garbled message about {loc} was a routine check-in.",
                cause="routine check-in, initially unintelligible", resources=[], hint="{loc}",
                kind=KIND_AMBIGUOUS,
            ),
        ],
    ),
    F(
        "other_instruction_conflicting",
        "OTHER",
        [
            L(
                "HIGH", "UNKNOWN",
                ["we've been told to hold at {loc} and also to move out immediately, both from people who can give that instruction",
                 "two instructions for {loc}, hold position and evacuate now, both from authorised people"],
                "Two contradictory instructions for {loc} have been issued by people with authority to give them.",
                cause="contradictory instructions", resources=["single point of decision"], hint="{loc}",
                kind=KIND_CONFLICTING,
            ),
            L(
                "LOW", "OPEN",
                ["the {loc} instructions are sorted, the hold applied to vehicles and the move applied to people",
                 "cleared up the {loc} confusion, hold was for transport, move was for personnel"],
                "The contradictory instructions for {loc} were reconciled: the hold applied to vehicles, the move to personnel.",
                cause="instructions clarified", resources=[], hint="{loc}",
                kind=KIND_CONFLICTING,
            ),
        ],
    ),
    F(
        "other_routine_info",
        "OTHER",
        [
            L(
                "LOW", "OPEN",
                ["shift handover at {loc} done, {count} on the next rota, nothing outstanding",
                 "handover complete at {loc}, {count} people on tonight, nothing to pass on"],
                "A shift handover at {loc} is complete with {count} people rostered and nothing outstanding.",
                cause="routine handover", entities=["{count} people on shift"], resources=[], hint="{loc}",
                kind=KIND_INFO,
                note="Genuinely routine traffic. A model that treats every message as an incident is wrong here.",
            ),
            L(
                "LOW", "OPEN",
                ["volunteer list for {loc} is full for tomorrow, might open a second shift if more want in",
                 "{loc} volunteer sheet full for tomorrow, could add a second shift"],
                "The volunteer roster for {loc} is full for tomorrow with a possible second shift.",
                cause="routine staffing", resources=["volunteers"], hint="{loc}",
                kind=KIND_INFO,
            ),
        ],
    ),
    F(
        "other_multi_message_admin",
        "OTHER",
        [
            L(
                "HIGH", "RESTRICTED",
                ["[1100] small queue issue at {loc}\n[1140] crowd growing, people unhappy\n[1220] barrier pushed over, {count} on the floor",
                 "0900 queue complaint {loc}\n0945 crowd building\n1030 barrier down, {count} knocked over"],
                "Messages from {loc} track a queue complaint escalating into a crowd surge that knocked over {count} people.",
                cause="crowd surge following queue disorder", entities=["{count} people knocked down"],
                resources=["crowd management", "first aid"], hint="{loc}",
                kind=KIND_MULTI_MESSAGE,
            ),
            L(
                "LOW", "OPEN",
                ["[1300] complaint about the {loc} rota\n[1330] spoke to both parties\n[1400] resolved, no further action",
                 "1600 rota dispute {loc}\n1625 discussed with those involved\n1650 settled, nothing further"],
                "A rota complaint at {loc} was discussed with those involved and resolved with no further action.",
                cause="staffing dispute, resolved", resources=[], hint="{loc}",
                kind=KIND_MULTI_MESSAGE,
            ),
        ],
    ),
]

PEOPLE_FAMILIES = MEDICAL_FAMILIES + EVACUATION_FAMILIES + OTHER_FAMILIES
