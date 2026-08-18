# SecureMesh — Security Model

**Status: Phase 2.5 (local node + authorized peer-to-peer mesh). This document
describes what SecureMesh protects *today*, not what it is intended to protect
eventually.**

## 0. The seven properties, kept apart

These are routinely conflated, and conflating them is how systems end up
trusting anyone who can complete a handshake.

| # | Property | Question it answers | Mechanism | Status |
|---|---|---|---|---|
| 1 | **Identity authentication** | "Are you who you claim to be?" | Ed25519 key possession proved by the QUIC/TLS 1.3 handshake; `node_id = SHA-256(public key)` | ✅ Implemented |
| 2 | **Peer authorization** | "Are you allowed here?" | Persisted per-peer trust state, granted only by an explicit local operator decision | ✅ Implemented (Phase 2.5) |
| 3 | **Transport encryption** | "Can anyone else read this in flight?" | QUIC / TLS 1.3, **hop-by-hop** | ✅ Implemented, §6.7 |
| 4 | **Event authenticity** | "Who wrote this record?" | Per-event Ed25519 signature by its author, verifiable standalone | ✅ Implemented |
| 5 | **Data integrity** | "Has this been altered?" | Signatures + content hashes; equivocation detected and preserved | ✅ Implemented |
| 6 | **Confidentiality at rest** | "Can someone reading the disk see it?" | — | ❌ **Not implemented**, §6.2 |
| 7 | **Revocation** | "Can access be withdrawn?" | Durable REVOKED state, enforced on every message | ⚠️ Local only — **does not propagate**, §6.13 |

**1 and 2 are the pair that matters most.** Authentication proves a peer holds
a key. Anyone can generate a key, so on its own that grants nothing.
Authorization is a separate, explicit, persisted decision. Phase 2 had only the
first; Phase 2.5 adds the second.

The distinction matters. A security document that describes aspirations as
though they were controls is worse than no document at all, because it causes
people to trust a system further than it deserves. Everything below is marked
either **implemented**, **not implemented**, or **planned**.

---

## 1. Scope of this document

SecureMesh Phase 2 is a desktop application that:

- generates and stores an Ed25519 node identity,
- stores incident records in a local, append-only signed event log,
- discovers peers on the local network and replicates that log to them over an
  encrypted, mutually authenticated QUIC session,
- displays all of this in a local user interface.

It has **no AI inference** and **no trusted execution environment**. Sections
covering those subsystems describe planned design, and say so explicitly.

There is **no server** anywhere in the design — not for discovery, not for
identity, not for relaying.

---

## 2. Assets to protect

| # | Asset | Why it matters | Current protection |
|---|-------|----------------|--------------------|
| A1 | Node private signing key | Whoever holds it can impersonate this node to every peer, forever. Now also the transport key | OS file permissions only — **see §6.1**, and §6.9 |
| A2 | Node public key / node ID | Integrity matters (substitution enables impersonation); secrecy does not | Bound as `node_id = SHA-256(key)`, re-checked at every boundary |
| A3 | Incident records | May describe casualties, locations, and response capability | Signed and tamper-evident; encrypted in transit; **not encrypted at rest** — see §6.2 |
| A4 | Database schema integrity | Corruption destroys the operational record | Migrations in a transaction, CHECK constraints, foreign keys |
| A5 | Audit trail | Evidence of what the node did | Written to stderr; **not durable** — see §6.4 |
| A6 | Event log integrity | The log is the authoritative record the mesh replicates | Per-event author signatures; equivocation detected and preserved — see §5.10 |
| A7 | Mesh traffic | Reveals operational activity | Encrypted hop-by-hop; **metadata exposed** — see §6.7, §6.8 |

---

## 3. Trust boundaries

```
┌─────────────────────────────────────────────────────────────────┐
│ HOST OPERATING SYSTEM  (trusted in Phase 1 — see §4)            │
│                                                                 │
│  ┌────────────────────────┐                                     │
│  │  WebView (React UI)    │   UNTRUSTED INPUT ORIGIN            │
│  │  - renders state       │   Everything the user types is      │
│  │  - never validates     │   attacker-controlled data          │
│  └───────────┬────────────┘                                     │
│              │  ◄── TRUST BOUNDARY 1: Tauri IPC                 │
│              │      Crossing inward:  validated in Rust         │
│              │      Crossing outward: public projections only   │
│  ┌───────────▼────────────┐                                     │
│  │  Rust core             │   TRUSTED                           │
│  │  - validates input     │                                     │
│  │  - owns the private key│                                     │
│  └───────────┬────────────┘                                     │
│              │  ◄── TRUST BOUNDARY 2: process ↔ filesystem      │
│  ┌───────────▼────────────┐                                     │
│  │ node_identity.json     │   private key, plaintext            │
│  │ securemesh.sqlite      │   incidents, plaintext              │
│  └────────────────────────┘                                     │
└─────────────────────────────────────────────────────────────────┘

        ◄── TRUST BOUNDARY 3: the mesh
              QUIC / TLS 1.3, peer proves possession of its node key
              Every replicated event independently signed by its author
```

**Boundary 1 — UI ↔ Rust core.** The React layer is treated as untrusted. It
performs some validation for responsiveness, but that validation is *never* the
enforcement point: `NewIncident::validate` in the Rust core re-checks
everything, and a caller that bypasses the UI entirely cannot write an invalid
record. In the outward direction, only public projections cross: there is no
command that returns private key material, and an integration test serialises
every command response and asserts the private key is absent.

**Boundary 2 — process ↔ filesystem.** Discussed in §6.1 and §6.2.

**Boundary 3 — the mesh.** Everything arriving from a peer is hostile until
proven otherwise. Three independent checks apply, and all must pass:

1. **The session** *(authentication)*. QUIC's TLS 1.3 handshake proves the peer
   holds the private key behind its libp2p `PeerId`. Because that key *is* the
   node's SecureMesh identity key, the session proves the peer is the node it
   claims to be.
2. **The trust store** *(authorization)*. The peer's `node_id` must be
   `TRUSTED` locally. Read fresh from SQLite on **every message**, so a
   revocation takes effect on the next message rather than the next
   reconnection. A peer that is merely authenticated gets nothing.
3. **The record** *(authenticity)*. Every replicated event carries its own
   author signature and its author's public key, bound by
   `node_id = SHA-256(public key)`. An event is verified against **its
   author's** key, never the key of the peer that delivered it.

Check 3 is what makes relaying safe: B forwarding A's event does not require C
to trust B, only to check A's signature. A relay can withhold or reorder, but
it cannot forge or alter.

Check 2 is new in Phase 2.5 and is what stops step 1 from being mistaken for
permission.

```text
  peer message
       │
       ▼
  authenticated session?  ── no ──▶ never delivered (transport refuses)
       │ yes
       ▼
  envelope signature matches the authenticated peer? ── no ──▶ rejected
       │ yes
       ▼
  handshake / liveness message?  ── yes ──▶ allowed at any trust state
       │ no  (operational message)
       ▼
  trust_state == TRUSTED?  ── no ──▶ REFUSED + audited
       │ yes
       ▼
  role grants the capability?  ── no ──▶ REFUSED + audited
       │ yes
       ▼
  each event verified against its own author's key
```

---

## 4. Threat assumptions

Phase 1 assumes:

1. **The host OS and its kernel are trusted and uncompromised.** SecureMesh
   runs as an ordinary user-space process with no privilege separation and no
   memory isolation beyond what the OS provides. Malware running as the same
   user defeats every control in this document.
2. **The device is physically controlled by the operator.** There is no
   protection against an attacker with unsupervised physical access, and no
   anti-tamper mechanism.
3. **Full-disk encryption is the deployment's responsibility.** SecureMesh does
   not encrypt its own files (§6.1, §6.2). On a lost or seized device without
   FDE, both the private key and all incident data are readable.
4. **The operator is authorised.** There is no login, no user account, and no
   authorisation model. Anyone who can open the application is treated as the
   node operator.

Assumptions 1–4 are all weakened deliberately by later phases; see §8.

### Out of scope

Denial of service, physical tamper-evidence, side-channel resistance, supply
chain attestation of the build, and protection against a malicious operator.

### 4.1 Adversary scenarios

What actually happens, case by case. "Mitigated" means tested, not intended.

| Scenario | Outcome | Status |
|---|---|---|
| **Malicious node on the local network** joins and speaks the protocol | Authenticates, reaches `PENDING`, receives nothing. Cannot self-approve — no protocol message grants authorization | ✅ Mitigated |
| **Spoofed identity** — a node claims another's node ID | Refused at two independent points: `node_id` must equal `SHA-256` of the key the handshake proved, and the envelope signature must match the authenticated peer | ✅ Mitigated |
| **Revoked device reconnects** | Stays `REVOKED` across reconnects, restarts, and re-announcements. It is never re-offered as a fresh enrolment candidate | ✅ Mitigated |
| **Revoked device with a fresh keypair** | A different `node_id`, so it appears as a new `UNKNOWN` node needing its own decision. It inherits nothing — but note it is also *not automatically denied*: the operator must recognise it. See §6.15 | ⚠️ Partial |
| **Replayed enrolment request** | Idempotent. Repeats neither create duplicate audit entries nor change state, and cannot lift a `REVOKED` peer | ✅ Mitigated |
| **Malformed enrolment / trust input** | Rejected without panicking; empty, non-hex, oversized, path-like and SQL-metacharacter identifiers all covered by tests | ✅ Mitigated |
| **Peer announces itself as an administrator** | Ignored. Roles are local and never taken from the wire; announced capabilities are informational only | ✅ Mitigated |
| **Peer renames itself or moves address** to escape a denial | No effect. Authorization is keyed by the public key fingerprint | ✅ Mitigated |
| **Compromised *trusted* node** | Can inject records of its own authorship and read everything replicated to it. It cannot forge another node's records or alter what it relays. Containment is manual revocation, which does not propagate (§6.13) | ⚠️ Partial |
| **Stolen device** | Holder gets the private key (unencrypted at rest, §6.1) and all local data (§6.2). Other operators must revoke it on each of their nodes; nothing is automatic | ⚠️ Partial |
| **Network partition during revocation** | A disconnected node keeps treating the peer as trusted until its own operator acts. **Inherent to offline-first** and stated plainly rather than papered over | ⚠️ Accepted, §6.13 |
| **Compromised administrator key** | **Not solved.** No key rotation, no revocation of an administrator, no recovery path | ❌ Not addressed |
| **Relay reads what it forwards** | True. Transport encryption is hop-by-hop, not end-to-end. The relay cannot alter or forge, only read and withhold | ⚠️ Accepted, §6.7 |

---

## 5. Implemented controls

These are real and verified by tests in `src-tauri/src` and
`src-tauri/tests/node_lifecycle.rs`.

### 5.1 Cryptographic identity

- **Ed25519** keypair (`ed25519-dalek` v3), generated from the OS CSPRNG via
  `getrandom`. No home-rolled cryptography anywhere in the codebase.
- The node ID is `SHA-256(public_key)` and the display name is derived from it.
  Identity is therefore **derived from key material, not assigned** — a node
  cannot claim an ID it does not hold the private key for.
- Signing and verification are implemented and tested, including rejection of
  tampered messages and cross-node signature confusion. They are not yet *used*
  for anything, because there is no peer to authenticate to.

### 5.2 Private key containment

Four independent mechanisms, so that no single mistake exposes the key:

1. `NodeIdentity` does not implement `Serialize`, so it **cannot** cross the IPC
   boundary. Commands return `PublicIdentity`, a separate type with no secret
   field.
2. `Secret<N>` renders as `<redacted>` under both `Debug` and `Display`, so a
   stray log statement — including a derived `Debug` on any enclosing struct —
   cannot print it. Tested.
3. `Secret<N>` deliberately does not implement `Serialize`. Any struct holding
   one fails to compile if someone tries to derive `Serialize` on it.
4. The key is zeroised on drop (`zeroize`), including the intermediate
   hex-encoded copies made while reading and writing the keystore file.

### 5.3 Input validation

All incident input is validated in Rust before it reaches storage:

- description non-empty after trimming, and bounded at 2 000 **characters**
  (not bytes — a byte limit would truncate non-Latin scripts unfairly);
- severity restricted to the four accepted labels;
- latitude/longitude required as a pair, finite, and within WGS 84 ranges.

Rejection is by returning an error, never by panicking. Malformed input,
including `NaN` coordinates and SQL metacharacters, is covered by tests.

### 5.4 SQL injection resistance

Every statement uses bound parameters. No SQL is built by concatenating
caller-supplied values. A test stores `'); DROP TABLE incidents; --` as a
description and asserts it round-trips as literal text with the table intact.

The two places a value is formatted into SQL text are `LIMIT` (an `u32` clamped
to 500) and `PRAGMA user_version` (a compile-time constant). Neither is
reachable from user input.

### 5.5 Defence in depth at the schema level

The database enforces its own invariants via `CHECK` constraints on severity,
sync status, and coordinate ranges, and a foreign key on incident authorship.
If a future code path forgets to validate, the database still refuses the write.

### 5.6 Durability

`journal_mode=WAL` and `synchronous=FULL` are set per connection, so a committed
incident survives an unclean shutdown or power loss — the normal failure mode
for field hardware. Migrations run inside a transaction, so a node never comes
up on a half-applied schema.

### 5.7 Error handling

`CoreError` carries a stable machine-readable code and a message written to be
free of secrets and of filesystem paths. `serde_json` errors are reduced to a
category label, so a parse failure on the keystore file cannot echo key
material into an error string shown in the UI.

No `unwrap()` or `expect()` appears on a recoverable path in the core. A
poisoned mutex is recovered rather than propagated, so one panicking thread does
not take the node down.

### 5.8 Transport security and peer authentication

**Corrects a Phase 1 statement.** Phase 1 claimed the application opened no
socket. That is no longer true, and the accurate claim is narrower: SecureMesh
contacts **no external service and no cloud API**. It binds a local UDP port for
QUIC and speaks only to peers that authenticate.

| Property | Mechanism |
|---|---|
| Confidentiality and integrity in transit | QUIC with TLS 1.3 (`libp2p-quic`) |
| Forward secrecy | TLS 1.3 ephemeral key exchange |
| Peer authentication | Proof of possession of the Ed25519 node key during the handshake |
| Identity binding | `node_id = SHA-256(public key)`, re-derived and re-checked on every connection |
| Discovery | mDNS on the local link. No bootstrap server, no DHT, no Internet |
| Protocol versioning | Negotiated as `/securemesh/sync/1.0.0`; an incompatible peer fails to negotiate rather than connecting and misinterpreting |

**No custom cryptography.** SecureMesh does not implement a handshake. The
node's existing Ed25519 key is its libp2p identity key, so the transport's own
authentication *is* SecureMesh's peer authentication. The alternative — a
separate transport key bound by a home-made certificate — would have meant
writing cryptographic code to prove something the handshake already proves.

**Key reuse across protocols is handled, not ignored.** Using one key in two
protocols risks a signature from one being valid in the other. Every context is
domain-separated by a distinct, length-prefixed prefix:

| Context | Prefix |
|---|---|
| libp2p handshake | libp2p's own |
| SecureMesh events | `securemesh-event-v1:` |
| SecureMesh envelopes | `securemesh-envelope-v1:` |

Length-prefixing every field additionally prevents field-boundary confusion, so
one signature cannot be reinterpreted as covering a different message.

### 5.12 Peer authorization

Every peer carries a persisted trust state, keyed by
`node_id = SHA-256(public key)`:

| State | Reached by | Permits |
|---|---|---|
| `UNKNOWN` | Default for any node encountered, including one learned only as the origin of a relayed event | nothing |
| `PENDING` | The peer presented itself (sent `HELLO`) | nothing |
| `TRUSTED` | **An explicit local operator decision** | synchronisation, per capability |
| `REVOKED` | An operator rejected or revoked it | nothing |

Properties that are enforced, not merely intended:

- **No protocol message grants authorization.** There is no wire message that
  moves a peer to `TRUSTED`. Decisions enter only through local commands, so an
  enrolled peer cannot promote itself or anyone else. `HELLO` can move a peer
  from `UNKNOWN` to `PENDING` and nothing further.
- **Ordinary nodes hold no administrative capability.** `PeerRole::Node` does
  not grant `PEER_ENROLL` or `PEER_REVOKE`, and capabilities are derived from
  the role rather than stored per node, so a stored list cannot drift.
- **Authorization is re-read on every message.** A revocation takes effect on
  the next message, not the next reconnection.
- **Revocation preserves the record.** Deleting the peer would return it to
  `UNKNOWN`, and the next handshake would offer it as a fresh enrolment
  candidate — making revocation a temporary inconvenience.
- **Enrolment is bound to the key.** A peer that changes its display name, IP
  address, or transport peer ID keeps exactly the authorization it had. A peer
  that changes its *keypair* is a different node ID needing its own decision.
- **The database enforces it too.** `peer_trust_events.node_id` is a foreign key
  into `nodes`, so a decision cannot be recorded about an identity this device
  holds no public key for.

### 5.13 Trust audit trail

Every decision writes an append-only entry to `peer_trust_events` in the same
transaction as the state change, so a decision cannot take effect without a
record or be recorded without taking effect. Each entry carries the peer, the
transition, the deciding node, an optional operator note, and an Ed25519
signature over its canonical encoding (domain `securemesh-trust-v1:`, distinct
from the event and envelope domains). Entries are ordered by a **local
monotonic sequence**, not by wall-clock time.

Audited events: `peer.enrollment.requested`, `peer.enrollment.approved`,
`peer.enrollment.rejected`, `peer.revoked`, `peer.reinstated`. Refused attempts
to use authority the node does not hold are audited as
`authorization.denied`.

### 5.14 Bootstrap of authority

On first launch the local node records itself as `TRUSTED` with role `ADMIN` in
its own trust store. This is not a claim of authority over any other node: it
records that **the operator of this device decides what this device accepts**,
which is true whether or not it is written down. There is no issuer, no
certificate chain, and no delegation. See §6.14 for what this does not do.

### 5.15 The AI trust boundary

Phase 3 introduced a component that consumes untrusted text and produces
untrusted text. It is contained by **structure, not by rules**.

**What the intelligence layer can reach.** Its fields are the boundary:

```rust
pub struct IntelligenceService {
    database: Arc<Database>,
    generator: Arc<dyn LocalInferenceEngine>,
    embedder: Arc<dyn EmbeddingEngine>,
}
```

A database handle and two engines. No `NodeIdentity`, no `KeyStore`, no
`SyncEngine`, no filesystem access. A model cannot sign, enrol a peer, revoke
one, or read a key because **no code path from the model to those things
exists** — not because a check rejects the attempt. `tests/ai_boundary.rs`
asserts this over the source, so a future edit that widens the boundary fails
the build rather than passing unnoticed.

**Model output is untrusted input.** It travels the same path as anything from
the network:

```text
   model output ──▶ parse ──▶ validate ──▶ normalise ──▶ stored as derived
                      │          │
                      └──────────┴──▶ rejected, and nothing is stored
```

`RawAnalysis` is the untrusted shape and has no path to storage except
`validate`. It uses `deny_unknown_fields`, so a model cannot even *express* a
field like `trust_state`. Free text is length-bounded, closed sets degrade to
`OTHER`/`UNKNOWN` rather than admitting invented values, and confidence is
clamped.

**Prompt injection is contained, not prevented.** An incident description is
attacker-influenced — more so once replication carries other nodes' records.
Text is fenced and stripped of its own fence markers, and output is
schema-constrained. None of that makes a model immune to being fooled. What
bounds the damage is that a fooled model can only produce a wrong *analysis*:
it has no capability to misuse. That is the defence, and it is structural.

**The model never overrides a human.** An analysis carries its own severity,
stored separately from the operator's. Silently rewriting a CRITICAL incident to
LOW on a model's say-so would be the worst available failure, so the two are
shown side by side and disagreement is surfaced.

**Derived intelligence is disposable and local.** Analyses and vectors are not
replicated: an inference is an opinion produced by a particular model, and two
nodes running different models will legitimately disagree. Deleting every row of
derived data leaves the operational record intact.

### 5.16 Local inference has no network dependency

| Property | Mechanism |
|---|---|
| No cloud AI | No API, no key, no endpoint. Models are local files an operator provisioned |
| No automatic downloads | Nothing is fetched, at startup or ever |
| No HTTP client | Verified absent from the dependency tree (§5.11) |
| Runtime reachable only locally | `src/ai/loopback_http.rs` takes a port and builds its address from `Ipv4Addr::LOCALHOST`; there is no hostname parameter and no name resolution on that path |
| Failure isolation | The runtime is a child process. A model that crashes or exhausts memory takes down that process, not the node |

**Verified by measurement, not only by argument.** Socket endpoints for the
application and both model servers were sampled while the evaluation was mid-run,
across both the extraction and retrieval phases, on two separate runs:
**zero non-loopback endpoints in every sample, and zero UDP endpoints.** Both
servers bind `127.0.0.1` explicitly; the only connection is the application's own
hop to them. Full output in `docs/ai/EVALUATION.md`.

**Stated precisely.** `rustls` and `hickory-proto` are in the dependency tree,
both via `libp2p` for the Phase 2 mesh — the latter through `libp2p-mdns`, which
parses DNS-format packets on the local multicast group rather than resolving
names against a server. Neither is reachable from the AI path. The claim is "no
HTTP client, and no name resolution on the inference path", not "no networking
crates at all": SecureMesh is a mesh and necessarily has a network stack.

### 5.17 Device location is local, optional, and honestly labelled

Location is **incident data**, not a separate channel. It is captured from the
operating system on this device, reviewed by the operator, and then travels
inside the ordinary signed incident event — so it inherits incident validation,
persistence, event signing, peer authorization and QUIC transport unchanged.

| Property | Mechanism |
|---|---|
| No online location API | No geocoder, no map tiles, no API key. `tests/incident_location.rs` asserts the location path references no URL, HTTP client or geocoding service |
| No new endpoint | Nothing is transmitted separately; coordinates ride the existing signed event |
| Operator-initiated | A position is read only on an explicit action. Rendering the form never prompts and never takes a fix |
| Optional | An incident with no coordinates is fully valid. No receiver, refused permission or a timed-out fix cannot block capture, replication or indexing |
| Snapshot | `getCurrentPosition` only. No `watchPosition`, no movement history |
| Never invented | A provider that cannot answer returns an error. There is no default coordinate and no last-known fallback |
| Not audited on read | A sensor read is an observation, not a state change (§6.4). `incident.created` already records the coordinates kept |

**Source is reported, not assumed.** A desktop rarely has a GNSS receiver;
Windows still answers by triangulating Wi-Fi or resolving the IP address. Those
are city-to-street-block guesses, and obtaining them requires the *operating
system* to reach Microsoft — SecureMesh makes no such call, but the position is
not offline-derived either. Each fix therefore carries its source and accuracy
exactly as the platform reported them. Measured on the development machine:
`Wireless`, **±165 m** — deliberately not labelled satellite.

Only a satellite fix is offline-capable, and `LocationSource::works_offline`
is the single place that rule is expressed.

**Location is operationally sensitive.** It is treated as incident content: it
replicates only to authorized peers, over encrypted QUIC, and only once an
operator has enrolled them.

### 5.9 Hostile input from the network

All of the following are tested (`tests/mesh_sync.rs`, `networking::protocol`):

- oversized frames rejected **before** parsing;
- malformed, truncated, empty and random bytes rejected without panicking;
- unsupported protocol versions refused, and the node keeps serving;
- messages whose envelope signature does not match the authenticated peer
  refused;
- events whose payload was altered in transit refused;
- events claiming another node's identity refused;
- batch sizes, watermark counts, payload and name lengths all bounded before
  the contents are acted on.

A peer sending rubbish is a routine condition, not a fatal one: bad messages are
counted and dropped, and the node continues serving every other peer.

### 5.10 Replication safety

- **Idempotent.** Applying an event twice is a no-op, so duplicate delivery and
  replay converge to the same state.
- **Order-independent.** Events are stored on arrival; the watermark advances
  only over a contiguous run, so gaps re-request themselves.
- **No silent overwrites.** Phase 2 is append-only, so concurrent work merges by
  union. The one genuine conflict — an origin signing two different events at
  the same sequence number — is detected, both versions are retained, the node
  is flagged, and replication from it stops advancing.
- **Durable.** All sync state is in SQLite. Nothing that matters is in memory,
  so a restart mid-round loses no progress.

### 5.11 No cloud exposure

No HTTP client exists in the Rust dependency tree, and there is no external
endpoint anywhere in the codebase. The Content-Security-Policy in
`src-tauri/tauri.conf.json` restricts the WebView to bundled assets. The Tauri
capability set grants `core:default` only.

### 5.9 Secrets are not committed

`.gitignore` excludes `*.key`, `*.pem`, `node_identity*`, `*.sqlite`, `.env`,
and model weights. There are no credentials in the repository — the system has
nothing to authenticate to.

---

## 6. Known limitations

These are genuine weaknesses in the current build. They are listed plainly
because a reader needs them to judge what SecureMesh is fit for today.

### 6.1 The private key is stored unencrypted at rest — **most significant limitation**

`node_identity.json` contains the raw Ed25519 secret key, hex-encoded.

- **Protection that does exist:** on Unix the file is created mode `0600`. On
  Windows it inherits the ACL of the per-user application data directory, which
  by default admits only that user, SYSTEM, and Administrators. The keystore
  refuses to overwrite an existing identity.
- **Protection that does not exist:** there is no passphrase, no encryption, and
  no hardware binding. Anyone able to read the file as that user — malware in
  the user's session, an attacker with a disk image and no FDE, an unencrypted
  backup — recovers the private key and can impersonate the node permanently.

There is no key rotation or revocation mechanism.

**Why it was left this way:** the honest options were a passphrase-derived key
(real, but requires a UX and key-derivation design that Phase 1 does not have
time to do properly) or platform-specific OS keystores (DPAPI on Windows,
Keychain on macOS, kernel keyring on Linux) that would be discarded when
TPM-backed storage lands. Rather than ship a mechanism that *looks* like
encryption without being usefully strong, the key is stored plainly and the
limitation is documented. The `KeyStore` trait exists precisely so the backend
can be replaced without touching the rest of the core.

### 6.2 Incident data is not encrypted at rest

`securemesh.sqlite` is a standard, unencrypted SQLite file. Any process running
as the operator can read every incident. Encrypted storage (SQLCipher, or
application-level encryption with a TPM-sealed key) is planned; see §8.

### 6.3 No authentication or authorisation

Anyone who can launch the application is the operator. There are no roles, no
login, and no per-record access control.

### 6.4 The audit trail is not durable or tamper-evident

Audit records go to stderr. They are not persisted, not signed, and not
append-only. They are useful for diagnostics; they are **not** evidence.

**The audit log records changes and refusals, never reads.** Reading the node's
public identity used to emit a record, which sounded prudent until the dashboard
polled it twice every two seconds — roughly 86,000 records a day stating that
nothing had happened. A log that must be filtered before it can be read is not
an audit log, and that volume would have buried a revocation.

The rule is now explicit: an audit record marks a *state change or a decision* —
a key generated, a peer authorized, a record written, an operation refused. Not
an observation of state that is immutable, non-secret, and already broadcast to
every peer by mDNS. `AuditEvent` has no variant for disclosure, and
`runtime::public_identity` is silent. Measured after the change: one minute of
dashboard polling produces **three** records, all from startup.

Two of those three are `identity.loaded`, which is correct rather than
duplication — the private key really is read from disk twice during startup,
once by the mesh transport (which needs the key before the runtime exists) and
once by the runtime. Two reads of key material, two records. Collapsing them
would make the log less truthful, not less noisy.

`security::audit::capture` lets a test observe what a call emits, so "this is
silent" is checked rather than asserted by reading the source.

### 6.5 ~~Records are not signed~~ — CLOSED in Phase 2

Every replicated event is now signed by its author and bound to that author's
public key. Note the residual: the `incidents` table is a *projection* of the
log, and a local attacker who can write to the SQLite file can still alter that
projection. The log itself remains authoritative and tamper-evident, so such an
alteration is detectable by re-deriving the projection from the events.

### 6.6 ~~There is no peer enrolment~~ — CLOSED in Phase 2.5

A peer must now be explicitly approved by a local operator before anything is
exchanged with it. Authentication no longer implies participation. See §5.12.

Residual limits, all covered below: the decision is **local only** and does not
propagate (§6.13); administrative authority on a device the operator physically
holds is a **policy** control, not a cryptographic one (§6.14); and there is no
enrolment channel that resists an operator being deceived about *which* node
they are approving (§6.15).

### 6.7 Transport encryption is hop-by-hop, not end-to-end

**SecureMesh does not provide end-to-end encryption, and this document will not
claim that it does.**

QUIC protects each link between two directly connected peers. In a multi-hop
path, the relaying node decrypts, processes, and re-encrypts. A relay therefore
**reads the plaintext of everything it forwards**.

Event signatures mean a relay cannot alter or forge what it carries, and its
withholding is detectable as a gap that another path can fill. But confidential
routing through an untrusted intermediary is not a property of this design.

### 6.8 Metadata is not protected

mDNS discovery broadcasts this node's presence on the local link. Anyone on the
same network can observe that a SecureMesh node exists, its node ID, its public
key, and its traffic patterns — even without being able to read message
contents. There is no traffic padding and no cover traffic.

### 6.9 The transport key must be extractable

Peer authentication requires handing the Ed25519 private key to libp2p, so the
key must be readable by the process. **This is incompatible with the
non-extractable hardware key storage planned for Phase 5**: a key sealed in a
TPM cannot be given to libp2p.

Resolving it will mean either a TPM-held identity key plus a separate,
software-held transport key bound to it by a signed statement, or a transport
that can delegate signing to hardware. This is a known architectural tension,
recorded now rather than discovered later. The `KeyStore` trait is unaffected;
only `identity/transport.rs` needs to change.

### 6.10 No rate limiting or resource fairness

Message and batch sizes are bounded, but there is no per-peer rate limit and no
quota. A connected peer can occupy the sync loop with a high volume of valid
traffic. Denial of service remains explicitly out of scope (§4).

### 6.13 Revocation does not propagate — **most significant Phase 2.5 limitation**

A trust decision is **local**. Revoking a peer on node A stops *A* replicating
with it. It does not revoke that peer anywhere else. Every node's operator must
revoke independently.

This is unavoidable in an offline-first system and is stated rather than
disguised: **a disconnected node cannot learn about a revocation until something
reaches it.** A node that was partitioned when a peer was revoked keeps treating
that peer as trusted until its own operator acts.

The model is SSH's `authorized_keys`, not a certificate authority. Making
revocation propagate would mean one node's policy binding another's — a public
key infrastructure, with issuance, chains, delegation, and its own much worse
failure modes, including a compromised authority revoking everyone.

**Designed path forward**, not implemented: replicate signed trust decisions as
first-class events and accept them only from nodes the receiver already treats
as administrators, with explicit conflict states when two administrators
disagree. Until then, safe reconciliation means an operator revoking on each
node that matters.

### 6.14 Administrative authority is policy, not cryptography

`set_local_role` can provision a node as `NODE`, removing its ability to enroll
or revoke. On hardware the operator physically controls this is a **policy**
control: someone with filesystem access can edit the SQLite row and restore
`ADMIN`.

It is defence in depth and a deployment aid, not a barrier against the device's
own holder. A cryptographic version needs hardware-backed provisioning — a role
attested by a key the operator cannot extract — which is Phase 5 work and
depends on §6.9.

### 6.15 Enrolment relies on the operator identifying the right peer

The system guarantees that approving node `SM-A7F32` authorizes exactly the
holder of that keypair. It cannot guarantee the operator meant to approve *that*
node.

Peers are presented by node name and node ID, both derived from the public key,
so a hostile node cannot impersonate an existing peer's identifier. But an
operator faced with an unfamiliar node ID has no in-band way to confirm it
belongs to the device they intend to enroll. Out-of-band verification — reading
the node ID off the other device, a QR code, a pre-shared roster — is required
and is **not** provided by the software.

### 6.17 A model can be fooled into producing a wrong analysis

Prompt injection is contained (§5.15), not prevented. An incident description
crafted to mislead can produce a misleading analysis — a wrong category, an
understated severity, a summary that omits something important.

What it cannot do is act. The consequence is bounded to derived data, which is
disposable and which the UI marks as model output rather than fact. An operator
reading an analysis is reading an opinion, and the interface says so.

### 6.18 Local inference is not confidential computing

The model runs in an ordinary child process. Its memory is readable by anything
running as that user, and the prompts it receives — which include incident text
— are visible to the operating system.

The interface is *shaped* so a trusted backend could replace it without an
application rewrite: `LocalInferenceEngine` passes plain text and returns plain
text, with no shared memory and no callback. **That is preparation, not a
property.** Nothing in Phase 3 is confidential computing, and it must not be
described as such. See §7.

**And on the hardware we would plausibly ship, it never will be.** The Phase 4A
study (`docs/hardware/PLATFORM_EVALUATION.md` §5) found that a typical OP-TEE
configuration offers roughly **30 MB** for all trusted applications against a
**1.04 GB** model, that upstream OP-TEE states TrustZone TEEs are "not equipped
to support large memory for trusted applications", and that secure-world
carveouts are unreachable from CUDA — so a TEE-resident model would also lose
the GPU. Confidential inference is therefore **ruled out, not deferred**. What a
TEE can realistically protect here is the signing key, which is a genuinely
valuable but much narrower claim.

### 6.19 Retrieval quality is not answer correctness

Grounding is checked at the level of *citation*. The answer schema requires a
`sources` field, so a model cannot answer without naming the passages it used;
each number is then verified against what was actually supplied, and any that
was not is discarded and counted in `droppedCitations`. Nothing verifies that
the answer's claims follow from the passage it cites.

A model can therefore cite a real source and still state something the source
does not support — or cite a passage it did not actually use. The UI
distinguishes grounded from ungrounded answers and surfaces dropped citations,
which is a weaker guarantee than "the answer is correct" and is labelled
accordingly.

### 6.20 Evaluation figures come from synthetic data

Accuracy in `docs/ai/EVALUATION.md` is measured against a generated corpus with
templated phrasing. Real field reports are messier, so those numbers are an
upper bound on a much easier task — not a prediction of field performance.

Two of the metrics are weaker than they look, and `EVALUATION.md` says so in
detail: severity accuracy is measured against a convention the model is never
told, part of which is unsignalled in the text; and the refusal set is five
questions, which bounds how much that rate can be trusted.

### 6.16 A trusted peer is trusted for everything in scope

Capabilities are coarse: a `TRUSTED` node with `INCIDENT_SYNC` receives the
whole replicated log. There is no per-incident, per-origin, or
classification-based filtering. Enrolling a peer means sharing everything the
node holds and will hold.

### 6.11 No protection against a compromised host

SecureMesh is an ordinary user-space process. It has no memory protection
beyond the OS's, and its key material is readable by anything that can attach a
debugger or read process memory as that user. This is what Phase 5 addresses,
and it is why the TEE work is not cosmetic.

### 6.12 Dependency supply chain is not yet audited

Dependencies were resolved from crates.io and npm at their current versions and
are pinned by `Cargo.lock` / `package-lock.json`. No `cargo audit` /
`npm audit` gate runs in CI yet, and there is no reproducible-build or
SBOM process.

---

## 7. Explicitly NOT claimed

To avoid the category errors that are common in this space:

- **SecureMesh does not currently use a TEE.** A normal Linux or Windows
  process is not a trusted execution environment, however carefully written.
- **A TPM is not a TEE.** A TPM 2.0 chip is a hardware root of trust that
  protects *keys* and *measurements*. A TEE is an isolated *execution*
  environment that protects code and data **while running**. SecureMesh will
  eventually want both; they solve different problems and neither substitutes
  for the other.
- **SecureMesh has not been penetration tested or independently reviewed.**
- **The cryptography is not novel and should not be.** SecureMesh uses standard
  primitives from maintained libraries.
- **SecureMesh does not provide end-to-end encryption.** Transport encryption is
  hop-by-hop (§6.7).
- **Peer authentication is not peer authorisation.** Any node that can reach the
  network can join (§6.6).
- **Phase 2 is not confidential computing.** Nothing here changes §7's position
  on TEEs.
- **This is not a PKI.** There is no certificate authority, no issuance, no
  chain of trust, and no delegation. Trust decisions are local policy, in the
  manner of SSH `authorized_keys` (§5.12, §6.13).
- **Revocation does not propagate.** Revoking a peer on one node revokes it
  there and nowhere else (§6.13).
- **A compromised administrator key is not solved.** There is no key rotation,
  no administrator revocation, and no recovery path.
- **Device location may not be satellite-derived.** On a machine with no GNSS
  receiver the platform may return a Wi-Fi or IP-based estimate, which is
  accurate to hundreds of metres or worse and required the OS to reach the
  network. The source and accuracy are always shown; they must not be described
  as GPS.
- **No offline map, no geocoding.** Coordinates are displayed as numbers.
- **Enrolment does not verify intent.** Approving a node ID authorizes exactly
  that keypair; whether it is the device the operator meant is out-of-band
  (§6.15).
- **Confidential AI is not on the roadmap, because it is not achievable on this
  hardware class.** Not "not yet" — the Phase 4A study measured the gap against
  vendor documentation and found it structural (§6.18,
  `docs/hardware/PLATFORM_EVALUATION.md` §5).
- **"Hardware-backed identity" would be an overstatement even after Phase 5.**
  libp2p cannot use a non-exportable key — verified in the dependency's source —
  so the transport session key must remain software-held. The accurate claim
  would be "event and trust signatures are hardware-backed; the transport key is
  not" (`PLATFORM_EVALUATION.md` §6).
- **No claim in the Phase 4A study has been demonstrated on hardware.** No board
  has been bought, flashed, or benchmarked. Every figure there is cited to vendor
  documentation or marked UNKNOWN.

---

## 8. Planned security work

Ordered by the phase that introduces it. None of this is implemented.

| Phase | Work | Addresses |
|-------|------|-----------|
| ~~2~~ | ~~Sign every record; verify on receipt~~ — **done** | §6.5 |
| ~~2~~ | ~~Mutually authenticated peer sessions over QUIC~~ — **done** | Boundary 3 |
| ~~2.5~~ | ~~Out-of-band peer enrolment and revocation~~ — **done** | §6.6 |
| ~~2.5~~ | ~~Signed, append-only trust audit log~~ — **done** | §5.13 |
| 3 | Out-of-band identity verification aid (QR code / roster) so an operator can confirm *which* node they approve | §6.15 |
| 3 | Replicated, administrator-signed revocation with explicit conflict states | **§6.13** |
| 3 | Per-peer rate limiting and quotas | §6.10 |
| 3 | Durable, append-only, signed audit log for non-trust events | §6.4 |
| 5 | Hardware-attested node roles, so administrative authority is cryptographic | §6.14 |
| 4+ | End-to-end encryption for multi-hop paths, if the threat model requires it | §6.7 |
| 3 | Model integrity verification before loading any local model | New asset |
| 5 | TPM 2.0 / secure element `KeyStore` backend: key sealed to platform state, signing performed in hardware so the private key never enters process memory. **Requires resolving the transport-key tension in §6.9.** | §6.1, §6.9, §6.11 |
| 5 | Encrypted database with a key sealed to the TPM | §6.2 |
| 5 | Measured boot, and remote attestation where the platform supports it | §6.11 |
| 5 | Confidential inference inside a TEE | §6.6 |
| — | `cargo audit` and `npm audit` in CI; SBOM generation | §6.7 |
| — | Operator authentication, if a deployment requires multi-user nodes | §6.3 |

Hardware-dependent items are deliberately **not** committed to a specific
platform. TEE availability differs sharply across candidate boards, and
Jetson-class devices in particular do not offer the same guarantees as a
server-class confidential computing platform. The capability will be verified on
real hardware before any claim is made about it. See
`docs/architecture/ROADMAP.md`.

---

## 9. Reporting a security issue

This is a Smart India Hackathon 2026 prototype and is **not deployed**. Report
issues through the project's issue tracker. Do not use this build to handle
real operational, classified, or personal data — see the data policy in
`README.md`.
