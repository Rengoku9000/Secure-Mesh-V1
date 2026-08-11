# SecureMesh — Demo Script

**Runtime: about 8 minutes.** Two things to show: a SecureMesh node is genuinely
self-contained, and two of them form a mesh with no server and no Internet.

Everything below is verifiable live. Nothing is staged.

---

## Before you start

```powershell
npm install
npm run app:stage
```

`app:stage` builds a **standalone** binary into `dist-app/` — one that carries
the frontend inside it and needs no dev server. Then launch **two** nodes, each
with its own data directory, because the directory *is* the node: it holds the
identity and the log.

```powershell
$env:SECUREMESH_DATA_DIR="$env:TEMP\smA"
Start-Process .\dist-app\securemesh.exe

$env:SECUREMESH_DATA_DIR="$env:TEMP\smB"
Start-Process .\dist-app\securemesh.exe
```

Two windows open, showing two different node names.

> **Do not demo from `src-tauri/target/debug/`.** Any `cargo test` or
> `cargo clippy` silently replaces that binary with a development build, which
> expects the Vite dev server and shows `ERR_CONNECTION_REFUSED` on its own.
> Nothing is more embarrassing five minutes before a demo. `dist-app/` is
> immune, because `cargo` never writes there.

**Do this:** disconnect Wi-Fi and unplug Ethernet before you begin, and leave
them off for the whole demo. The mesh runs on the local link; nothing here wants
the Internet. If you have two laptops on one switch or ad-hoc Wi-Fi, use those
instead — it is the same demo and more convincing.

*(For a single-node walkthrough, `npm run tauri dev` still works and skips to
section 3.)*

---

## 1. The node identifies itself (45 s)

Point at the header: **Node SM-XXXXX**, status **OFFLINE**, peers **0**.

> "This node generated an Ed25519 keypair the first time it launched. The name
> isn't assigned by a server — it's derived from the public key. The node ID is
> the SHA-256 of that key, so a node can't claim an identity it doesn't hold the
> private key for."

Point at the **Node identity** panel: algorithm, node ID, public key, and key
storage backend.

> "Public key, shown. Private key, never — it has no path to this screen. It
> isn't in any command response, and the type that holds it doesn't implement
> serialisation, so it can't cross the boundary even by mistake."

**If asked how that is enforced rather than merely intended:** four independent
mechanisms, in `docs/security/SECURITY.md` §5.2, and a test that serialises every
command response and asserts the key is absent.

---

## 1b. The two nodes find each other — and refuse to talk (2 min) — **the centrepiece**

Look at the **Peers** panel in either window. Within a few seconds each node
lists the other: node name, `CONNECTED` — and `PENDING`, with a banner saying
*"1 peer awaiting enrollment."*

> "Neither was configured with the other's address. They found each other by
> mDNS on the local link — no server, for discovery or anything else."

**Now create an incident in window A. Watch window B.** Nothing arrives.

> "They're connected. They've authenticated — each has cryptographically proved
> it holds the private key behind its node ID. And they are exchanging *nothing*.
>
> That's the distinction this phase is about. Authentication answers 'are you
> who you claim to be'. Anyone can generate a keypair, so on its own that
> answers nothing useful. Authorization is a separate question — 'are you
> allowed here' — and it needs a human decision."

Click **Approve** in window A, then in window B. The incident appears in B
within a couple of seconds, and both peers now read `TRUSTED` · `Synced`.

**Worth stressing:** there is no protocol message that grants authorization.

> "Node B cannot approve itself. There is no message it can send that moves it
> to TRUSTED — the decision only enters through a local command on this device.
> That's what stops an enrolled peer promoting itself, or anyone else."

### Revocation

In window A, click **Revoke** on peer B. Then create another incident in A.

It does not reach B. B still shows as `CONNECTED` — the session is open; the
authorization is not.

> "Revocation takes effect on the next message, not the next reconnection. And
> we keep the peer record rather than deleting it — deleting would send the node
> back to UNKNOWN, and the next handshake would offer it as a fresh enrollment
> candidate. That would make revocation a temporary inconvenience."

**Close both windows and reopen them.** B is still `REVOKED`.

**Be straight about the limitation if asked** — and it is the right question:

> "That revocation is local to node A. It does not propagate. If there were a
> node C on this mesh, C would still trust B until its own operator revoked it.
> That's inherent to offline-first: a disconnected node can't learn about a
> revocation until something reaches it. We chose SSH's authorized_keys model
> over a certificate authority deliberately — making it propagate means one
> node's policy binding another's, which is a PKI, and a compromised authority
> then revokes everyone. It's documented as the top limitation of this phase."

---

## 1c. Replication, once authorized (60 s)

**Create an incident in window A —**

- **Description:** `Bridge on NH-48 collapsed, southbound lane impassable`
- **Severity:** `HIGH`

It appears in window B within a couple of seconds. Point out that the incident
in B is still attributed to **A's** node ID.

> "B didn't copy a database. A appended a signed event to its log, and B pulled
> what it was missing, checked A's signature, and applied it. Authorship
> survives replication because it's part of what was signed."

### Now break it — the partition test

1. Disconnect the network (or stop node B's process).
2. In A, create `INC-A`. In B, create `INC-B`. Both keep working; both show the
   record locally.
3. Reconnect.

Both nodes end up with **both** incidents.

> "Nothing was overwritten. Most systems would resolve that by comparing
> timestamps and discarding one side — which is exactly wrong when the clocks
> are two field devices with no GNSS. SecureMesh never orders by wall clock.
> Events are immutable and each node numbers its own, so merging is set union:
> there's no conflict to resolve, by construction."

**If asked "what if there is a genuine conflict?"**

> One case exists: a node signing two *different* events at the same sequence
> number — it forked its own log. We detect it, keep both versions, record which
> peer reported it, flag the node, and stop replicating from it. Nothing is
> silently overwritten. The peer shows as "Conflicting history" in the panel.

---

## 2. The status panel tells the truth (45 s)

Point at the five subsystem rows:

| Row | Reads | Why |
|---|---|---|
| Local database | ● Healthy | Real SQLite file, migrated |
| Node identity | ● Active | Real Ed25519 keypair |
| Network | ● Connected | Authenticated **and enrolled** peers over encrypted QUIC |
| Local AI | ○ Not installed | Phase 3 — no model on this node |
| TEE | ○ Not available | Phase 5 — this is an ordinary OS process |

> "The Network row changed between Phase 1 and Phase 2 because the *system*
> changed — the status comes from the Rust core, not from a constant in the UI.
> The two that still say 'missing' are telling the truth for the same reason."

This is worth dwelling on. **A TEE row that said "enabled" on a normal Windows
process would be false**, and the distinction between a TPM and a TEE is exactly
the sort of thing this project should get right.

**If asked how peers are authenticated:**

> The node's Ed25519 key *is* its libp2p identity key, so the QUIC/TLS 1.3
> handshake already proves the peer holds the key behind its node ID — and the
> node ID is the SHA-256 of that key. We wrote no handshake and no cryptography
> of our own. What we did write is domain separation, so a signature made in one
> context can't be replayed in another.

---

## 3. Record an incident (60 s)

Click **Create incident**. Enter:

- **Description:** `Bridge on NH-48 collapsed, southbound lane impassable`
- **Severity:** `HIGH`
- **Latitude:** `12.9716` **Longitude:** `77.5946`

Save. The incident appears immediately in the timeline with severity, location,
relative time, and sync status **PENDING**.

> "PENDING is accurate, not decorative. There's no peer to send it to yet, so
> nothing has been synchronised. When Phase 2 lands, this field starts moving on
> its own."

**Show validation is real.** Open the dialog again and try:
- an empty description → rejected
- latitude `95` → rejected

> "Those checks run in Rust, not in the form. The form is a convenience; the
> core is the enforcement point. A caller that bypassed this UI entirely still
> couldn't write an invalid record."

---

## 4. The data is real — restart the app (60 s)

**This is the most convincing part of the demo. Do not skip it.**

Close the SecureMesh window entirely. Relaunch it.

> "Same node name. Same node ID. The incident is still there."

The identity was reloaded from the keystore rather than regenerated, and the
incidents came back from a SQLite file on disk. To make the point concrete,
show the files:

```powershell
# Windows
ls $env:APPDATA\org.securemesh.node
```

```
node_identity.json        the keypair
securemesh.sqlite         the incidents
```

**Being straight about the keystore is a strength, not a weakness.** If someone
asks whether that file is encrypted:

> "No, and that's documented as the most significant limitation in the security
> model. It's protected by OS file permissions only. Encrypting it properly
> means either a passphrase design we haven't built yet, or the TPM-backed
> storage in Phase 5 — and we'd rather state that plainly than ship something
> that looks like encryption without being strong."

---

## 5. Themes and accessibility (30 s)

Use the **Auto / Light / Dark** switch in the header.

> "One token set drives both themes, so text can't become unreadable in one of
> them. Field operators use these at night and in direct sunlight."

---

## 6. Offline is the default, not a mode (45 s)

If the machine is still disconnected, say so. If not, disconnect now — the UI
will not change, because nothing was ever depending on the network.

```powershell
cd src-tauri
cargo tree | Select-String "reqwest|hyper|tokio-tungstenite"   # no results
```

> "There's no HTTP client in the dependency tree and no external endpoint
> anywhere in the codebase. This isn't a node with cloud calls switched off —
> there is nothing to switch off."

---

## 7. Tests (30 s)

```powershell
cd src-tauri
cargo test
cargo clippy --all-targets --all-features -- -D warnings
```

218 tests. Alongside the Phase 1 coverage — private-key containment, SQL
injection resistance, validation, migration idempotency, restart persistence —
the mesh tests cover partition and reconciliation, the full offline scenario
(disconnect → create → restart → reconnect), duplicate delivery, reversed
delivery order, mid-sync restart, equivocation, spoofed senders, tampered
events, unsupported protocol versions, malformed floods, and three-node relaying
where the endpoints never meet.

> "Those run over an in-process transport, deliberately. Partition and replay
> are the cases that actually break replication, and over real sockets they'd be
> timing-dependent and flaky — and flaky tests get deleted. Here they're
> decisions we assert. The real QUIC path has its own test."

---

## Questions you should expect

**"Where's the AI?"**
> Phase 3. Networking and storage come first — inference with nothing durable
> to reason over, and no way to share results, would be a demo rather than an
> architecture. The status panel says "not installed" because it isn't.

**"Why not blockchain? The theme mentions it."**
> The theme is "Blockchain & Cybersecurity" and we fit through cybersecurity.
> Consensus sacrifices availability under partition, and availability under
> partition is the entire requirement here. Adding a ledger would work against
> the design rather than for it.

**"Is this actually secure?"**
> Some of it, provably; the rest is documented as not yet done.
> `docs/security/SECURITY.md` lists the limitations by name. What's implemented
> is real: standard Ed25519 and TLS 1.3 from maintained libraries, parameterised
> SQL, validated input, no key exposure, and every event independently
> verifiable.

**"So anyone on the network can join?"**
> No — that was true in Phase 2 and it's what Phase 2.5 fixed. A node that
> connects reaches PENDING and receives nothing until an operator approves it.
> Authentication proves key possession; authorization is a separate, persisted,
> human decision, enforced in the Rust core on every message.

**"Could a malicious node approve itself?"**
> No. There is no protocol message that changes trust state — decisions enter
> only through local commands on the device. And an ordinary node holds no
> enroll or revoke capability at all, so even a trusted peer can't promote
> anyone.

**"What if it comes back with a new keypair?"**
> Then it's a different node ID and shows up as a new UNKNOWN peer needing its
> own decision. It inherits nothing — but note it isn't automatically *denied*
> either. The operator has to recognise that it shouldn't be approved, and we
> don't yet give them an out-of-band way to confirm which physical device a node
> ID belongs to. That's a documented gap.

**"Is this a PKI?"**
> No, and we don't claim it is. No certificate authority, no chain, no
> delegation. It's closer to SSH's authorized_keys: each node records what it
> accepts. That's why revocation doesn't propagate.

**"Is this end-to-end encrypted?"**
> No. QUIC protects each hop. On a multi-hop path the relay decrypts and
> re-encrypts, so it reads what it forwards — it just can't change it, because
> every event carries its author's signature. We don't claim E2E.

**"What's the hardest part still ahead?"**
> Two things. Peer enrolment, above. And a real architectural tension: libp2p
> needs the private key in process memory, which is incompatible with the
> non-extractable TPM storage planned for Phase 5. That's written down in
> `SECURITY.md` §6.9 rather than left to be discovered later.

---

## What NOT to claim

- Do not say the node uses a TEE. It does not.
- Do not call the TPM a TEE, or vice versa.
- Do not say data is encrypted at rest. It is not.
- **Do not say the mesh is end-to-end encrypted.** It is hop-by-hop.
- **Do not call this a PKI.** There is no certificate authority and no chain.
- **Do not say revocation propagates.** It is local to each node.
- **Do not claim a compromised administrator key is handled.** It is not.
- Do not demonstrate with real operational or personal data. All demo data is
  synthetic.
