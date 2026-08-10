# SecureMesh — Security Model

**Status: Phase 2 (local node + peer-to-peer mesh). This document describes what
SecureMesh protects *today*, not what it is intended to protect eventually.**

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
proven otherwise. Two independent checks apply, and both must pass:

1. **The session.** QUIC's TLS 1.3 handshake proves the peer holds the private
   key behind its libp2p `PeerId`. Because that key *is* the node's SecureMesh
   identity key, the session proves the peer is the node it claims to be.
2. **The record.** Every replicated event carries its own author signature and
   its author's public key, bound by `node_id = SHA-256(public key)`. An event
   is verified against **its author's** key, never the key of the peer that
   delivered it.

The second check is what makes relaying safe: B forwarding A's event does not
require C to trust B, only to check A's signature. A relay can withhold or
reorder, but it cannot forge or alter.

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

### Out of scope for Phase 1

Denial of service, physical tamper-evidence, side-channel resistance, supply
chain attestation of the build, and protection against a malicious operator.

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

### 6.5 ~~Records are not signed~~ — CLOSED in Phase 2

Every replicated event is now signed by its author and bound to that author's
public key. Note the residual: the `incidents` table is a *projection* of the
log, and a local attacker who can write to the SQLite file can still alter that
projection. The log itself remains authoritative and tamper-evident, so such an
alteration is detectable by re-deriving the projection from the events.

### 6.6 There is no peer enrolment — **most significant Phase 2 limitation**

Any node on the local network that speaks the protocol can connect, authenticate
with a self-generated identity, and have its events accepted.

Authentication proves a peer *holds the key it claims*. It does **not** prove
the peer is authorised to participate. There is no allowlist, no invitation, no
operator approval step, and no revocation. On an untrusted network, a hostile
node can join the mesh and inject signed records of its own authorship.

What this does still guarantee: an attacker cannot forge records attributed to
*another* node, cannot alter records in transit, and cannot read traffic between
two other peers. The exposure is that its own fabricated records are accepted,
and that it can read what is replicated to it.

Trust-on-first-use is not sufficient for the target deployments. An out-of-band
enrolment step — a QR code, a pre-shared roster, or explicit operator
confirmation — is required before this is fit for a contested environment, and
the mechanism has not yet been chosen.

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

---

## 8. Planned security work

Ordered by the phase that introduces it. None of this is implemented.

| Phase | Work | Addresses |
|-------|------|-----------|
| ~~2~~ | ~~Sign every record; verify on receipt~~ — **done** | §6.5 |
| ~~2~~ | ~~Mutually authenticated peer sessions over QUIC~~ — **done** | Boundary 3 |
| 3 | Out-of-band peer enrolment and revocation | **§6.6** |
| 3 | Per-peer rate limiting and quotas | §6.10 |
| 3 | Durable, append-only, signed audit log | §6.4 |
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
