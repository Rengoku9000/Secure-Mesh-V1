# SecureMesh — Architecture

**Phases 1 and 2 are implemented. Phases 3–5 are design documents, not code.**
Each section is labelled.

---

## 1. The shape of the system

A SecureMesh node is a **self-contained unit**. It holds its own identity, its
own data, and (eventually) its own inference. It cooperates with other nodes
when they are reachable and loses no capability when they are not.

This is the central architectural constraint, and it rules out the common
default:

```
  ✗ REJECTED                         ✓ SECUREMESH
  React → REST API → cloud DB        React → IPC → Rust core → local SQLite
                                                             → local mesh
                                                             → local AI
```

Not because client/server is bad, but because the deployment environments this
targets — disaster response, remote healthcare, field operations — are exactly
the environments where the server is unreachable. A design whose core function
degrades without connectivity has failed before it is deployed.

There is **no mandatory central server**. An optional gateway may be added later
for deployments that *do* have infrastructure, but it can only ever be an
addition — never a dependency.

---

## 2. Layers

```
┌───────────────────────────────────────────────────────────┐
│ UI LAYER — React 19 + TypeScript + Vite                   │
│ Renders state. Holds no business logic.                   │
└──────────────────────────┬────────────────────────────────┘
                           │  Tauri IPC (typed commands)
┌──────────────────────────▼────────────────────────────────┐
│ COMMAND LAYER — src-tauri/src/commands/                   │
│ Thin. Unwraps state, forwards, returns. No decisions.     │
└──────────────────────────┬────────────────────────────────┘
┌──────────────────────────▼────────────────────────────────┐
│ RUNTIME — src-tauri/src/runtime.rs                        │
│ Owns identity + storage. Makes every operational decision. │
│ Free of Tauri types, so it is testable standalone.        │
└───┬───────────────┬───────────────┬───────────────┬───────┘
    │               │               │               │
┌───▼──────┐  ┌─────▼─────┐  ┌──────▼──────┐  ┌────▼──────────┐
│ IDENTITY │  │  DOMAIN   │  │   STORAGE   │  │   SECURITY    │
│ Ed25519  │  │ Incident  │  │  SQLite     │  │ Secret<N>     │
│ KeyStore │  │ Event     │  │  event log  │  │ audit         │
│ trait    │  │ Peer      │  │ watermarks  │  │ redaction     │
└──────────┘  └───────────┘  └─────────────┘  └───────────────┘
       │                            ▲
┌──────▼─────────────────┐          │
│ SYNC ENGINE            │──────────┘  reconciles the log
│ pull-based, idempotent │
└──────┬─────────────────┘
┌──────▼──────────────────────────────────────────┐
│ NETWORKING — MeshTransport (trait)              │
│   ├── Libp2pTransport   QUIC + mDNS  (real)     │
│   └── LoopbackTransport deterministic (tests)   │
└─────────────────────────────────────────────────┘

     PHASE 3 ─── local inference                 (not built)
     PHASE 4 ─── local RAG                       (not built)
     PHASE 5 ─── TEE                             (not built)
```

### Why the runtime is separate from the commands

`NodeRuntime` contains no Tauri types. That single constraint means the whole
core can be constructed in a test with a temporary directory and driven exactly
as the UI drives it — which is how `src-tauri/tests/node_lifecycle.rs` verifies
restart persistence without launching a window. If business logic lived in the
command functions, none of it would be testable without a running application.

---

## 3. Key design decisions

| Decision | Alternative rejected | Reasoning |
|---|---|---|
| Rust core, thin React | Logic in the frontend | Security-sensitive code belongs in a memory-safe, testable layer. The WebView is treated as untrusted. |
| Tauri 2 | Electron | Far smaller footprint on constrained edge hardware; Rust backend is the point, not an accessory. |
| SQLite (file, bundled) | Embedded KV, or a server DB | Relational integrity via CHECK constraints and foreign keys; the `bundled` feature means no external SQLite to install on a field device. |
| UUIDv4 primary keys | Auto-increment integers | Nodes create records while partitioned. Sequential IDs would collide the moment two offline nodes sync. |
| RFC 3339 text timestamps | Unix integers | Fixed-width UTC text sorts lexicographically *and* chronologically, and stays human-readable when inspecting a database in the field. |
| Ed25519 | RSA, ECDSA | Small keys and signatures, fast verification, no parameter choices to get wrong. Matters when a low-power node verifies many peer records. |
| Identity derived from the key | Assigned/random node IDs | A node cannot claim an identity it does not hold the private key for. |
| `KeyStore` trait | Direct file access | The seam that lets TPM/TEE backends replace software storage without touching the core. |
| One key for identity *and* transport | Separate transport key + binding certificate | The handshake already proves possession; a separate key would mean writing custom crypto to prove the same thing. Cross-protocol risk is handled by domain separation. Cost: conflicts with non-extractable hardware keys (`SECURITY.md` §6.9). |
| Append-only events | Mutable records with a merge rule | Immutability makes merging set union, which converges with no causality metadata and no conflict resolution to get wrong. |
| Per-origin sequence numbers | Timestamps, Lamport/vector clocks, CRDT | Sufficient for union; field clocks are untrustworthy; richer schemes buy ordering this domain does not need. |
| `MeshTransport` trait | Calling libp2p from the sync engine | Lets partition/replay/restart be tested deterministically in-process, and keeps libp2p replaceable. |
| Pull-based sync | Push with a per-peer outbound queue | The log plus an ack watermark already describes what a peer needs; a second buffer could drift and would need its own durability. |
| Centralised CSS tokens | Per-component colours | Both themes define an identical, complete token set, which structurally prevents "invisible text in dark mode". |

---

## 4. Data model

```
  events  (the replicated log — append-only, never updated)
  ──────
  event_id (uuid)  PK
  origin_node          ─── author's node_id
  origin_public_key    ─── makes the event verifiable standalone
  origin_seq           ─── monotonic per origin, from 1
  kind                 ─── INCIDENT_CREATED | INCIDENT_OBSERVATION
  payload              ─── the origin's own bytes, never re-encoded
  created_at           ─── display only, NEVER used for ordering
  signature, content_hash, received_at
  UNIQUE (origin_node, origin_seq)   ◄── turns equivocation into a
                                          detectable error

        │ projected, in the same transaction
        ▼
  nodes                        incidents
  ─────                        ─────────
  id ◄──────────────────────── created_by  (FK, RESTRICT)
  node_name                    id, description, severity
  public_key (UNIQUE)          latitude/longitude  CHECK paired + in range
  status LOCAL|ONLINE|OFFLINE  created_at / updated_at
  last_seen                    sync_status   CHECK enum
  transport_peer_id            origin_event_id ──▶ events
  protocol_version
  capabilities, equivocating   incident_observations
                               ─────────────────────
  sync_watermarks              id, incident_id, author_node, note
  ───────────────              event_id ──▶ events
  origin_node, watermark

  peer_ack_watermarks          event_conflicts
  ───────────────────          ───────────────
  peer_node_id, origin_node    both versions of an equivocation,
  acked_through                kept verbatim for audit
```

Three properties of this shape are load-bearing:

1. **The log is authoritative; `incidents` is a projection.** Local creation and
   remote replication both append an event and project it *in one transaction*,
   so the two can never disagree after a crash, and the projection can be
   rebuilt from the log.
2. **`events.origin_node` is deliberately not a foreign key.** An event may
   arrive relayed from a node this device has never had a session with. It
   carries its own public key and is self-verifying, so it needs no prior
   registration to be trustworthy.
3. **`incidents.created_by` *is* a foreign key.** Projecting a remote event
   therefore registers its author first, using the public key already verified
   against the node ID. The database structurally cannot hold a record
   attributed to a node whose key is unknown.

`messages` and `sync_events` were created by migration 001 speculatively and
remain unused; the Phase 2 design made them unnecessary. They are left in place
rather than dropped, because dropping them would gain nothing and cost a
migration on deployed nodes.

Existing Phase 1 incidents are **backfilled** into the log at startup, not in
SQL: an event must be signed, and only the running node holds the key. Only
incidents this node authored are eligible, which is complete for a Phase 1
database because there was no networking.

---

## 5. Phase 2 — Peer-to-peer networking (IMPLEMENTED)

```
   Node A ←──→ Node B          and          A ←→ B ←→ C
   (direct, QUIC)                           (multi-hop; A and C never meet)
```

### 5.1 Discovery

mDNS on the local link. **No bootstrap server, no DHT, no Internet.** A node
that finds no peers is in a normal state, not an error state — the dashboard
says "Listening", not "Failed".

Kademlia was considered and left out: it solves finding peers across a wide
network, which is the opposite of the deployment this targets.

### 5.2 Transport

QUIC only, via `libp2p-quic`:

- TLS 1.3 built in rather than layered on;
- connection migration survives an IP change when a node moves between links;
- streams are multiplexed without head-of-line blocking, so a large sync does
  not stall a control message.

**TCP/Noise/Yamux fallback was deliberately not included.** It would roughly
double the transport surface to serve networks that block UDP — a case that has
not arisen on a local link. It can be added later without touching anything
above `MeshTransport`.

### 5.3 Authenticated sessions

The node's Ed25519 key **is** its libp2p identity key, so the `PeerId` that QUIC
authenticates is derived from the same key that signs events, and:

```
   node_id = SHA-256(peer's authenticated public key)
```

A completed session is therefore already proof that the peer is the SecureMesh
node it claims to be. **No custom handshake and no custom cryptography** were
written — the alternative (a separate transport key bound by a home-made
certificate) would have meant implementing crypto to prove what the handshake
already proves.

Key reuse across protocols is a real hazard and is handled by domain separation;
see `SECURITY.md` §5.8. The single place key material leaves the identity module
is `identity/transport.rs`, isolated so it is easy to audit.

**Trust-on-first-use remains insufficient.** Authentication proves a peer holds
its key; it does not prove the peer is *authorised*. Enrolment is Phase 3 work
and is recorded as the most significant Phase 2 limitation (`SECURITY.md` §6.6).

### 5.4 Record exchange and store-and-forward

Every event is signed by its author and carries its author's public key, so it
verifies standalone. A receiver checks the signature against **the origin's**
key, never the delivering peer's. That is what makes multi-hop safe: B relaying
A's event does not require C to trust B.

**There is no separate outbound queue for sync.** The log *is* the queue: what a
peer still needs is derived from the log minus that peer's acknowledgement
watermark, both of which are in SQLite. A second buffer would be one more thing
to keep in step with the log, and one more thing to lose on a crash.

### 5.4b Synchronisation is caused, not awaited (Phase 2.6)

Phase 2.5 had a defect that only showed up in a live demonstration: two peers
connected, were approved by both operators, and then exchanged nothing for
about seventy seconds.

**Root cause.** There was no trigger for authorization. Only two things ever
started a round — a connection, and a five-second timer. Approving a peer that
was *already connected* therefore had no causal path to replication at all; the
timer was doing the work, and the timer was doing it for reasons unrelated to
what had just happened. The same hole applied to a record written during an
open session.

This is reproduced deterministically in `tests/sync_determinism.rs`, which never
calls the timer: against the Phase 2.5 engine **11 of its 16 cases fail**. The 5
that passed were all reconnection and restart cases — exactly the ones that
happen to hit the connection trigger, which is why the failure looked
intermittent and why restarting the application "fixed" it.

**The fix** is an explicit per-peer lifecycle plus a trigger for every cause:

```text
   Disconnected ──▶ Connected ──▶ Authenticated ──▶ AwaitingAuthorization
                                        │                    │
                                        └──── authorized ────┤
                                                             ▼
                                                    Syncing ⇄ Synced
```

| Trigger | Fires when |
|---|---|
| `connected` | A session is established, from **either** direction |
| `local_authorization` | This node approves or reinstates a peer |
| `local_event` | A record is written locally |
| `peer_ahead` | A peer's request reveals it holds more than this node |
| `relay` | Events were accepted, and another peer may lack them |
| `manual` | An operator asks explicitly |
| `reconciliation` | The periodic safety net — see below |

Every one of them funnels into a single `open_round`, so a new cause cannot
acquire its own subtly different rules. That is precisely how the original gap
arose: connection had a path and authorization did not.

**Direction independence.** A round carries the sender's watermarks, so a
responder that finds itself behind reciprocates. One request from *either* side
therefore reconciles *both*, which is what makes convergence independent of who
dialled, who approved first, and who happens to hold the data.

**The periodic sweep is now a safety net, not the mechanism.** It runs every 60
seconds (was 5) and exists only to recover a message lost to a transport failure
that produced no disconnection event. The determinism tests never run it at all.
If correctness ever appears to need it faster, a trigger is missing.

**A second, related defect** was fixed at the same time: `tick` propagated
errors out of the event loop, and because `poll_events` had already *drained*
the transport, every remaining event in that batch was discarded permanently.
One bad event could therefore silently lose a `PeerConnected`, leaving a peer
connected but invisible to the engine. Each event is now handled independently.

### 5.5 Synchronisation model

Pull-based. A node states what it holds; the peer computes the difference:

```
   A                                    B
   │── SYNC_REQUEST  have:{A:5, B:2} ──▶│
   │◀─ SYNC_RESPONSE available:{B:7} ───│
   │◀─ EVENT_BATCH   B:3..7 ────────────│
   │── ACK           B accepted:7 ─────▶│   (persisted by B)
```

Each origin's log is ordered by a **monotonic sequence number starting at 1**. A
node's knowledge of an origin is one integer: the highest *contiguous* sequence
it holds. With 1, 2 and 4 present the watermark is 2 — so the next request asks
from 3, and event 4, already stored, folds in when the gap closes. Out-of-order
delivery is therefore safe by construction rather than by special handling.

A `SYNC_REQUEST` also reveals what the *requester* holds, so a responder that
finds itself behind asks in return. Without that reciprocity, a node creating an
event during an already-open session could not announce it.

**Wall-clock time is never used for ordering.** Field devices without GNSS drift,
and a timestamp sort would reorder history whenever a clock stepped.
`created_at` is recorded for display only.

#### Why this model, and not a CRDT

The set of per-origin watermarks is a **version vector in its minimal form** —
worth naming honestly rather than dressing up. It is sufficient because Phase 2
events are immutable, which makes merging two logs **set union**, and union is
associative, commutative and idempotent on its own. It needs no causality
metadata to converge.

Lamport clocks, vector clocks with causal parents, or a full CRDT would buy a
happens-before ordering the incident domain does not need, at the cost of
metadata on every record and considerably more that can go subtly wrong.

**The tradeoff is explicit: this model is correct only while events are
immutable.** Unrestricted mutable editing would require revisiting it.

### 5.6 Conflict handling

Phase 2 is **append-only**, so ordinary concurrent work produces no conflicts:
two nodes creating incidents during a partition produce disjoint sets that merge
by union, and neither can overwrite the other. Updates are appended as
observations rather than mutating the original.

That leaves exactly one genuine conflict: **equivocation** — one origin signing
two *different* events at the same sequence number, meaning it forked its own
log. The response is deterministic and non-destructive:

1. the event already held is kept as authoritative;
2. the conflicting event is stored verbatim in `event_conflicts` for audit,
   along with which peer reported it;
3. the origin is flagged `equivocating`, and replication from it stops
   advancing;
4. the UI shows the peer as having a conflicting history.

Nothing is silently overwritten and nothing is discarded.

### 5.7 What is deliberately still missing

- Peer enrolment and revocation (`SECURITY.md` §6.6)
- End-to-end encryption for multi-hop paths (§6.7)
- Per-peer rate limiting (§6.10)
- Mutable incident editing, and the conflict model it would require

---

## 6. Phase 3 — Local AI and RAG (IMPLEMENTED)

```text
   incident text ──▶ fenced as data ──▶ local model ──▶ schema-constrained JSON
                                                              │
                                          parse ──▶ validate ─┤
                                                              ▼
                                                   derived intelligence
                                                       (not replicated)

   question ──▶ local embedding ──▶ local vector search ──▶ top-k passages
                                                                 │
                                       local model ◀─ context ───┘
                                             │
                                             ▼
                                  answer + verified citations
```

### 6.1 Model selection

Chosen against the criteria in the brief, on hardware that was measured rather
than assumed. Full reasoning, licences and checksums:
[`ai/models/README.md`](../../ai/models/README.md).

| | Choice | Why |
|---|---|---|
| Generation | Qwen2.5-1.5B-Instruct, Q4_K_M, 1.04 GB | Reliable schema-constrained output — the deciding criterion, since an unparsable analysis is worthless. Apache-2.0. Fits edge hardware |
| Embedding | BGE-small-en-v1.5, Q8_0, 35 MB, 384-dim | MIT. Q8 over Q4 because retrieval quality depends directly on embedding fidelity and 35 MB is already negligible |
| Runtime | llama.cpp b10375, prebuilt, CPU | Building from source needs CMake/Clang/Ninja, none otherwise required. CPU-only is the honest baseline for Phase 6 edge hardware |

A 7B model was rejected: several times the CPU latency for a task that is mostly
classification and field extraction, and it stops fitting the target hardware.

### 6.2 The runtime is a supervised child process

Rather than linking llama.cpp into the binary, the official server runs as a
child process reached over loopback. Beyond avoiding a heavy build toolchain,
this buys **failure isolation** — a model that exhausts memory or crashes takes
down a child process, not the node — and a **clean boundary** that shares no
memory with SecureMesh.

The loopback client is hand-written (`src/ai/loopback_http.rs`) specifically to
avoid adding an HTTP client to the dependency tree, which would falsify the
verifiable "no HTTP client" property. It takes a **port**, not a host: its
address is built from `Ipv4Addr::LOCALHOST`, and there is no hostname parameter,
no URL parsing and no name resolution, so reaching the Internet is
unrepresentable rather than merely forbidden.

`rustls` and `hickory-proto` do appear in the tree — both via `libp2p` for the
Phase 2 mesh, the latter through `libp2p-mdns`, which parses DNS-format packets
on the local multicast group. Neither is reachable from the AI path. The claim
is "no HTTP client, and no resolution on the inference path", not "no networking
crates at all".

### 6.3 Model output is untrusted

`RawAnalysis` → validate → `IncidentAnalysis`, mirroring `NewIncident` →
`Incident` exactly: the invalid state is unrepresentable rather than merely
checked. `deny_unknown_fields` means a model cannot express a field like
`trust_state` at all.

The model's severity is stored *beside* the operator's, never over it. See
`SECURITY.md` §5.15 for the full trust boundary.

### 6.4 Derived intelligence is local and disposable

Analyses and vectors are **not replicated**. An inference is an opinion produced
by a particular model, and two nodes running different models will legitimately
disagree — replicating opinions as facts would corrupt the one thing the mesh
does guarantee. Every node analyses independently, from data it already holds.

Dropping every derived row leaves the operational record intact.

### 6.5 Retrieval

Vectors are stored as `f32` BLOBs and scanned brute-force. A vector extension
would mean a native dependency on every target platform, for a corpus measured
in thousands of chunks. Measured: **18 ms** to embed a question and scan 500
vectors — against roughly **4 s** to generate the answer, so retrieval is not
where the time goes. That trade stops holding somewhere in the tens of
thousands; the interface is the same shape an approximate index would need, so
replacing it later does not disturb callers.

One table serves both knowledge chunks and incidents, because a question must be
able to rank a procedure and a field report against each other. Nothing selects a
category first: ranking is by score alone across the single table, which is why
one answer can cite an operational procedure and a live incident together.

### 6.6 Grounding is enforced, not requested

A model *asked* to cite its sources will sometimes cite ones that do not exist —
and a small one will often not cite at all. Measured on the evaluation corpus,
asking Qwen2.5-1.5B in prose for `[S1]` markers produced citations in **0 of 19**
answerable questions: it answered correctly and cited nothing, or quoted record
IDs it found in the passages. Asking harder would not have fixed that.

Citation is therefore a **field in a constrained schema**, not a request. The
answer must carry `sources` (the passage numbers used) and `sufficient` (whether
the context could answer at all), so a model cannot return an answer without
also stating what it was built from. With the same corpus, model and retrieval,
grounding went from 0/19 to 19/19.

The numbers are then checked against the passages actually supplied and invented
ones discarded — reported as `droppedCitations` rather than silently swallowed.
If retrieval finds nothing the model is never called at all, and the refusal is
returned directly. `sufficient: false`, an empty answer, or output that will not
parse all become the same canonical refusal. An answer citing nothing is
reported as *model interpretation*, not as fact.

Citation alone stopped being enough once the corpus grew past a handful of
incidents. With eleven operational documents indexed, "What is the capital of
France?" retrieved five passages at 0.38–0.40 — over the 0.35 threshold — and
Qwen declared the context sufficient, cited all five, and answered "Paris". So a
fourth check compares the answer's content words against the passages it cites,
and refuses below `MIN_ANSWER_SUPPORT`. Measured on that run: genuine answers
above 0.8, "Paris" at 0.00.

This verifies containment, not meaning — a model can still quote a real passage
and draw the wrong conclusion from it (`SECURITY.md` §6.19).

### 6.7 AI is a layer, never a dependency

The service is an `Option` on the runtime, and analysis is never on the path of
incident creation or synchronisation. A node with no model, or a crashed
runtime, keeps capturing and replicating incidents and reports intelligence as
unavailable. `tests/ai_boundary.rs` asserts this.

### 6.8 Incident insight and the situation brief

`ai/nlp.rs` is a deterministic rule layer — hazard cues, people counts and
status, locations, routes, and an explainable severity score — that reads a
report in microseconds with no model loaded. `ai/insight.rs` is pure on top of
it: handed incidents, extractions and whatever vectors exist, it returns
category choice, related/duplicate matches, and severity factors, reading no
database and calling no model itself. Nothing either produces is stored or
replicated; two nodes may derive different insights about the same incident,
which is why none of it goes on the wire (§6.4 applies here too).

Related-report matching degrades rather than disappears: semantic (embedding
cosine) when a vector exists, lexical (stemmed word and hazard-set overlap)
when it does not, with each threshold band tuned independently for its method.
See `docs/ai/EVALUATION.md` for the measured similarity bands — the current
duplicate/possible/related cut points are not yet calibrated against them.

The situation brief (`insight::build_brief`) aggregates every incident a node
holds — counts, summed people totals, blocked routes, a priority ranking, and
duplicate groups — with no model call. A prose summary is optional and, when
asked for, is generated *from the brief's own figures only* and then checked
by the same containment method §6.6 uses for grounded answers: a summary that
is not supported by the figures it was given is withheld and the reason
stated, never shown.

### 6.9 A gate in front of the generation model

The generation runtime is one CPU-bound process; two requests in flight finish
no sooner and both finish late. `ai/gate.rs` (`InferenceGate`) admits one
request, lets a bounded few queue briefly, and refuses the rest immediately
with a message that says why. It guards only the generation model — embedding
is milliseconds with its own process, and nothing on the incident-capture or
sync path acquires it — so a full queue can delay a *question*, never a
record. Every Tauri command that can reach a model is `#[tauri::command(async)]`
so a multi-second wait runs on Tauri's thread pool instead of the UI thread.

## 6b. Original Phase 3 design notes (superseded)

```
   Incident text
        │
        ▼
   Local inference runtime  (llama.cpp / Candle / ONNX Runtime — not yet chosen)
   Open-weight model, quantised, loaded from local disk
        │
        ├──► classification      (incident type, priority)
        ├──► entity extraction   (locations, resources, casualties)
        └──► summarisation       (situation digest across many incidents)
        │
        ▼
   Structured intelligence, stored locally
```

Constraints: no external API, ever. The model file is a local asset with a
verified hash; model weights are excluded from version control. Inference must
degrade gracefully — a node with no model installed reports "not installed" and
continues to function, exactly as it does today.

Runtime selection is deferred until Phase 6 hardware is chosen, because the
right answer differs between a Jetson-class GPU device and a CPU-only SBC.

---

### 6c.4 Node location heartbeat

```text
   LocationProvider ──▶ publish_location() ──▶ Envelope (signed)
                                                    │
                                          authorized peers only
                                                    │
                                                    ▼
                                        PeerLocationBook (in memory)
                                                    │
                                                    ▼
                                             Tactical Map
```

Every five minutes a node takes a position and tells its authorized peers. The
first publication happens as soon as a position is available rather than after
the first interval — a node invisible for the first five minutes is invisible
for the most useful five minutes.

**On its own thread.** Obtaining a fix blocks: the Windows location service can
take twelve seconds. Doing that on the sync pump, which ticks every hundred
milliseconds, would stall replication for the duration of every fix. One thread
for the node, never one per peer.

**A failed fix publishes nothing and advances no sequence.** Nothing is
invented, and a counter that moved without a position would tell peers a
heartbeat had been missed rather than never made. Failures retry on a shorter
interval, bounded so a device with no receiver is not asked constantly.

#### Ordering is by sequence, not by clock

Each node keeps a monotonic counter, and a receiver refuses anything not
strictly newer than what it holds. This is the same principle the event log uses
for replication, applied to ephemeral state: two nodes do not share a clock, and
comparing timestamps would let a peer with a fast clock overwrite fresher data.
The counter lives in memory — it orders the heartbeats of one process lifetime,
and a restart re-announces from 1 to peers that have no record of the previous
run either.

#### Ephemeral, not a record

The receiver holds one entry per peer, always the latest, in memory. It is never
written to the event log, never stored as an incident, and never replicated
onward. An incident is something that happened and is worth keeping forever; a
node being somewhere five minutes ago is not, and a heartbeat writing to an
append-only log would add hundreds of rows a day that nobody reads.

Nothing queues. A peer that is disconnected simply misses a heartbeat and gets
the next one, so an hour out of contact costs one entry on return rather than
twelve.

#### Freshness is not reachability

| Age | State | Shown as |
|---|---|---|
| under 5 min | `CURRENT` | The node's position |
| 5–15 min | `STALE` | Drawn hollow, marked ageing |
| over 15 min | `EXPIRED` | Kept as a last known position; not drawn as current |

A node can be connected and unable to obtain a position. `ONLINE` and "we know
where it is" are separate questions, and an expired position is kept rather than
deleted because "we last saw it here, eighteen minutes ago" is useful and
silence is not.

#### Nothing is inferred

A position comes from the node's own `LocationProvider` or not at all. IP
addresses, transport peer IDs, mDNS records and network topology describe where
a packet came from, not where a device is. The map still refuses to place a peer
at the coordinates of an incident it authored.

Direct peers only. There is no relay and no rebroadcast, so there is no storm to
cause. The message is shaped so relaying could be added later without changing
what it means.

---

## 6d. Operational knowledge (IMPLEMENTED)

```text
              LOCAL KNOWLEDGE
                     │
        ┌────────────┴────────────┐
        │                         │
  Operational docs          Live incidents
  (provisioned pack)     (created / replicated)
        │                         │
        └────────────┬────────────┘
                     ▼
              BGE-small-en-v1.5
                     ▼
              one vector table
                     ▼
            retrieval (cosine, 0.35)
                     ▼
                   Qwen
                     ▼
             grounded answer
```

### 6d.1 The problem

The index held only incidents, so a node that had just been provisioned could
answer nothing. "What should I do in heavy rain?" returned the refusal —
correctly, since nothing local supported an answer, and uselessly. An assistant
that can only recite what has already been reported is a search box.

The fix is **a larger corpus, not a looser pipeline.** BGE, the 0.35 threshold,
top-k, the constrained schema and the refusal are all unchanged.

### 6d.2 Two kinds of knowledge, one index

| | Operational knowledge | Live incident |
|---|---|---|
| What it is | Stable field guidance | One dynamic, unverified report |
| Where it comes from | Provisioned pack, compiled into the binary | Created locally or replicated from a peer |
| Authoritative? | Reference material | **Yes** — it is the operational record |
| Replicated? | No | Yes, as a signed event |

They stay separate as data. Incidents remain the authoritative, replicated,
signed record; operational documents are reference material that is never
replicated and can be deleted and reinstalled without touching anything
operational.

They share one vector table, because a question has to be able to rank a
procedure against a field report. `PassageSource` records which a passage is, and
every citation carries it through to the UI — an answer drawing on both must let
a reader see which half came from where.

`IP`-style provenance collapsing does not happen here: an operator's own import
is `IMPORTED_DOCUMENT`, never promoted to `OPERATIONAL_KNOWLEDGE`. This project
does not get to turn arbitrary text into doctrine by ingesting it.

### 6d.3 Provisioning

The eleven documents are compiled in with `include_str!`. There is no download,
no runtime filesystem lookup and no path to populate — the pack is present
exactly when the binary is, which is the only honest way to promise it works
offline.

Installation is **explicit**. Nothing is provisioned at startup, so "where did
this text come from?" is always answerable: an operator installed it, from the
binary. It is audited as `knowledge.installed`.

Installation is **idempotent**. Each document is keyed by a SHA-256 of its
normalised text, so a second install adds no documents, no chunks and no
vectors. Re-running it is how an operator confirms the pack is present, which
means it has to be safe to run.

Embedding is *not* part of installation. New chunks are handed to the same
background indexer incidents use, so a slow or failing model cannot leave an
install half-applied.

### 6d.4 What the documents are

**Demonstration content written for this project.** Not sourced from NDMA, NDRF,
FEMA, the IFRC or any other authority, and not agency doctrine. Every document
carries that label *in its own text*, so a passage quoted back to an operator
after chunking still says what it is — a label living only in a database column
would be stripped by the very pipeline that quotes it. A test asserts the label
is present and that no document names an authority it does not have.

On a real device this is the slot a genuine licensed procedure set drops into at
provisioning time. The mechanism is the deliverable; the content is a stand-in
that makes the mechanism demonstrable.

### 6d.5 Measured

On this machine, with the pack installed and two incidents recorded:

| | |
|---|---|
| Documents | 11 |
| Chunks | 33 |
| Vectors | 33 + one per incident |
| Install | 13 ms |
| Embed 33 chunks | 2 320 ms |
| Retrieval | 4–11 ms |
| Generation | 4.1–8.7 s |

Retrieval scores separate cleanly: genuine questions return passages at
**0.70–0.85**, while a question local knowledge cannot support returns a flat
band at **0.38–0.40**. That band is above the 0.35 threshold, which is why the
answer-support check in §6.6 exists.

---

## 6e. Offline tactical map (IMPLEMENTED)

```text
   LocationProvider ──┐
   incidents (SQLite) ─┼──▶ IPC ──▶ React map ──▶ SVG renderer
   peers (mesh)  ─────┘                              │
                                                     ▼
                                          map/basemap.geojson
                                            (local file, optional)
```

**The map is a visualization layer and never a source of truth.** It reads
existing state through existing commands and writes nothing. A test asserts the
map module names no database type at all, so there is no path from it to a
write.

### 6e.1 Why a hand-written SVG renderer

The obvious choice was MapLibre GL JS. The application's Content-Security-Policy
ruled it out:

```
default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline';
img-src 'self' data:; font-src 'self'; object-src 'none'; ...
```

There is no `worker-src` directive, so it falls back to `default-src 'self'`.
MapLibre creates its worker threads from `blob:` URLs, which that blocks
outright. Running it would have required adding `worker-src blob:`,
`connect-src` for tile reads and `img-src blob:` — weakening a real security
boundary to gain a basemap. Three further facts pointed the same way: it is
roughly 900 KB against a 230 KB bundle, it needs a style, glyphs, sprites and
tile data that must all be packaged, and this project may not download any of
them.

The renderer is therefore SecureMesh's own: Web Mercator projection, pointer
pan and zoom, SVG markers. It cost **11 KB** and required no CSP change, no
capability change and no new dependency. `capabilities/default.json` remains
`core:default`.

The offline guarantee is consequently *structural*. There is no HTTP client, no
tile URL, no style URL, no glyph or sprite host and no geocoder anywhere on the
map path — not disabled, absent. Tests scan both the Rust module and the
frontend layer for every online provider and every transport primitive.

### 6e.2 What is drawn, and what is refused

| | |
|---|---|
| Incident with coordinates | Marker, coloured by the **existing** severity tokens |
| Incident without coordinates | **No marker.** It belongs in the table, not at a guessed point |
| Recorded accuracy | Dashed circle at the true ground radius |
| No recorded accuracy | **No circle.** A default radius would invent precision |
| This node | Green marker, **only after** the operator asks for a position |
| Peer node | **Nothing** — see below |

The accuracy circle is sized through `metresPerPixel(latitude, zoom)`, which is
latitude-dependent because Mercator stretches away from the equator. A circle
scaled by a fixed pixel factor would misstate a fix by a factor of two at
temperate latitudes. It is drawn unfilled and dashed on purpose: a solid disc
reads as "the incident covers this area", which is not what accuracy means.

### 6e.3 Peers are absent, deliberately

`domain::peer::Peer` carries identity, trust, reachability and replication
state. It has no coordinates, and nothing in the schema stores a peer position —
so SecureMesh does not know where any peer is.

The tempting shortcut is to place a peer at the coordinates of an incident it
authored. That is wrong: it is where the peer was when it filed a report, which
may be hours old and kilometres away, and drawing it as the peer's position
would state something the system does not know. The incident is shown there
because the incident is what happened there.

The marker layer and its type exist and are exercised by a test that asserts the
result is empty. The day a peer position becomes authoritative, only the source
of that array changes. The legend shows the peer entry dimmed and labelled
"no position held", so the absence is visible rather than looking like a bug.

### 6e.4 Basemap provisioning

Nothing is ever downloaded **by the application**. Obtaining map data is a
separate, deliberate act: `scripts/extract-osm-basemap.mjs` queries
OpenStreetMap once, by hand, and writes `map/basemap.geojson`. The shipped
binary has no HTTP client on the map path and could not fetch anything if it
tried.

The demonstration node carries a real 3 302-feature extract of northern
Bengaluru — 2 175 classified roads, 869 water bodies, 167 railway ways and 88
named places over a 20 km box, 1.25 MB, © OpenStreetMap contributors under ODbL.
The full dataset record, including checksum and coverage, is in
`docs/map/PROVISIONING.md`.

**The region is derived from real records.** `npm run map:provision --
<data-dir>` reads the node's own incidents, takes their bounding box and adds a
10 km margin, then checks the installed basemap actually contains it. No
demonstration coordinate is written into the application; the region follows
wherever the deployment is. A basemap for the wrong region would otherwise
render happily and look correct.

Until an operator installs a basemap the map draws a graticule, a scale bar and
every real marker, and the status row reads **Not provisioned** — never "Ready".

Features are drawn by a `kind` the extraction assigns, so OSM tag vocabulary
stays in provisioning and the renderer stays readable: water and boundaries
beneath, then rail, then roads by class, with markers above all of it. Place
names are capped at fourteen and ranked by significance — a 20 km box holds
hundreds of named hamlets, and drawing them all is less legible than drawing
none. Names come from the data or not at all.

The layout deliberately mirrors the AI models, so "an asset an operator
installs" has one shape in this project. `npm run map:provision` validates
through `map::load`, the same function the running node uses, so the check and
the application cannot disagree. It reports the file, size, feature count,
SHA-256 and geographic coverage, and **fails loudly rather than fetching**
anything.

A file that is not JSON, is not a `FeatureCollection`, or holds no drawable
coordinates is refused with a stated reason. An empty map and a provisioning
mistake look identical on screen, and only one of them is acceptable.

### 6e.5 Performance

The dashboard re-reads every two seconds and returns a fresh array each time, so
array identity says nothing about whether the map changed. Markers are memoized
on a **content signature** covering only what is drawn — id, position, severity,
accuracy, sync status — so a poll that changed nothing costs nothing, and
editing an incident's description does not redraw the map.

The basemap is projected into normalized world coordinates **once**. Panning and
zooming then move a single SVG transform rather than reprojecting every path,
which is what keeps a large basemap responsive.

### 6e.6 Boundaries

- **No continuous tracking.** No `watchPosition`, no movement history. Position
  is a snapshot taken when the operator presses "My location", through the
  existing `LocationProvider` — React never touches a platform location API.
- **No geocoding**, forward or reverse. Coordinates are shown as numbers.
- **No routing and no imagery.**
- **One selection.** A marker click sets the dashboard's existing `selected`
  incident, which is the same state the incident table uses. There is no second
  selection and no duplicate details system.

---

## 6c. Device location (IMPLEMENTED)

```text
   operator presses "Use current location"
        │
        ▼
   LocationProvider (trait)
        ├── WindowsLocationProvider   Windows.Devices.Geolocation
        ├── UnavailableProvider       honest "this machine cannot"
        └── NmeaSerialProvider        USB/UART GNSS — Phase 6, not built
        │
        ▼
   latitude / longitude / accuracy / source / captured   shown for review
        │  operator confirms
        ▼
   NewIncident.validate ──▶ signed event ──▶ QUIC ──▶ peers
```

**Location is optional incident metadata, captured as a snapshot.** It is not a
new subsystem: `incidents` and `IncidentCreatedPayload` already carried
`latitude`/`longitude` as paired, range-validated options, so coordinates
already replicated. What was missing was a way to *obtain* them from the device.

### 6c.1 Provenance travels with the coordinates

A coordinate on its own is not actionable. `13.133599, 77.565330` is the same
two numbers whether it came off a satellite and is good to five metres or came
from an IP lookup and is good to fifty kilometres. Until migration 005 the
receiving node could not tell: accuracy and source existed only in the capture
panel of the machine that took the reading, and were discarded on submit.

Three fields now sit on the incident and inside `IncidentCreatedPayload`:

| Field                  | Meaning                                              |
| ---------------------- | ---------------------------------------------------- |
| `accuracyMeters`       | Reported radius. `null` = no figure, never `0`.      |
| `locationSource`       | `GNSS` / `WIRELESS` / `UNKNOWN`                      |
| `locationCapturedAt`   | When the position was **measured**                   |

They are in the payload, not beside it, so the existing event signature covers
them: a peer cannot alter how trustworthy a position claims to be without
breaking the event. There is **no second location event, no separate location
message, and no new endpoint.**

`locationCapturedAt` is deliberately separate from `createdAt`. A stale fix
attached to a fresh incident is a real failure mode, and two timestamps are what
let a reader notice it.

**The record's vocabulary is coarser than the platform's.** The device layer
distinguishes `SATELLITE` / `WIRELESS` / `IP_ADDRESS` / `UNKNOWN`; the record
keeps only the distinction that changes a decision. `IP_ADDRESS` collapses to
`UNKNOWN` rather than to `WIRELESS` — an IP lookup resolves to a city, and
bucketing it with a Wi-Fi fix two orders of magnitude better would be the
overclaim this design exists to prevent. The accuracy radius travels with it, so
such a reading is stored as unattested provenance carrying a figure in the tens
of kilometres. The mapping lives in one place in Rust (`From` for the typed
path, serde aliases for the JSON path) and a test asserts the two agree on every
variant.

### 6c.2 Migration 005 fabricates nothing

Existing incidents get `accuracy_meters = NULL`, `location_source = NULL`
(reading as `UNKNOWN`), and `location_captured_at = NULL`. `created_at` is
**not** copied into `location_captured_at`: filing time is not measurement time,
and inventing one would put a fabricated figure inside a signed record. An old
incident reading "accuracy unknown" is reporting the truth — nobody recorded it.

Events written before these fields existed still verify: their stored bytes are
untouched, and the new fields default on deserialisation.

### 6c.3 The same gate applies to peers

A valid signature proves *who* wrote a payload, not that the payload is sane.
The apply path rebuilds every incoming position through `Location::new` — the
same constructor the local command path uses — so a remote node cannot store a
negative, non-finite or absurd accuracy that a local operator would have been
refused. Provenance with no coordinates to describe is rejected outright.

### Why the platform API rather than the Tauri plugin

`tauri-plugin-geolocation` marks Linux, Windows and macOS **unsupported** and
ships no desktop implementation — its own install instructions target only
`android` and `ios`. On desktop it would do nothing. The Windows location
service is therefore called directly through the `windows` crate, which Tauri
already pulls in on this target, so this adds a feature rather than a supply
chain.

### The honesty requirement

A desktop rarely has a GNSS receiver. Windows still answers, by triangulating
Wi-Fi or by looking up the IP address — a guess accurate to a city, and one that
requires the OS to reach Microsoft. Reporting that as "GPS" would mislead
someone deciding whether to walk to a coordinate.

So a fix carries its `LocationSource` and accuracy exactly as the platform gave
them, the UI shows both, and both are now kept on the record. Measured on the
development machine: a real fix in 2212 ms, **source `Wireless`, accuracy
±165 m** — correctly *not* labelled satellite, and correctly reported as not
offline-capable.

**Windows Wireless positioning is not an offline capability.** It requires the
operating system to reach a lookup service. Only `GNSS` returns `true` from
`works_offline()`, and that rule is stated once, in the core, so the UI cannot
get it subtly wrong somewhere else.

**Nothing is ever invented.** A provider that cannot answer returns an error;
there is no default coordinate, no last-known fallback, and no placeholder.

### Boundaries

- **Snapshot, not tracking.** `getCurrentPosition` only. No `watchPosition`,
  no background polling, no movement history. The incident keeps the position it
  was reported at.
- **A vector for location is never transmitted separately.** Coordinates and
  their provenance travel inside the signed incident event, over the authorized
  path, so a receiving node needs no location hardware to display where
  something happened.
- **Provenance describes a measurement or it is not sent.** The coordinate
  fields stay editable after a capture; if the operator changes them, the UI
  drops the accuracy and source rather than attaching a satellite-grade claim to
  hand-typed numbers, and says so.
- **Reads are not audited, and coordinates are never written to the audit log.**
  Reading a sensor is an observation, not a state change. Recording a position on
  every incident would build a track of where the operator has been, which is a
  worse disclosure than anything it would prove.
- **No geocoding and no map.** Coordinates are displayed as numbers. An offline
  map is a separate future phase.
- **GPS is never a precondition.** No receiver, refused permission or a timed-out
  fix all leave incident capture, replication and indexing untouched.

---

## 7. Phase 4 — Local RAG (DESIGN ONLY)

```
   Local documents (field manuals, protocols, maps)
        │  text extraction
        │  chunking
        ▼
   Local embedding model
        │
        ▼
   Local vector index  (FAISS, or sqlite-vec to keep everything in one file)
        │  retrieval
        ▼
   Local LLM  ──►  grounded answer with citations
```

No cloud vector database. `sqlite-vec` is the more interesting candidate for a
field node because it keeps the vector index inside the same SQLite file that
already holds operational data — one file to back up, one file to encrypt, one
file to sync.

Knowledge sources must be openly licensed and non-sensitive; see the data policy
in `README.md`.

---

## 8. Phase 5 — Confidential computing (DESIGN ONLY)

Read §7 of `docs/security/SECURITY.md` first — it defines what SecureMesh will
and will not claim. The essential distinction, repeated because it is the one
most often got wrong:

| | TPM 2.0 / secure element | TEE |
|---|---|---|
| Protects | Keys and measurements **at rest** | Code and data **while executing** |
| Mechanism | Discrete chip, non-extractable keys, PCR-sealed secrets | CPU-enforced isolated execution environment |
| Answers | "Has this platform booted the software it should have?" | "Can the OS read this process's memory?" |
| Does NOT | Run application code | Replace a hardware root of trust |

A TPM is **not** a TEE. A normal OS process is **not** a TEE. SecureMesh will
want both a hardware root of trust and an isolated execution environment; they
are complementary, not alternatives.

Planned properties, none implemented:

- **Secure boot / measured boot** — the boot chain is measured into PCRs, so the
  platform can prove what it loaded.
- **Sealed data** — the database encryption key is sealed to platform state, so
  it unseals only on an unmodified node.
- **Key protection** — the node's signing key is generated inside and never
  leaves the TPM/secure element; the core calls a signing *operation* instead of
  loading a key. The `KeyStore` trait already has the right shape for this.
- **Protected memory** — inference over sensitive data runs inside the TEE.
- **Remote attestation** — where the platform supports it, a node proves its
  integrity to a peer before being trusted with sensitive records.

**Deliberately not committed to a platform.** TEE support varies enormously
across candidate hardware, and marketing material is not a substitute for
verification. Nothing here will be claimed until it has been demonstrated on the
specific board selected in Phase 6.

---

## 9. Repository layout

```
src/                    React frontend
  components/           Presentational, reusable
  features/             Feature-scoped UI (dashboard, incidents)
  pages/                Screen composition
  lib/                  IPC client, theme, formatting
  types/                TypeScript mirrors of Rust types
  styles/               tokens.css (the single palette) + app.css

src-tauri/              Rust core
  src/commands/         Tauri IPC surface — thin
  src/runtime.rs        Node assembly and decisions
  src/identity/         Ed25519 identity + KeyStore trait
  src/domain/           Incident/node types and their invariants
  src/storage/          SQLite, migrations, repositories
  src/security/         Secret<N>, audit
  src/ai/               Local inference, embedding, RAG, rule layer, insight,
                        situation brief, and the generation-model gate
  migrations/           Versioned SQL
  tests/                Integration tests
  examples/             Standalone measurement harnesses (`run_benchmark`,
                        `nlp_evaluation`) — not part of the shipped binary

docs/architecture/      This file, ROADMAP.md
docs/security/          SECURITY.md
docs/ai/                PROVISIONING.md, EVALUATION.md
docs/demo/              DEMO.md
```

Rust integration tests live in `src-tauri/tests/` rather than a top-level
`tests/` directory, because Cargo requires them to sit beside the crate they
exercise.

Directories for `data/` and `hardware/` are intentionally **not** created yet.
They will be added when the phase that needs them begins, rather than standing
empty.
