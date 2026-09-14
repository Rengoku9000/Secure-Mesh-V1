# SecureMesh

**Confidential Edge-AI Platform for Resilient Offline Communication**

Smart India Hackathon 2026 · STH260001 (Open Innovation) · Hardware ·
Blockchain & Cybersecurity

> A self-contained, offline-first, secure edge-computing node that can
> communicate, store, synchronise and locally process information without
> depending on external cloud services.

**Current status: Phase 2.5 complete.** Nodes hold cryptographic identities,
persist an append-only signed event log, discover each other on a local network
with no server, authenticate over encrypted QUIC, and replicate incidents —
converging correctly across partitions, restarts, duplicate delivery and
out-of-order arrival. A peer must be **explicitly enrolled by an operator**
before anything is exchanged with it; authentication alone grants nothing. Local
AI and confidential computing are designed but **not implemented** — see
[Roadmap](#roadmap) and [Current limitations](#current-limitations).

---

## The problem

When infrastructure fails, the software built on top of it fails with it.

Disaster response, remote healthcare, and field operations share a
characteristic that most software gets wrong: **connectivity is the exception,
not the rule.** Cell towers are down or saturated, satellite links are
expensive and intermittent, and the moment a system needs a server to validate a
login or store a record, it stops working exactly when it is needed most.

Meanwhile the data involved is often sensitive — casualty information, location
of vulnerable people, operational capability — which rules out routing it
through third-party cloud services even when a connection exists.

## The solution

SecureMesh nodes are independent by construction. Each one:

1. Holds its own cryptographic identity ✅
2. Stores its own operational data locally ✅
3. Communicates directly with nearby nodes ✅
4. Synchronises without a coordinating server ✅ — **only with enrolled peers** ✅
5. Runs AI inference on-device *(Phase 3)*
6. Answers questions from local documents *(Phase 4)*
7. Protects keys and processing in hardware *(Phase 5)*

A node that can see no peers is in a **normal** state, not a degraded one.

The architecture is deliberately not tied to disaster response. Defence field
operations, humanitarian logistics, remote healthcare, critical infrastructure,
and remote industrial sites share the same constraint and are served by the same
design.

---

## The local-only principle

**SecureMesh contacts no external service. This is not a configuration option —
there is nothing to configure.**

Not used, at any phase: OpenAI · Anthropic · Gemini · Groq · OpenRouter ·
Hugging Face inference · Firebase · Supabase · hosted vector databases · cloud
databases · cloud authentication · mapping APIs · any paid SaaS API.

Verifiable rather than asserted:

```powershell
cd src-tauri
cargo tree | Select-String "reqwest|hyper|isahc|ureq|curl"   # no results
```

There is no HTTP client in the dependency tree and no external endpoint in the
codebase.

**Precision matters here.** Phase 2 *does* open a local UDP socket for QUIC and
*does* broadcast mDNS on the local link — peer-to-peer networking cannot work
otherwise. Local sockets and P2P protocols were always permitted; the constraint
is on **external and cloud dependencies**, not on networking as such. A
SecureMesh node still talks to nobody but its neighbours, and needs no server,
no DHT bootstrap, and no Internet.

---

## Architecture

```
                        SECUREMESH NODE
                              │
              ┌───────────────┴───────────────┐
              │                               │
          UI LAYER                       CORE LAYER
    React 19 + TypeScript                 Rust core
              │                               │
              └────── Tauri 2 IPC ────────────┤
                                              │
     ┌────────────┬────────────┬────────────┬────────────┐
     │            │            │            │            │
  Identity     Storage     Networking     Sync       Security
  Ed25519      SQLite      MeshTransport  event log  Secret/audit
  KeyStore     event log   libp2p/QUIC    watermarks
     │                     mDNS
     │
  ┌──┴─────────────────────────────────┐
  │  PLANNED:  AI → RAG → TEE          │
  └────────────────────────────────────┘
```

Replication is an **append-only log of signed events**, ordered by per-origin
sequence numbers rather than wall-clock time. Incidents are a projection of that
log, so local creation and remote replication travel the same code path.

No mandatory central server. No `React → API → cloud` dependency. Full detail in
[`docs/architecture/ARCHITECTURE.md`](docs/architecture/ARCHITECTURE.md).

---

## Technology stack

| Layer | Choice | Version |
|---|---|---|
| Frontend | React + TypeScript + Vite | 19 · 5.8 · 7 |
| Desktop shell | Tauri | 2 |
| Core | Rust | 1.97 (MSVC on Windows) |
| Database | SQLite via `rusqlite` (bundled) | 0.40 |
| P2P | `libp2p` — QUIC, mDNS, request-response, identify | 0.56 |
| Signatures | `ed25519-dalek` | 3.0 |
| Randomness | `getrandom` (OS CSPRNG) | 0.4 |
| Hashing | `sha2` | 0.11 |
| Key erasure | `zeroize` | 1.9 |

libp2p is deliberately minimal: QUIC only, with no TCP/Noise/Yamux fallback and
no DHT. Planned: a local inference runtime (Phase 3), a local vector index
(Phase 4).

---

## Development setup

### Prerequisites

| Requirement | Notes |
|---|---|
| Node.js 20+ | LTS recommended |
| Rust (stable) | Install via [rustup](https://rustup.rs) |
| **Windows:** VS 2022 Build Tools with the C++ workload + Windows SDK | Rust needs the MSVC linker |
| **Windows:** WebView2 Runtime | Pre-installed on Windows 11 |
| **Linux:** `webkit2gtk-4.1`, `libayatana-appindicator3-dev`, `librsvg2-dev` | See [Tauri prerequisites](https://tauri.app/start/prerequisites/) |
| **macOS:** Xcode Command Line Tools | |

### Run

```bash
npm install
npm run tauri dev
```

The first build compiles the Rust core and takes several minutes.

### Test

```bash
cd src-tauri
cargo test            # 701 tests: 493 unit + 208 integration
cargo clippy --all-targets --all-features -- -D warnings
```

The distributed-systems behaviour (partition, restart, duplicate delivery,
reordering, equivocation, multi-hop relay) is tested in `tests/mesh_sync.rs`
over a deterministic in-process transport, so those cases are asserted rather
than raced. `tests/mesh_libp2p.rs` exercises the same code over real QUIC.

### Run two nodes on one machine

Each node *is* its data directory — identity plus log — so a second node needs
its own:

```powershell
npm run app:stage    # builds a standalone binary into dist-app/

$env:SECUREMESH_DATA_DIR="$env:TEMP\smA"; Start-Process .\dist-app\securemesh.exe
$env:SECUREMESH_DATA_DIR="$env:TEMP\smB"; Start-Process .\dist-app\securemesh.exe
```

They discover each other automatically — and then **nothing happens**, which is
correct. Each window shows the other as `PENDING` in the Peers panel. Click
**Approve** on both, and only then does an incident created in one appear in the
other.

> **Do not launch `src-tauri/target/debug/securemesh.exe` directly.** That path
> is rewritten by every `cargo build`, `cargo test` and `cargo clippy` into a
> *development* binary, which loads the UI from the Vite dev server instead of
> from embedded assets. Run it without `npm run tauri dev` and the window shows
> `ERR_CONNECTION_REFUSED`.
>
> `npm run app:stage` produces a production binary in `dist-app/`, where `cargo`
> never writes, and fails the build if the frontend is not actually embedded.
> The app also states its build mode on startup.

```bash
npx tsc --noEmit      # frontend type check
```

### Build a release binary

```bash
npm run tauri build
```

### Where a node keeps its data

| OS | Path |
|---|---|
| Windows | `%APPDATA%\org.securemesh.node\` |
| Linux | `~/.local/share/org.securemesh.node/` |
| macOS | `~/Library/Application Support/org.securemesh.node/` |

`node_identity.json` (the keypair) and `securemesh.sqlite` (the records).
Deleting the directory resets the node to a first launch.

---

## What it actually does

**Phase 1 — the node**

- Generates an Ed25519 keypair on first launch, reused thereafter
- Derives the node ID as `SHA-256(public key)` and the display name from it, so
  identity is derived from key material rather than assigned
- Keeps the private key out of the UI, IPC, logs, and `Debug` output
- Persists to a real SQLite file with versioned migrations, foreign keys, and
  `CHECK` constraints
- Validates all input in Rust, never in the frontend
- Light and dark themes from one token set

**Phase 3 — local intelligence**

- **Local inference only.** No API, no key, no endpoint, and nothing downloaded
  — models are files an operator provisions. Verified structurally: there is no
  HTTP client in the dependency tree, and the runtime client takes a *port*, not
  a host
- Incident classification and structured extraction via schema-constrained
  decoding, so output always parses — then it is parsed, validated and
  normalised anyway, because a model is untrusted input
- Local embeddings and vector retrieval over both knowledge documents and
  incidents
- **An operational knowledge pack ships inside the binary**, so a node answers
  field questions before any incident has been recorded. Eleven emergency-
  response documents, installed on an explicit operator action — never at
  startup, never over the network — and idempotent by content hash. Clearly
  labelled demonstration content, not agency doctrine
- **Two kinds of knowledge, one index.** Standing guidance and live incidents
  are ranked against each other, so one answer can cite a procedure *and* a
  field report — and every citation says which it is, because the two carry very
  different weight
- Grounded question answering where **citation is a required schema field, not
  a request** — the model cannot answer without naming the passages it used.
  Those numbers are verified against what was actually supplied, invented ones
  are discarded and counted, and a question retrieval cannot serve is refused
  without calling the model at all. Measured: asking in prose grounded 0 of 19
  answers; requiring it in the schema grounded 19 of 19
- **An answer is checked against the passages it was given.** Citation alone
  stopped being enough as the corpus grew: with eleven documents indexed, "What
  is the capital of France?" retrieved five emergency passages just over the
  relevance threshold, and the model cited all five and answered "Paris". The
  answer's words are now compared with the retrieved text, and an answer the
  records do not contain is refused rather than shown. Measured: genuine answers
  score 1.00, "Paris" scores 0.00
- **AI is a layer, never a dependency.** A node with no model keeps capturing,
  replicating and serving; it reports intelligence as unavailable
- The model never overwrites an operator's judgement — its severity is stored
  beside theirs, and disagreement is surfaced
- **A deterministic rule layer reads every report in microseconds, with no
  model loaded.** Hazard cues, people counts and status, locations, routes,
  and an explainable severity score with the factors behind it — all derived,
  advisory, and never written over the report. Measured on the held-out set:
  **69.4%** category accuracy from rules alone, **83.3%** combined with the
  semantic fallback below. See [`docs/ai/EVALUATION.md`](docs/ai/EVALUATION.md)
- **Related and duplicate reports surface automatically**, by embedding
  similarity when a model is provisioned and by word/hazard overlap when it is
  not — so the same behaviour degrades rather than disappears. **The
  similarity thresholds are not yet calibrated against measured data**: on the
  hand-labelled pairs, most genuine duplicates score in the `RELATED` band, not
  `DUPLICATE`. Recorded as an open item, not silently shipped — see
  [`docs/ai/EVALUATION.md`](docs/ai/EVALUATION.md)
- **A situation brief aggregates every incident this node holds** — counts by
  category and severity, people totals (summed, with duplicates called out
  separately), blocked routes, and a priority ranking — computed with no
  model, and available over IPC (`get_situation_brief`). A prose summary can
  be requested from the model on top of these figures; it is withheld, not
  shown, if it is not supported by the figures it was given
- **A bounded queue, not an open door, in front of the generation model.** One
  request runs, a few wait briefly, the rest are refused immediately with a
  reason — because the model is one CPU-bound process and a queue backing up
  would only delay every request, never speed one up. Incident capture and
  sync never touch this gate
- **Every command that can reach a model runs off the Tauri main thread**, so
  a multi-second analysis or answer does not freeze the window

**Device location**

- **Local only.** The position comes from the operating system on this device —
  no geocoding service, no map tiles, no API key, no network call by SecureMesh
- Captured **on request**, reviewed, then committed with the incident. Opening
  the form neither prompts nor reads the sensor
- **Optional and never blocking**: no receiver, refused permission or a failed
  fix all leave incident capture, replication and AI indexing untouched
- A **snapshot**, not tracking — the incident keeps the position it was reported
  at, and there is no `watchPosition` and no movement history
- Coordinates travel inside the signed incident event, so a receiving node
  displays them **without needing location hardware of its own**
- **Source and accuracy are shown, never assumed.** A desktop without GNSS gets a
  Wi-Fi or IP estimate; measured here as `Wireless` at ±165 m, and labelled as
  such rather than as GPS
- **Provenance is part of the record, not just the capture panel.** Accuracy,
  source (`GNSS` / `WIRELESS` / `UNKNOWN`) and the time the position was
  *measured* sit inside the signed payload, so a receiving node can tell a ±5 m
  fix from a ±50 km one instead of seeing two identical-looking numbers
- **Only `GNSS` is offline.** Windows Wireless positioning needs the operating
  system to reach a lookup service; SecureMesh makes no such call, but that does
  not make the position offline-derived, and it is not described as one
- **Nothing is backfilled.** Incidents recorded before provenance existed read as
  unknown, because that is what they are — no invented accuracy, and no capture
  time borrowed from when the record happened to be filed
- There is no official Tauri geolocation plugin for desktop — it marks Windows,
  Linux and macOS unsupported — so the platform API is called directly behind a
  `LocationProvider` trait that a USB/UART GNSS module can later replace

**Offline tactical map**

- **Nothing is requested.** No tile server, style server, glyph host, sprite
  host or geocoder — there is no HTTP client anywhere on the map path, so no
  request can be made regardless of configuration
- **The renderer was chosen by the security policy, not by taste.** MapLibre GL
  JS spawns workers from `blob:` URLs, which this application's
  `default-src 'self'` CSP blocks. Rather than weaken the policy for a basemap,
  the map is drawn with SecureMesh's own Web Mercator SVG renderer — **11 KB**,
  no new dependency, and no change to the CSP or Tauri capabilities
- **Real geography, provisioned locally.** The demonstration node ships a
  3 302-feature OpenStreetMap extract — roads by class, water, railways and
  named places over a 20 km box — read from `map/basemap.geojson` at startup.
  Extraction is a separate operator-run script; the application has no HTTP
  client on the map path and downloads nothing, ever.
  **© OpenStreetMap contributors, ODbL 1.0** — attribution is written inside the
  data file so it cannot be separated from it
- **The region is derived, not chosen.** `npm run map:provision` reads the
  node's own incidents from SQLite and computes the box they need plus a 10 km
  margin. No demonstration coordinate appears anywhere in the application
- **The grid fallback stays.** With no basemap the status row reads
  *Not provisioned* — it never claims Ready without data — and the map still
  draws a coordinate graticule, a scale bar and every real marker
- **Accuracy is drawn honestly.** The uncertainty circle is sized in true metres
  for the latitude and zoom, and an incident that recorded no accuracy gets **no
  circle at all** rather than an invented radius
- **Peers are not placed.** SecureMesh holds no authoritative peer position, so
  none is drawn — putting a peer at an incident's coordinates would state where
  it *was*, not where it is. The legend says so rather than leaving a gap
- **A snapshot, not a track.** Position comes from the existing
  `LocationProvider` when the operator asks; there is no `watchPosition` and no
  movement history

**Node location heartbeat**

- Every node publishes its own position to **authorized peers only**, every five
  minutes, with the first publication as soon as a position is available rather
  than after the first interval
- **A peer cannot speak for another node.** The message body carries no node
  identifier at all — attribution comes from the authenticated session, so the
  spoofing question is removed rather than checked
- **Ordered by sequence, not by clock.** Each node keeps a monotonic counter and
  a receiver refuses anything not strictly newer. Two nodes do not share a
  clock, and comparing timestamps would let a fast one overwrite fresher data
- **Ephemeral.** Held in memory, one entry per peer, always the latest. Never
  written to the event log, never stored as an incident, never replicated
  onward, and gone on restart — because it describes *now*
- **Freshness is not reachability.** Current under 5 minutes, stale to 15,
  expired beyond. A node can be connected and unable to obtain a position, so an
  expired position is kept as a *last known* position rather than deleted, and
  never drawn as current
- **Nothing is inferred.** A position comes from the node's own
  `LocationProvider` or not at all. IP addresses, transport peer IDs and mDNS
  records say where a packet came from, not where a device is
- **Audited on transition only** — `peer.location_available` and
  `peer.location_expired`. A heartbeat every five minutes is an observation, and
  logging each would bury every real security event
- **This shares personal data**: a node's position is where the person carrying
  it is. It goes to authorized peers over the local mesh and nowhere else

**Phase 2.6 — deterministic synchronisation**

- Replication is **caused**, not waited for: every legitimate cause —
  connection, authorization, a local write, a peer announcing it is ahead, a
  relay learning something new — fires its own trigger, and all of them funnel
  into one code path
- An explicit per-peer lifecycle (`Connected → Authenticated →
  AwaitingAuthorization → Syncing → Synced`) so "idle because up to date" is
  distinguishable from "idle because nothing authorized it"
- Independent of **who dialled, who approved first, and who holds the data**
- The periodic sweep is a 60-second safety net, not the mechanism; the
  determinism tests never run it at all
- Structured `sync.*` logging with per-round latency

Measured on one machine over real QUIC: **authorization → sync round start in
under 1 ms**, approval → the record landing on the peer in **340 ms**, of which
the productive exchange was **240 ms**.

**Phase 2.5 — the trust layer**

- Distinguishes **authentication** ("are you who you claim?") from
  **authorization** ("are you allowed here?") — a peer that completes the
  handshake is `PENDING`, not trusted
- Persists a per-peer trust state (`UNKNOWN` → `PENDING` → `TRUSTED` →
  `REVOKED`) in SQLite, keyed by the public key fingerprint
- Enforces it in the Rust core on every message, not in the UI — a frontend
  that ignored the state would still be refused
- **No protocol message can grant authorization.** Decisions come only from
  local operator commands, so an enrolled peer cannot promote itself or anyone
  else, and ordinary nodes hold no enroll/revoke capability at all
- Revocation is durable and takes effect on the next message, not the next
  reconnection; a revoked peer stays revoked across reconnects and restarts
- Records every decision in an append-only, signed audit log ordered by a local
  monotonic sequence

**Phase 2 — the mesh**

- Discovers peers on the local network by mDNS, with no server and no bootstrap
- Authenticates peers by the QUIC handshake, reusing the node's own Ed25519 key,
  so no custom cryptography was written
- Replicates an append-only log of author-signed events, each verifiable
  standalone — so a relayed event does not require trusting the relay
- Orders by per-origin sequence numbers, **never by wall-clock time**
- Converges by union across partitions: concurrent creation produces no
  conflicts by construction
- Detects equivocation (one node signing two different events at one sequence
  number), keeps both versions, and stops advancing replication from that node
- Survives restart, duplicate delivery, out-of-order arrival, and interrupted
  sync rounds — all sync state is durable, none is held in memory
- Reports peers, connection state, last-seen, and per-peer backlog in the UI

## Current limitations

Stated plainly, because a reader needs them to judge what this is fit for.

| Limitation | Detail |
|---|---|
| **Revocation does not propagate** | A trust decision is local. Revoking a peer on one node does not revoke it elsewhere, and a partitioned node cannot learn of a revocation until something reaches it. Inherent to offline-first; the most significant Phase 2.5 weakness. `SECURITY.md` §6.13 |
| **Not a PKI** | No certificate authority, no chain, no delegation. Trust is local policy, in the manner of SSH `authorized_keys` |
| **A compromised administrator key is not solved** | No key rotation, no administrator revocation, no recovery path |
| **Enrolment does not verify intent** | Approving a node ID authorizes exactly that keypair. Confirming it is the device you meant requires out-of-band checking the software does not provide. §6.15 |
| **Administrative authority is policy, not cryptography** | A node can be provisioned as non-admin, but someone with filesystem access can edit that back. §6.14 |
| **The private key is stored unencrypted at rest** | Protected by OS file permissions only. §6.1 |
| **Not end-to-end encrypted** | QUIC protects each hop. A relay node reads the plaintext it forwards — it cannot forge or alter it, but it can read it. §6.7 |
| Incident data is not encrypted at rest | Standard SQLite file. §6.2 |
| Metadata is exposed | mDNS advertises this node's presence, ID and public key on the local link. §6.8 |
| The transport key must be extractable | libp2p needs the private key in process memory, which conflicts with the non-extractable hardware storage planned for Phase 5. §6.9 |
| No mutable incident editing | Phase 2 is append-only by design; updates are observations. This is what makes conflict-free merging possible |
| No AI, no RAG | No model ships with the build. Phases 3–4 |
| **No TEE** | This is an ordinary OS process. A normal process is not a TEE, and a TPM is not a TEE. Phase 5 |
| No operator authentication | Anyone who can open the app is the operator. §6.3 |
| Audit log is not durable | Written to stderr; useful for diagnostics, not evidence. §6.4 |
| No rate limiting | Message sizes are bounded, but there is no per-peer quota. §6.10 |
| Not security reviewed | No penetration test, no independent audit |

Full analysis: [`docs/security/SECURITY.md`](docs/security/SECURITY.md).

---

## Roadmap

| Phase | Scope | Status |
|---|---|---|
| **1** | Node foundation: identity, storage, incidents, dashboard | ✅ **Complete** |
| **2** | P2P mesh: libp2p, QUIC, authenticated peers, signed events, offline sync | ✅ **Complete** |
| **2.5** | Peer trust: enrollment, authorization, capabilities, revocation, audit | ✅ **Complete** |
| **2.6** | Deterministic synchronisation: explicit triggers, link lifecycle | ✅ **Complete** |
| **2.75** | Mesh hardening: replicated revocation, out-of-band verification, quotas | 📋 Next |
| **3** | Local AI and RAG: inference, embeddings, retrieval, grounded answers | ✅ **Complete** |
| **5** | Confidential computing: TPM-backed keys, encrypted storage, TEE | 🔍 Research |
| **6** | Physical node: edge compute, secure element, LoRa, GNSS, battery | 🔍 Research |

Phase 3 absorbed what was planned as Phase 4 (local RAG): both need the same
runtime, provisioning story and trust boundary, and splitting them would have
meant building all three twice.

Details and per-phase exit criteria:
[`docs/architecture/ROADMAP.md`](docs/architecture/ROADMAP.md).

### On blockchain

The theme is "Blockchain & Cybersecurity"; SecureMesh fits through
cybersecurity. A distributed ledger is **not** part of the MVP: consensus trades
away availability under partition, and availability under partition is the whole
requirement. It will be added only if a specific architectural problem genuinely
calls for it.

---

## Data policy

- All demo data is **synthetic** and labelled as such.
- No private, classified, sensitive, or real operational data is collected or
  committed.
- Knowledge sources for Phase 4 must be openly licensed and non-sensitive.
- This is a prototype. It must not be used to handle real operational data.

---

## Documentation

| Document | Contents |
|---|---|
| [`docs/architecture/ARCHITECTURE.md`](docs/architecture/ARCHITECTURE.md) | Layers, design decisions and trade-offs, data model, Phase 2–5 design |
| [`docs/security/SECURITY.md`](docs/security/SECURITY.md) | Threat assumptions, trust boundaries, implemented controls, known limitations |
| [`docs/architecture/ROADMAP.md`](docs/architecture/ROADMAP.md) | Phased plan with exit criteria and open research questions |
| [`docs/demo/DEMO.md`](docs/demo/DEMO.md) | Five-minute demo script |
| [`docs/ai/PROVISIONING.md`](docs/ai/PROVISIONING.md) | Installing the local model and runtime by hand — nothing is downloaded |
| [`docs/ai/EVALUATION.md`](docs/ai/EVALUATION.md) | Measured accuracy, latency and grounding, with method and limits |
| [`ai/models/README.md`](ai/models/README.md) | Model provenance: versions, licences, checksums, hardware |
| [`docs/hardware/PLATFORM_EVALUATION.md`](docs/hardware/PLATFORM_EVALUATION.md) | Phase 4A study: edge platforms, TEE feasibility, what hardware can and cannot protect |

---

## Licence

Not yet chosen.
