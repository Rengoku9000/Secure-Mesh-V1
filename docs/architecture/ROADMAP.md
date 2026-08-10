# SecureMesh — Roadmap

Ordered by dependency, not by ambition. Each phase exists because the one after
it cannot be built honestly without it.

**Legend:** ✅ done · 🔨 in progress · 📋 designed, not built · 🔍 needs research

---

## Phase 1 — Node foundation ✅ COMPLETE

A single node that has an identity, keeps data, and survives a restart.

| Item | Status |
|---|---|
| Tauri 2 + React 19 + TypeScript + Vite 7 application | ✅ |
| Rust core with a layered, testable architecture | ✅ |
| Ed25519 node identity, derived node ID and name | ✅ |
| `KeyStore` trait, with a documented software backend | ✅ |
| Private key excluded from UI, IPC, logs, and `Debug` output | ✅ |
| SQLite persistence, file-backed, with versioned migrations | ✅ |
| Incident creation, validation, listing, retrieval | ✅ |
| Dashboard with node, subsystem, and network status | ✅ |
| Light and dark themes from a single token set | ✅ |
| 109 automated tests (99 unit, 10 integration) | ✅ |
| No external API, verified by dependency inspection | ✅ |

**Why it stops here:** networking without record signing would mean accepting
unauthenticated data from peers, and AI before storage would have nothing
durable to reason over.

---

## Phase 2 — Peer-to-peer mesh ✅ COMPLETE

**Goal:** two nodes exchange incidents with no Internet and no server.

| Item | Status |
|---|---|
| Append-only log of author-signed events | ✅ |
| libp2p integration, mDNS local discovery, no bootstrap server | ✅ |
| QUIC transport | ✅ (TCP fallback deliberately omitted) |
| Peer identity bound to the Ed25519 node key | ✅ — the handshake *is* the authentication |
| Signature verification on receipt, against the origin's key | ✅ — makes multi-hop safe |
| Durable store-and-forward | ✅ — the log plus per-peer ack watermarks |
| Convergent sync with no timestamp ordering | ✅ — per-origin sequence numbers |
| Equivocation detection, both versions preserved | ✅ |
| `MeshTransport` abstraction, no libp2p above it | ✅ |
| Three-node relay and partition testing | ✅ |
| Peer list, connection state, per-peer backlog in the UI | ✅ |
| Out-of-band peer enrolment | ❌ **deferred to Phase 3** |
| Durable, append-only, signed audit log | ❌ deferred to Phase 3 |

**How the risk resolved.** The identified risk was conflict resolution under
clock skew. It was removed rather than solved: making events **append-only**
turns merging into set union, which converges with no clock and no merge rule.
The hybrid logical clock was not needed and was not built. That option returns
only if mutable editing is ever introduced.

**Verified:** 218 tests, clippy clean. Partition, restart, duplicate delivery,
out-of-order arrival, mid-sync restart, equivocation, spoofed senders, tampered
events, malformed floods, and three-node relay are covered deterministically;
the real QUIC path is covered separately; two GUI instances discover and
authenticate each other on one machine.

**Not yet done:** the two-laptop test on a genuinely isolated physical network.
Everything so far is single-machine, and the loopback interface is a kinder
environment than real Wi-Fi.

---

## Phase 2.5 — Mesh hardening 📋 NEXT

Carried over from Phase 2, and required before the mesh is fit for a contested
network.

| Item | Notes |
|---|---|
| **Out-of-band peer enrolment and revocation** | 🔍 Mechanism not chosen. Authentication proves a peer holds its key; it does not prove the peer belongs. `SECURITY.md` §6.6 |
| Per-peer rate limiting and quotas | §6.10 |
| Durable, append-only, signed audit log | §6.4 |
| Two-laptop test on an isolated physical network | The one Phase 2 claim not yet demonstrated on real hardware |

## Phase 3 — Local inference 📋

**Goal:** classify, extract from, and summarise incidents entirely on-device.

| Item | Notes |
|---|---|
| Inference runtime selection | 🔍 llama.cpp vs Candle vs ONNX Runtime — depends on Phase 6 hardware |
| Open-weight quantised model, local file | Licence must permit the intended use |
| Model integrity verification before load | Hash-pinned; a model file is executable input |
| Classification, entity extraction, summarisation | Per ARCHITECTURE.md §6 |
| Graceful degradation with no model installed | Already the current behaviour |

**Constraint:** the "AI" status row must keep reporting truthfully. A node with
no model reports "not installed" and keeps working.

---

## Phase 4 — Local RAG 📋

**Goal:** answer questions from local documents with citations, offline.

| Item | Notes |
|---|---|
| Text extraction and chunking | |
| Local embedding model | |
| Local vector index | 🔍 FAISS vs `sqlite-vec` — the latter keeps one file to back up, encrypt, and sync |
| Retrieval + grounded generation with citations | |
| Openly licensed, non-sensitive corpus | Per the data policy |

---

## Phase 5 — Confidential computing 🔍

**Goal:** protect keys and sensitive processing from a compromised host.

Read `docs/security/SECURITY.md` §7 before working on this phase. A TPM is not a
TEE, and a normal OS process is not a TEE.

| Item | Notes |
|---|---|
| TPM 2.0 / secure element `KeyStore` backend | Key generated in and non-extractable from hardware; core calls a signing operation |
| Database encryption with a TPM-sealed key | Closes SECURITY.md §6.2 |
| Measured boot | Platform proves what it loaded |
| Remote attestation | **Only where the chosen platform genuinely supports it** |
| Confidential inference inside a TEE | Depends on platform capability |

**Hard rule for this phase:** no capability is claimed in documentation, a demo,
or a presentation until it has been demonstrated on the specific hardware. TEE
support differs sharply between candidate boards, and vendor marketing is not
verification.

---

## Phase 6 — Physical node 🔍

**Goal:** a self-contained field device.

Target components: edge AI compute · TPM 2.0 or secure element · local SSD ·
Wi-Fi · optional LoRa · GNSS · optional camera · battery · enclosure.

**No hardware is to be purchased or committed to yet.** Evaluate first:

| Criterion | Why it decides the outcome |
|---|---|
| AI inference performance | Determines whether Phase 3 is viable at all |
| Power draw | Sets battery size and therefore field endurance |
| Linux support | Vendor-forked kernels age badly |
| Hardware security (TPM / secure element) | Prerequisite for Phase 5 |
| **TEE availability — verified, not claimed** | The single most over-stated spec in this class of device |
| Driver availability | Especially LoRa and GNSS |
| Thermal behaviour in an enclosure | Sustained inference throttles in a sealed box |
| Cost | Multiple nodes are needed to demonstrate a mesh |
| Availability in India | A board that cannot be sourced is not a candidate |

The software architecture must not depend on any single board. Phase 5's
`KeyStore` abstraction is what keeps that true.

---

## Explicitly not planned

**Blockchain.** The SIH theme is "Blockchain & Cybersecurity", and SecureMesh
fits it through cybersecurity: cryptographic identity, authenticated peers,
confidential computing. Adding a distributed ledger would introduce consensus
overhead and, in a partitioned mesh, is the wrong tool for the problem —
availability under partition is exactly what SecureMesh requires and what
consensus sacrifices. If a specific architectural problem later genuinely calls
for it, it will be justified on those merits. It is not in the MVP.

**A mandatory central server.** An optional gateway may be added for
deployments that have infrastructure. It can never become a dependency.

**Cloud APIs of any kind.** Non-negotiable, at every phase.
