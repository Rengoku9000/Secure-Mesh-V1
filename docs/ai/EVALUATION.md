# SecureMesh — Local Intelligence Evaluation

Every number here was **measured**, by running the local model against a
deterministic synthetic corpus on the hardware named below. Nothing is
estimated, and nothing is carried over from a model card.

Raw output: [`benchmark-latest.json`](benchmark-latest.json). Reproduce it with

```powershell
cd src-tauri
cargo run --example run_benchmark -- 42 500 30 30
#                                     │   │   │  └ questions asked
#                                     │   │   └ incidents analysed
#                                     │   └ corpus size
#                                     └ dataset seed
```

The harness refuses to run without a provisioned model rather than reporting
zeros that could be mistaken for measurements.

---

## What was measured, on what

| | |
|---|---|
| Generation model | Qwen2.5-1.5B-Instruct, **Q4_K_M**, 1.04 GB, Apache-2.0 |
| Embedding model | BAAI bge-small-en-v1.5, **Q8_0**, 35 MB, 384-dim, MIT |
| Runtime | llama.cpp `b10375` prebuilt, **CPU only**, `-c 2048 -t 8` |
| Hardware | AMD Ryzen AI 7 350, 8C/16T, 23 GB RAM, Windows 11 |
| Prompt version | `analysis-v2/rag-v2` |
| Dataset | `synthetic-v2`, seed 42, 500 incidents, 24 questions |
| Harness build | `cargo` dev profile (see [note](#a-note-on-the-build-profile)) |

Prompt and dataset versions are recorded with every run because accuracy moves
with both. A figure without them is not comparable to anything.

---

## Results

### Structured extraction

| Metric | Result |
|---|---|
| Output that parsed and validated | **30 / 30 (100%)** |
| Category correct | **21 / 30 (70.0%)** |
| Severity correct | 13 / 30 (43.3%) |
| Keyword recall | 52.5% |
| Latency, mean / median | **2674 ms / 2536 ms** |
| Latency, min / max | 1753 ms / 4436 ms |

**Valid output at 100% is the load-bearing number.** Decoding is constrained to
the JSON schema, so the runtime cannot emit anything else — and the output is
then parsed, validated and bounded anyway, because a model is untrusted input
whether or not a decoder was supposed to constrain it.

**Category accuracy was 36.7% before the categories were described.** The first
version handed the model a bare enum. `INFRASTRUCTURE`, `ROAD_BLOCKAGE` and
`SEVERE_WEATHER` all plausibly cover "a landslip has closed the mountain pass",
and nothing in the identifiers settles which. Adding one line of description per
category — the same thing a new operator would be given — took it to 70.0% with
no other change.

Twelve categories, so chance is 8.3%.

### Retrieval and grounding

| Metric | Result |
|---|---|
| Answerable questions retrieving context | **19 / 19 (100%)** |
| Answers grounded in a supplied passage | **19 / 19 (100%)** |
| Unanswerable questions correctly refused | **4 / 5 (80%)** |
| Invented citations reaching a user | **0** |
| Retrieval latency, mean / median | **19 ms / 20 ms** |
| Answer latency, mean / median | 4011 ms / 4370 ms |

**Grounding was 0% before citation became a schema field.** Asked in prose to
write `[S1]` markers, the model complied in **0 of 19** answerable questions: it
answered correctly and cited nothing, or quoted incident IDs it found in the
passages. Asking harder was not going to fix that.

Citation is now a **required field in a constrained schema** — `sources` (the
passage numbers used) and `sufficient` (whether the context could answer at
all). A model cannot return an answer without also stating what it was built
from. Same corpus, same model, same retrieval: **0/19 → 19/19**.

The general lesson, which is worth more than the number: a small model follows a
*schema* far more reliably than an *instruction*. Where a property matters,
make it structural.

### Embedding

| Metric | Result |
|---|---|
| Latency, mean / median | **5 ms / 6 ms** |
| Latency, min / max | 5 ms / 7 ms |

500 incidents were embedded before retrieval was measured.

### The run reproduces

Seed 42 was run twice, hours apart, on the same build. Every accuracy figure was
**identical** — 30/30 valid, 21/30 category, 13/30 severity, 52.5% recall, 19/19
grounded, 4/5 refused, 0 invented citations — and the same unanswerable question
got through both times. Only wall-clock latency moved (mean analysis 2561 ms vs
2674 ms), which is what wall clock does.

That is by construction rather than luck: the corpus is generated from a seeded
PRNG, and every model call is at temperature 0. It matters because it means a
change in these numbers is a change in the *system*, not sampling noise — which
is the only way "36.7% → 70.0%" earlier in this document can be read as an
effect rather than a coincidence.

### Held on a second corpus

A single seed can flatter a pipeline that happens to suit it. Re-run against a
different corpus — **seed 7, 300 incidents, 20 analysed** — with nothing else
changed:

| Metric | Seed 42 / 500 | Seed 7 / 300 |
|---|---|---|
| Valid structured output | 100% | **100%** |
| Category accuracy | 70.0% | **80.0%** |
| Severity accuracy | 43.3% | **25.0%** |
| Keyword recall | 52.5% | 55.0% |
| Grounded answers | 19/19 | **19/19** |
| Correctly refused | 4/5 | **4/5** |
| Invented citations | 0 | **0** |
| Analysis latency, median | 2536 ms | 2728 ms |

The structural properties — valid output, grounding, citation integrity — are
stable across corpora, which is what one would expect of properties enforced by
schema rather than by model behaviour. The *accuracy* figures move by ten to
twenty points on samples of 20–30, which is the more useful reading of them:
they are indicative, not precise. Severity moving from 43.3% to 25.0% on
unchanged code is the clearest evidence that the severity metric is noise as
much as signal — see [below](#severity-accuracy-is-the-weakest-number-here-and-partly-unlearnable).

The same unanswerable question got through on both runs, which is at least a
reproducible failure rather than a random one.

---

## Offline verification

Measured, not asserted. Sampled repeatedly across two separate benchmark runs
while they were mid-flight, covering both the extraction and the retrieval
phases — eleven samples in total:

```
sample 1: processes=3 tcp_endpoints=5 non_loopback=0
    pid 4732  0.0.0.0:59826    -> 0.0.0.0:0        Bound
    pid 4732  127.0.0.1:59826  -> 127.0.0.1:19100  Established
    pid 8424  127.0.0.1:19101  -> 0.0.0.0:0        Listen
    pid 15252 127.0.0.1:19100  -> 127.0.0.1:59826  Established
    pid 15252 127.0.0.1:19100  -> 0.0.0.0:0        Listen
...
sample 6: processes=3 tcp_endpoints=5 non_loopback=0
```

Every sample: **zero non-loopback endpoints, and zero UDP endpoints**. Both
model servers bind `127.0.0.1` explicitly, and the only connection is the
application's own hop to them.

Re-measured after the operational knowledge pack was added, sampling every
400 ms across a full run — install, embed 33 chunks, and answer six questions:

```
samples with AI processes alive : 40
loopback / listening endpoints  : 119
EXTERNAL endpoints observed     : 0
```

The knowledge pack itself cannot reach the network by construction: the eleven
documents are compiled into the binary with `include_str!`, so installing them
copies text that is already in memory. There is no download step to observe,
and `tests/operational_knowledge.rs` asserts over the source that no HTTP
client, socket or file-read appears on that path.

**What this does not prove.** These samples were taken on a machine that *had*
a working Internet connection; they show the process never used it. That is the
honest claim. A test with the adapter physically disabled has not been run here.

The claim is also structural, and checkable without watching sockets:

```powershell
cd src-tauri
cargo tree | Select-String "reqwest|hyper|ureq|isahc|curl"
# no HTTP client crate in the dependency tree
```

The runtime is reached through `src/ai/loopback_http.rs`, which takes a **port**
and builds its address from `Ipv4Addr::LOCALHOST`. There is no hostname
parameter, no URL parsing and no name resolution anywhere on that path, so
reaching a remote host is not forbidden — it is unrepresentable.
`tests/ai_boundary.rs` asserts this against the source.

**Being precise about the dependency tree**, since a reader may check: `rustls`
and `hickory-proto` *are* present. Both arrive through `libp2p`, for the Phase 2
mesh — `hickory-proto` via `libp2p-mdns`, which needs to parse DNS-format
packets on the local multicast group, not to resolve names against a server.
Neither is reachable from the AI path, and neither is an HTTP client. The claim
is "no HTTP client, and no resolution on the inference path", not "no
networking crates at all" — SecureMesh is a mesh, and it obviously has a network
stack.

---

## What these numbers do not mean

### The corpus is synthetic, and easier than reality

Descriptions are generated from templates. Real field reports are messier,
partial, and inconsistently worded, so every accuracy figure here is an **upper
bound on an easier task** — not a prediction of field performance. The corpus is
labelled `SYNTHETIC` in storage so it can never be mistaken for operational
data.

### Severity accuracy is the weakest number here, and partly unlearnable

43.3% looks poor, and the metric deserves the scepticism more than the model
does:

- The corpus assigns a **base severity per scenario family** — `POWER` is
  `MEDIUM`, `FIRE` is `CRITICAL` — a convention the model is never told and
  cannot infer from the text.
- **20% of reports are escalated one level**, and the escalation is *not
  reflected in the description*. Those labels are unpredictable from the input
  by construction, capping a perfect score well below 100%.

Fixing this by writing a severity rubric into the prompt would mean encoding the
generator's own convention — tuning to the test set, and a number that measures
nothing. It is left alone and reported as it stands.

What makes this tolerable in practice is a design decision, not a metric: **the
model's severity never overwrites the operator's.** It is stored alongside, and
disagreement is surfaced in the UI. `tests/ai_boundary.rs` asserts it
(`analysis_never_alters_the_operators_own_severity`).

### The refusal set is five questions

80% means four of five. The harness records which one by name in
`answeredUnanswerable`, because the failing case is the part worth acting on and
a rate alone would hide it:

> *"What is the recommended torque setting for a helicopter rotor bolt?"*

That is the instructive failure. The other four — a stock price, a football
result, satellites in orbit, the capital of Mars — share no vocabulary with a
corpus of field reports, so retrieval returns nothing above the 0.35 relevance
threshold and the model is never called. This one reads like maintenance: bolts,
settings, structures. Retrieval found infrastructure passages that were
genuinely similar in wording and genuinely useless as an answer, and the model
then judged them sufficient.

The lesson is about **where refusal comes from**. Four of the five refusals were
free — retrieval simply found nothing, and a question with no context is refused
without any model judgement at all. The fifth needed the model to notice that
topically-similar passages did not actually answer the question, and it did not.
Refusal is only as strong as retrieval where the two agree, and only as strong
as a 1.5B model's judgement where they do not.

Five is too few to be confident about the rate. It is enough to establish that
refusal happens, that both mechanisms are exercised, and that the weaker of the
two is the model's.

Five is too few to be confident about the rate. It is enough to establish that
refusal *happens* and that the mechanism is exercised, and not enough to quote
80% as a property of the system.

### Grounding is containment, not correctness

An answer marked grounded cites a passage that was genuinely retrieved, **and**
its wording is drawn from the passages the model was shown. Nothing verifies
that its claims *follow* from those passages: a model can quote a real source and
still draw the wrong conclusion from it. See `SECURITY.md` §6.19.

### The answer-support check, and why it was needed

Measured on this machine after the operational knowledge pack was installed
(11 documents, 33 chunks), with the real BGE and Qwen2.5-1.5B:

| Question | Top retrieval score | Support | Verdict |
|---|---|---|---|
| What should I do in heavy rain? | 0.716 | 1.00 | answered |
| What should I do during an avalanche? | 0.722 | 1.00 | answered |
| What happened at Mount Abu? | 0.720 | 1.00 | answered |
| Avalanche at Mount Abu — what should the team do? | 0.733 | 0.94 | answered |
| Heavy rain road blockage — what should the team do? | 0.845 | 0.93 | answered |
| **What is the capital of France?** | **0.395** | **0.00** | **refused** |

The last row is the reason the check exists. Retrieval returned five
emergency-procedure passages *above* the 0.35 relevance threshold — with eleven
documents of English prose, something is always slightly related to any English
sentence — and the model then declared the context sufficient, cited all five,
and answered "Paris". Every gate that existed had passed.

The margin between a genuine answer (0.93–1.00) and a fabricated one (0.00) is
wide enough that the 0.40 threshold is not a fine judgement. It does, however,
assume the generator quotes its context, which Qwen2.5-1.5B does; a heavily
paraphrasing model could be refused despite being correctly grounded.

`droppedCitations` reports source numbers the model named that were never
supplied. They are discarded before display; a non-zero count is the fabrication
filter reporting that it fired, and is surfaced to the operator rather than
silently swallowed.

### Keyword recall measures phrasing as much as understanding

52.5% is recall over expected keywords appearing anywhere in the analysis.
Recall deliberately, not exact match: a model that writes "structural failure"
where the corpus said "collapse" has not misunderstood anything. The figure is
therefore a floor.

### A note on the build profile

The harness is a `cargo` dev-profile build. Inference runs in a separate
prebuilt `llama-server` process, so the profile affects orchestration overhead —
JSON handling, SQLite calls — and not the model itself. Retrieval latency (18 ms
over 500 vectors, brute-force cosine) would improve in release; generation
latency would not meaningfully move. Release builds are currently blocked on
this machine by Windows Smart App Control, which is noted rather than worked
around.

---

## Rule layer, semantic fallback, and similarity calibration

Measured by `cargo run --example nlp_evaluation` — no benchmark harness or
database involved; the rule layer needs neither, and the two similarity parts
run against the local BGE embedding model when it is provisioned.

```powershell
cd src-tauri
cargo run --example nlp_evaluation --release
```

### Category accuracy

| Set | Rules only | Semantic only | Rules + fallback |
|---|---|---|---|
| Synthetic, seed 42, n=500 | **96.8%** | — | — |
| Synthetic, seed 7, n=300 | **96.7%** | — | — |
| Held-out, hand-written, n=36 | 69.4% (8 with no cue) | 72.2% | **83.3%** |

The synthetic figure is the same upper-bound caveat as everywhere else in this
document — the rule vocabulary was written by someone who had read the
templates it is scored against. The held-out set is hand-phrased,
independently, and is the more honest number: rules alone answer 69.4%, and
the semantic fallback recovers roughly half of the rest, for 83.3% combined.
Six of thirty-six still miss — mostly medical and resource-shortage phrasing
distant from both the rule cues and the category prototype text (e.g. "Woman
in labour needs urgent help" and "The river has breached the embankment" match
nothing and fall to `OTHER` untouched).

### Similarity bands — measured against the thresholds in `ai/insight.rs`

| Pair type (n=6–7) | Measured cosine (min / mean / max) | Threshold |
|---|---|---|
| Same event, restated | 0.796 / 0.851 / 0.962 | `DUPLICATE_MIN = 0.93` |
| Same kind, different event | 0.579 / 0.626 / 0.691 | `POSSIBLE_DUPLICATE_MIN = 0.86`, `RELATED_MIN = 0.76` |
| Unrelated | 0.425 / 0.517 / 0.623 | — |

**The thresholds are not calibrated to this data, and it shows.** Of seven
hand-labelled "same event, restated" pairs, only one scored above 0.93 and was
correctly banded `DUPLICATE`; two landed as `POSSIBLE_DUPLICATE`, and four —
more than half — scored as merely `RELATED`. The bands separate genuinely
unrelated pairs cleanly (nothing above 0.691 was misbanded), so the mechanism
works; the specific cut points chosen (0.93 / 0.86 / 0.76) were not derived
from measurement and sit too high for `DUPLICATE` given what restating a real
report actually looks like on this model. **Open item, not silently shipped**:
recorded in `docs/architecture/ROADMAP.md` (Phase 3), not yet fixed.

---

## Latency, summarised

On CPU, no GPU, on the hardware above:

| Operation | Median |
|---|---|
| Single text embedding | **6 ms** |
| Vector retrieval over 500 items | **18 ms** |
| Structured incident analysis | **2.4 s** |
| Grounded answer (retrieval + generation) | **4.4 s** |

Interactive for an operator working one incident at a time. Not interactive for
bulk analysis — indexing 500 incidents is an explicit, batched, backgroundable
action for that reason.
