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
able to rank a procedure and a field report against each other.

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

This verifies citation, not meaning — a model can cite a real passage and still
say something it does not support (`SECURITY.md` §6.19).

### 6.7 AI is a layer, never a dependency

The service is an `Option` on the runtime, and analysis is never on the path of
incident creation or synchronisation. A node with no model, or a crashed
runtime, keeps capturing and replicating incidents and reports intelligence as
unavailable. `tests/ai_boundary.rs` asserts this.

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
  migrations/           Versioned SQL
  tests/                Integration tests

docs/architecture/      This file, ROADMAP.md
docs/security/          SECURITY.md
docs/demo/              DEMO.md
```

Rust integration tests live in `src-tauri/tests/` rather than a top-level
`tests/` directory, because Cargo requires them to sit beside the crate they
exercise.

Directories for `ai/`, `data/`, and `hardware/` are intentionally **not** created
yet. They will be added when the phase that needs them begins, rather than
standing empty.
