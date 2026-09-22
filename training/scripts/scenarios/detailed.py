"""Long, information-dense scenario families — one per category.

Three gaps are closed here that the other modules leave open.

**Length.** Every other family produces a report between roughly 50 and 150
characters, which is the same narrow band the Phase 1 corpus sat in (77-130).
Real traffic includes the write-up: a coordinator who has walked the site and
sends four sentences. A model trained only on terse messages has never seen
one.

**Multiple entities.** Short reports carry one fact. These carry several —
counts, assets, and resource needs in one message — which is where extraction
actually gets hard.

**Mixed-quality reports.** Each family pairs a long report that *establishes*
things with a long report that says a great deal and establishes very little.
Length is not evidence, and a model that treats a wall of text as
authoritative has learned the wrong lesson. The second level of every family
here is deliberately verbose and thin, and its correct answer is conservative.
"""

from __future__ import annotations

from .schema import F, KIND_AMBIGUOUS, KIND_FALSE_POSITIVE, L

DETAILED_FAMILIES = [
    F(
        "detailed_fire_site_writeup",
        "FIRE",
        [
            L(
                "HIGH", "RESTRICTED",
                ["Walked the {loc} site after the call came in. Fire is in the store building, "
                 "confined to the rear section so far but the roof space is open the whole length. "
                 "{count} staff got out, {smallcount} of them with smoke exposure and being watched. "
                 "We have two extinguishers left and no hose that reaches. Need a proper appliance.",
                 "Report from {loc}: fire in the rear of the store building, not yet into the front "
                 "but the roof void runs through. {count} people evacuated, {smallcount} treated for "
                 "smoke. Our firefighting kit is two extinguishers and they will not hold it. "
                 "Requesting an appliance and a crew."],
                "A fire in the rear of the store building at {loc} threatens to spread through an open roof void; {count} staff evacuated with {smallcount} treated for smoke exposure, and on-site firefighting capability is inadequate.",
                cause="structure fire, rear section", asset="store building",
                entities=["{count} staff evacuated", "{smallcount} with smoke exposure"],
                resources=["fire appliance", "firefighting crew", "medical assessment"], hint="{loc}",
            ),
            L(
                "MEDIUM", "UNKNOWN",
                ["Long message from {loc} and I am not much clearer. Someone smelled burning earlier, "
                 "someone else thought they saw smoke near the old sheds, a third person says it was "
                 "probably the kitchen extractor which has done this before. Nobody has actually gone "
                 "and looked. Sending someone now.",
                 "Passing on what has come in from {loc}, which is a lot of talk and no facts. Burning "
                 "smell reported, possible smoke near the sheds, and a suggestion it is the extractor "
                 "again. No one has been to check. I will confirm when someone has eyes on it."],
                "Reports at {loc} describe a burning smell and possible smoke near the old sheds, with a suggestion of a known extractor fault; nothing has been directly verified.",
                cause="unverified", resources=["direct inspection"], hint="{loc}",
                kind=KIND_AMBIGUOUS,
                note="Long and detailed, but nothing in it is established. Length is not evidence.",
            ),
        ],
    ),
    F(
        "detailed_flood_assessment",
        "FLOODING",
        [
            L(
                "CRITICAL", "BLOCKED",
                ["Assessment from {loc}. Water came up faster than anyone expected overnight. "
                 "The lower row of houses is flooded to first floor level, {count} households are "
                 "on upper floors and cannot get out, and the access road is under water at both ends. "
                 "We have one small boat. Need swift water capability and somewhere to put {bigcount} people.",
                 "{loc} situation: overnight rise has put the lower houses under to the first floor. "
                 "{count} households stranded upstairs, road submerged both ways, one boat on site. "
                 "Requesting swift water team and shelter capacity for {bigcount}."],
                "Rapid overnight flooding at {loc} has submerged lower housing to first floor level, stranding {count} households upstairs with both road accesses under water and only one boat available.",
                cause="rapid overnight flooding",
                entities=["{count} households stranded", "{bigcount} people needing shelter"],
                resources=["swift water rescue team", "boats", "shelter capacity"], hint="{loc}",
            ),
            L(
                "MEDIUM", "UNKNOWN",
                ["Several accounts from {loc} and they do not line up. One says the water is into the "
                 "houses, one says it is at the gate and holding, one says it went down an hour ago. "
                 "All three are from people who live there. The rain has stopped here but I do not know "
                 "about up the valley. Sending someone to look properly.",
                 "Mixed reporting from {loc}. Residents variously describe water inside houses, water "
                 "at the gate, and water receding. Rain has stopped locally, upstream unknown. "
                 "No reliable picture until someone assesses it directly."],
                "Accounts from {loc} conflict between water inside houses, water holding at the gate, and water receding, with upstream conditions unknown.",
                cause="flooding, extent disputed", resources=["direct assessment"], hint="{loc}",
                kind=KIND_AMBIGUOUS,
            ),
        ],
    ),
    F(
        "detailed_medical_scene",
        "MEDICAL",
        [
            L(
                "CRITICAL", "RESTRICTED",
                ["Scene report from {loc}. Wall came down onto the work party. {count} casualties total, "
                 "{smallcount} of them serious including one with a crush injury to the leg and one not "
                 "responding. Remaining {bigcount} workers are accounted for and unhurt. We have one "
                 "first aid kit between us and no stretcher. Need medical team and transport for {smallcount}.",
                 "{loc}: collapse onto a work party. {count} injured, {smallcount} serious, one crush "
                 "injury and one unresponsive. {bigcount} others accounted for. One first aid kit, no "
                 "stretchers. Requesting medics and evacuation for the serious cases."],
                "A wall collapse onto a work party at {loc} has injured {count} people, {smallcount} seriously including a crush injury and an unresponsive casualty, with {bigcount} others unhurt and minimal medical equipment on site.",
                cause="structural collapse onto personnel",
                entities=["{count} casualties", "{smallcount} serious", "1 unresponsive", "{bigcount} uninjured"],
                resources=["medical team", "stretchers", "casualty transport"], hint="{loc}",
            ),
            L(
                "MEDIUM", "UNKNOWN",
                ["Message from {loc} that I am struggling to make sense of. Mentions people being unwell, "
                 "possibly several, possibly since yesterday, possibly connected to the water or possibly "
                 "not. No numbers, no symptoms described, no idea who assessed them. Asking for clarification.",
                 "From {loc}: reports of illness, unclear how many, unclear since when, and a suggestion "
                 "it may be water related with nothing to support that. No symptoms given and no "
                 "assessment done. Following up for detail."],
                "A report from {loc} describes illness among an unspecified number of people, with no symptoms, timing, or assessment established, and an unsupported suggestion of a water-related cause.",
                cause="undetermined", resources=["clarification", "clinical assessment"], hint="{loc}",
                kind=KIND_AMBIGUOUS,
                note="No count may be extracted: 'possibly several' is not a number.",
            ),
        ],
    ),
    F(
        "detailed_infrastructure_survey",
        "INFRASTRUCTURE",
        [
            L(
                "HIGH", "BLOCKED",
                ["Survey of {loc} complete. The main store has movement in two walls with cracking that "
                 "runs floor to ceiling, and the roof beam bearing has shifted. We have taken {bigcount} "
                 "people out of it and moved {count} pallets of stock to the yard. The building should "
                 "not be entered again until an engineer has seen it. Everything else on site is sound.",
                 "{loc} survey: main store has two walls moving, full height cracking, and a displaced "
                 "roof beam bearing. {bigcount} people relocated out, {count} pallets moved to the yard. "
                 "No re-entry until engineered. Remaining structures check out fine."],
                "A survey at {loc} found two moving walls, full-height cracking and a displaced roof beam bearing in the main store, which has been emptied of {bigcount} people and {count} pallets and closed pending engineering assessment.",
                cause="progressive structural movement", asset="main store",
                entities=["{bigcount} people relocated", "{count} pallets of stock moved"],
                resources=["structural engineers", "alternative storage"], hint="{loc}",
            ),
            L(
                "LOW", "OPEN",
                ["Full walk round of {loc} done as asked. Checked all {count} structures. Found the "
                 "usual weathering, a couple of loose roof sheets which we refixed, some cracked render "
                 "on the north side that has been there since before I arrived, and a door that does "
                 "not close properly. Nothing structural. No action needed beyond the door.",
                 "Completed the {loc} inspection across {count} buildings. Weathering, two loose roof "
                 "sheets now refixed, long standing render cracks on the north elevation, one sticking "
                 "door. Nothing that affects structure. Only the door needs attention."],
                "A full inspection of {count} structures at {loc} found only weathering, refixed roof sheets, long-standing render cracks and a sticking door, with nothing structural.",
                cause="routine inspection findings", entities=["{count} structures inspected"],
                resources=["door repair"], hint="{loc}",
                note="A long, thorough report whose correct answer is LOW. Detail is not severity.",
            ),
        ],
    ),
    F(
        "detailed_evacuation_status",
        "EVACUATION",
        [
            L(
                "HIGH", "RESTRICTED",
                ["Evacuation status at {loc}. {bigcount} people moved so far out of an estimated total "
                 "of double that. {count} are still waiting at the assembly point with no transport, "
                 "{smallcount} of those cannot walk. Shelter at the far end is taking people but is "
                 "filling. Weather is closing in. Need more vehicles and somewhere else to send people.",
                 "{loc} evacuation: {bigcount} moved, roughly the same again to go. {count} at the "
                 "assembly point without transport, {smallcount} non-ambulatory. Receiving shelter "
                 "nearly full, weather deteriorating. Requesting vehicles and additional shelter."],
                "An evacuation at {loc} has moved {bigcount} people with roughly as many remaining; {count} wait at the assembly point without transport including {smallcount} unable to walk, while the receiving shelter approaches capacity in deteriorating weather.",
                cause="evacuation constrained by transport and shelter capacity",
                entities=["{bigcount} people moved", "{count} awaiting transport", "{smallcount} non-ambulatory"],
                resources=["vehicles", "additional shelter", "accessible transport"], hint="{loc}",
            ),
            L(
                "LOW", "OPEN",
                ["Full write up of the {loc} move for the record. {bigcount} people relocated over "
                 "{hours} hours, all by vehicle, no injuries and nothing left behind. Shelter received "
                 "everyone and has capacity to spare. {count} volunteers helped. Everything went the "
                 "way it was planned, which is worth noting because it usually does not.",
                 "Record of the {loc} relocation: {bigcount} people moved across {hours} hours, all "
                 "transported, no injuries, nothing left. Receiving shelter has spare capacity. "
                 "{count} volunteers assisted. Went entirely to plan."],
                "A planned relocation at {loc} moved {bigcount} people over {hours} hours without injury, assisted by {count} volunteers, with the receiving shelter retaining spare capacity.",
                cause="completed planned evacuation",
                entities=["{bigcount} people relocated", "{count} volunteers"],
                resources=[], hint="{loc}",
            ),
        ],
    ),
    F(
        "detailed_power_status",
        "POWER",
        [
            L(
                "CRITICAL", "OPEN",
                ["Power situation at {loc} is bad. Mains went {hours} hours ago with no estimate for "
                 "return. Main generator is running but the backup will not start. Fuel between them is "
                 "under a day. On that supply we have the clinic with {smallcount} patients on powered "
                 "equipment, the cold store holding the vaccine stock, and the water pumps for "
                 "{bigcount} people. Losing the generator loses all three.",
                 "{loc} power: mains down {hours} hours, no restoration estimate, backup generator "
                 "will not start, under a day of fuel for the one that runs. It carries the clinic "
                 "({smallcount} patients on equipment), the vaccine cold store, and the pumps serving "
                 "{bigcount} people. Single point of failure."],
                "At {loc} mains power has been down {hours} hours with no restoration estimate, the backup generator will not start, and under a day of fuel remains for the single running generator carrying the clinic with {smallcount} patients on powered equipment, the vaccine cold store, and water pumps for {bigcount} people.",
                cause="mains failure with failed backup and limited fuel", asset="generator",
                entities=["{smallcount} patients on powered equipment", "{bigcount} people dependent on pumped water"],
                resources=["generator fuel", "backup generator repair", "power restoration"], hint="{loc}",
            ),
            L(
                "LOW", "OPEN",
                ["Monthly power write up for {loc}. Generator ran {hours} hours on test, all "
                 "parameters normal. Fuel stock full. Mains has been stable with two brief dips, both "
                 "under a second, nothing tripped. Replaced a contactor on the distribution board as "
                 "preventive work. Nothing outstanding.",
                 "{loc} power report: generator tested {hours} hours, normal throughout. Fuel full. "
                 "Mains stable aside from two sub-second dips with no trips. Preventive contactor "
                 "replacement done on the board. No outstanding items."],
                "A routine power report for {loc} records a normal {hours}-hour generator test, full fuel stock, stable mains with two sub-second dips, and a preventive contactor replacement.",
                cause="routine testing and maintenance", asset="generator",
                resources=[], hint="{loc}",
            ),
        ],
    ),
    F(
        "detailed_comms_status",
        "COMMUNICATIONS",
        [
            L(
                "HIGH", "UNKNOWN",
                ["Communications picture at {loc} is degraded. Repeater is down and will not come back "
                 "without a part we do not have. Mobile coverage exists on the high ground only. "
                 "{count} field teams are working beyond handheld range and we are relaying through "
                 "whoever happens to be on the ridge. One team has not checked in this cycle. "
                 "We need the repeater part or a satellite handset.",
                 "{loc} comms: repeater down pending a part we lack, mobile only on high ground, "
                 "{count} teams operating beyond handheld range with ad hoc relay through the ridge. "
                 "One team missed this cycle's check in. Requesting the repeater part or a satellite unit."],
                "Communications at {loc} are degraded with the repeater down pending an unavailable part, mobile coverage limited to high ground, and {count} field teams beyond handheld range relayed ad hoc, one of which has missed a check-in.",
                cause="repeater failure with limited alternative coverage", asset="repeater",
                entities=["{count} teams beyond radio range", "1 team missed check-in"],
                resources=["repeater part", "satellite handset", "relay arrangement"], hint="{loc}",
            ),
            L(
                "MEDIUM", "UNKNOWN",
                ["Trying to give a picture of {loc} comms and I cannot. Some people say the network is "
                 "fine, others cannot get a call out, one says it works only outdoors and another says "
                 "only indoors. The repeater light is green. I have no way to test properly from here "
                 "and no pattern I can describe.",
                 "{loc} comms status is unclear. Conflicting accounts of network availability, "
                 "contradictory reports about indoors and outdoors, repeater indicating healthy. "
                 "No means to test from here and no discernible pattern."],
                "Reports on communications at {loc} conflict on network availability and indoor or outdoor coverage, with the repeater indicating healthy and no means of testing available.",
                cause="undetermined", resources=["on-site testing"], hint="{loc}",
                kind=KIND_AMBIGUOUS,
            ),
        ],
    ),
    F(
        "detailed_resource_position",
        "RESOURCE_SHORTAGE",
        [
            L(
                "CRITICAL", "OPEN",
                ["Stock position at {loc}. Water is the problem: {hours} hours at current draw for "
                 "{bigcount} people, and the draw goes up as it gets hotter. Food is better, several "
                 "days. Medical is adequate except we are out of oxygen with {smallcount} patients who "
                 "may need it. Fuel covers the pumps for two days. Water and oxygen are the ones that "
                 "will hurt first.",
                 "{loc} stock: water {hours} hours for {bigcount} people and rising consumption, food "
                 "several days, medical adequate but oxygen exhausted with {smallcount} patients who "
                 "may require it, fuel two days for pumping. Water and oxygen are the critical lines."],
                "Stock at {loc} leaves {hours} hours of water for {bigcount} people with rising consumption and no oxygen remaining for {smallcount} patients who may need it, while food, other medical stock and pump fuel are adequate.",
                cause="water and oxygen stock exhaustion",
                entities=["{bigcount} people dependent on water", "{smallcount} patients who may need oxygen"],
                resources=["drinking water", "oxygen cylinders", "water tankering"], hint="{loc}",
            ),
            L(
                "MEDIUM", "UNKNOWN",
                ["Asked for a stock position at {loc} and what I have is not usable. The store says "
                 "plenty, the distribution team says they ran out yesterday, and the paper log has not "
                 "been written up since the weekend. Different people are counting different things in "
                 "different units. I cannot give you a figure I would stand behind.",
                 "{loc} stock reporting is inconsistent. Store reports ample, distribution reports "
                 "exhaustion yesterday, log not updated since the weekend, and counts use different "
                 "units. No defensible figure available."],
                "Stock reporting at {loc} is inconsistent between the store, the distribution team and an out-of-date log, with counts in differing units and no reliable figure available.",
                cause="stock position unestablished",
                resources=["physical stock count"], hint="{loc}",
                kind=KIND_AMBIGUOUS,
                note="A long report that establishes only that nobody knows. No quantity may be extracted.",
            ),
        ],
    ),
    F(
        "detailed_road_assessment",
        "ROAD_BLOCKAGE",
        [
            L(
                "HIGH", "BLOCKED",
                ["Drove the {loc} route as far as possible. Blocked at three points: a slip across the "
                 "carriageway about {count} metres wide, a fallen tree which we could clear ourselves, "
                 "and a section where the edge has gone and half the width with it. The slip is the "
                 "problem and it needs plant. There is no alternative route that avoids all three.",
                 "{loc} route assessment: three blockages, a slip roughly {count} metres across, a "
                 "fallen tree we can handle, and an edge failure taking half the width. The slip "
                 "requires heavy plant. No alternative route clears all three points."],
                "The route at {loc} is blocked at three points including a {count}-metre slip requiring heavy plant, a clearable fallen tree, and an edge failure removing half the carriageway, with no alternative route avoiding all three.",
                cause="multiple blockages including a landslip",
                entities=["3 separate blockages"],
                resources=["heavy plant", "clearance crew"], hint="{loc}",
            ),
            L(
                "LOW", "OPEN",
                ["Route report for {loc} after the weather. Drove the full length both ways. Surface "
                 "water in two dips which drained while I watched, some gravel washed onto the "
                 "carriageway on the bend, and a branch down on the verge which I moved. Road is "
                 "entirely passable to anything. Worth a sweep for the gravel at some point.",
                 "{loc} route checked end to end both directions after the weather. Two dips holding "
                 "water that cleared, gravel wash on the bend, one branch on the verge now moved. "
                 "Fully passable to all vehicles. Gravel sweep worth scheduling."],
                "A full route check at {loc} after the weather found draining surface water, gravel wash on a bend and a cleared branch, with the road fully passable to all vehicles.",
                cause="minor weather debris", resources=["road sweeping"], hint="{loc}",
                note="A thorough report of a non-problem. The correct answer is LOW despite the length.",
            ),
        ],
    ),
    F(
        "detailed_weather_observation",
        "SEVERE_WEATHER",
        [
            L(
                "HIGH", "RESTRICTED",
                ["Conditions at {loc} over the last {hours} hours. Wind has built steadily and is now "
                 "taking roof sheeting off the lighter structures. {count} shelters have lost covering "
                 "and {bigcount} people have been moved into the hall. Rain is horizontal and visibility "
                 "is down to a few metres. Anyone outside is working blind. Recommend no further "
                 "outdoor movement until it eases.",
                 "{loc} conditions over {hours} hours: wind building to the point of stripping roof "
                 "sheeting, {count} shelters damaged, {bigcount} people relocated to the hall, "
                 "horizontal rain and visibility of a few metres. Outdoor work is unsafe."],
                "Over {hours} hours conditions at {loc} have deteriorated to wind stripping roof sheeting from {count} shelters, {bigcount} people relocated indoors, and visibility of a few metres making outdoor movement unsafe.",
                cause="sustained severe wind and rain",
                entities=["{count} shelters damaged", "{bigcount} people relocated"],
                resources=["shelter repair", "indoor accommodation"], hint="{loc}",
            ),
            L(
                "MEDIUM", "UNKNOWN",
                ["Weather picture for {loc} is muddled. The forecast said one thing this morning and "
                 "something different at midday, people here are quoting both, and someone has heard a "
                 "warning was issued without knowing what for. It is grey and windy which tells you "
                 "nothing. I have no reliable forecast and no instrument here.",
                 "{loc} weather outlook unclear. Two conflicting forecasts circulating, a reported "
                 "warning of unknown type, and no instrumentation on site. Present conditions are "
                 "grey and windy, which is not informative."],
                "The weather outlook for {loc} is unclear, with conflicting forecasts circulating, a reported warning of unknown type, and no instrumentation on site.",
                cause="forecast unestablished", resources=["reliable forecast"], hint="{loc}",
                kind=KIND_AMBIGUOUS,
            ),
        ],
    ),
    F(
        "detailed_quake_damage_report",
        "EARTHQUAKE",
        [
            L(
                "HIGH", "RESTRICTED",
                ["Damage report from {loc} after the shake. {count} buildings have structural cracking "
                 "of which {smallcount} are bad enough that we have closed them. The water tower is "
                 "standing but one leg has moved and I do not trust it. {bigcount} people have been "
                 "relocated out of the closed buildings and are in the open. No injuries, which is "
                 "luck rather than anything else.",
                 "{loc} post-earthquake damage: {count} buildings cracked, {smallcount} closed as "
                 "unsafe, water tower leg displaced and suspect, {bigcount} people relocated outdoors "
                 "from the closed buildings. No injuries reported."],
                "An earthquake at {loc} has cracked {count} buildings with {smallcount} closed as unsafe and displaced a water tower leg, relocating {bigcount} people outdoors with no injuries.",
                cause="earthquake structural damage", asset="water tower",
                entities=["{count} buildings damaged", "{smallcount} closed as unsafe", "{bigcount} people relocated"],
                resources=["structural engineers", "temporary shelter"], hint="{loc}",
            ),
            L(
                "LOW", "OPEN",
                ["Post tremor check at {loc} finished. Went through all {count} buildings room by room "
                 "with the checklist. Found hairline cracking in three places, all of which match the "
                 "photographs from the last survey, so nothing new. Nothing has moved, no doors are "
                 "binding, no new gaps at the junctions. Everything signed off and back in use.",
                 "{loc} tremor inspection complete across {count} buildings using the checklist. "
                 "Three hairline cracks, all matching prior survey photographs. No movement, no "
                 "binding doors, no new junction gaps. All buildings signed off."],
                "A room-by-room inspection of {count} buildings at {loc} after a tremor found only pre-existing hairline cracking and no new movement, and all buildings have been returned to use.",
                cause="post-tremor inspection, no new damage",
                entities=["{count} buildings inspected"],
                resources=[], hint="{loc}",
                kind=KIND_FALSE_POSITIVE,
                note="Detailed, careful, and the answer is that nothing is wrong.",
            ),
        ],
    ),
    F(
        "detailed_other_site_report",
        "OTHER",
        [
            L(
                "HIGH", "RESTRICTED",
                ["Situation at {loc} is deteriorating and it is not an incident in the usual sense. "
                 "The distribution has been delayed twice, the crowd is now {bigcount} strong and "
                 "has been waiting {hours} hours, and the mood has turned. {count} of our staff have "
                 "been surrounded and shouted at. Nobody is hurt but the next delay will not pass "
                 "quietly. We need either the supplies or enough people to manage the crowd.",
                 "{loc}: twice delayed distribution, crowd of {bigcount} waiting {hours} hours, mood "
                 "hostile. {count} staff surrounded and shouted at, no injuries yet. Either the "
                 "supplies arrive or we need crowd management support before the next delay."],
                "Twice-delayed distribution at {loc} has left a crowd of {bigcount} waiting {hours} hours with hostility rising and {count} staff surrounded and shouted at, though no one has been hurt.",
                cause="crowd hostility following repeated distribution delays",
                entities=["{bigcount} people waiting", "{count} staff confronted"],
                resources=["crowd management support", "the delayed supplies"], hint="{loc}",
                note="OTHER at genuine HIGH severity — it fits no named category and is still serious.",
            ),
            L(
                "LOW", "OPEN",
                ["End of week summary for {loc}. {count} volunteers worked across the period, "
                 "{hours} hours of vehicle movements logged, stores reconciled with no discrepancies, "
                 "two pieces of equipment sent for repair and one returned. The rota for next week is "
                 "filled. Nothing outstanding and nothing that needs a decision.",
                 "{loc} weekly summary: {count} volunteers, {hours} hours of vehicle movement, stores "
                 "reconciled clean, two items to repair and one back, next week's rota complete. "
                 "No outstanding items."],
                "A weekly summary for {loc} records {count} volunteers, {hours} hours of vehicle movements, clean stores reconciliation, routine equipment repairs and a complete rota with nothing outstanding.",
                cause="routine weekly reporting", entities=["{count} volunteers"],
                resources=[], hint="{loc}",
            ),
        ],
    ),
]
