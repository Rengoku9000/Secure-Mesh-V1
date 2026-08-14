# SecureMesh — Phase 4A Platform Feasibility Study

**Status: study only. Nothing here has been built, bought, flashed, or measured.**

Phase 3 measured everything it claimed on hardware in hand. This document cannot
do that: no candidate board has been acquired. Every figure is therefore either
**cited to vendor documentation** or marked **UNKNOWN** with a note on what
would settle it. Where a claim is commonly repeated but I could not confirm it
from a primary source, it is marked unverified rather than quietly included.

The one thing this document does settle from primary evidence is the awkward
question, because it can be answered from source already on this machine:
**rust-libp2p cannot use a non-exportable hardware key.** See §6.

---

## 1. Six things that are not each other

The single most common failure in this area is treating these as one property.
They are not, and a system can have any of them without the others.

| | What it actually gives you | What it does **not** give you |
|---|---|---|
| **Hardware security** | An umbrella term. On its own, a marketing word. | Anything specific |
| **Secure Boot** | Only firmware signed by a key whose hash is fused into the SoC will execute | No protection *after* boot. A signed kernel that is later exploited is still a compromised kernel |
| **TPM / secure element** | A separate device that generates and uses keys internally; key bytes never enter host RAM | No protection for your *application's* memory or computation |
| **TrustZone** | A CPU/bus partition into secure and normal worlds, with memory the normal world physically cannot address | Nothing by itself — it is a *mechanism*. Empty TrustZone protects nothing |
| **TEE** | An OS (OP-TEE) running inside TrustZone, hosting trusted applications with their own memory and storage | Protection against a physical attacker, side channels, or a compromised secure world |
| **Confidential AI** | Model weights, prompts and outputs invisible to the host OS during inference | Not implied by any of the above. See §8: on these platforms it is not achievable for a 1 GB LLM |

The order matters. **Secure Boot protects the path to a running system.
TrustZone + TEE protect a small amount of computation on that system. A TPM or
secure element protects keys. None of them protects a 1 GB language model.**

SecureMesh today has *none* of these. It has an Ed25519 key in a `0600` file
and says so (`SECURITY.md` §6.1).

---

## 2. Candidate platforms

Three candidates: two Jetsons as the brief specifies, and one non-NVIDIA
TrustZone/OP-TEE platform as the control.

### 2.1 Jetson Orin Nano 8GB

| | |
|---|---|
| CPU | 6-core Arm Cortex-A78AE v8.2 64-bit, max 1.7 GHz, 1.5 MB L2 + 4 MB L3 |
| GPU | 1024-core Ampere, 32 Tensor Cores, 1020 MHz |
| AI performance | 67 sparse INT8 TOPS (Super mode) |
| RAM | 8 GB 128-bit LPDDR5, 102 GB/s |
| Storage | External NVMe / SD card |
| Power | 7 W / 15 W / 25 W modes |
| Dev kit price | **$249** (Orin Nano Super Developer Kit, reduced from $499) |
| Module price | UNKNOWN — see §9 |

A 4 GB variant exists: 34 sparse INT8 TOPS, 4 GB 64-bit LPDDR5, 51 GB/s. Too
little RAM for our model plus the OS plus SecureMesh; not considered further.

### 2.2 Jetson Orin NX 16GB

| | |
|---|---|
| CPU | 8-core Arm Cortex-A78AE v8.2 64-bit, max 2.0 GHz, 2 MB L2 + 4 MB L3 |
| GPU | 1792-core Ampere, 56 Tensor Cores, 1.2 GHz |
| AI performance | 157 sparse INT8 TOPS |
| RAM | 16 GB 128-bit LPDDR5, 102.4 GB/s |
| Storage | External NVMe via M.2 Key M |
| Power | 10 W / 15 W / 25 W / 40 W modes |
| Module price | ~$599 MSRP — **unverified**, from distributor listings rather than an NVIDIA price page |

An 8 GB variant delivers 117 sparse INT8 TOPS.

### 2.3 NXP i.MX 8M Plus — the TrustZone/OP-TEE control

Chosen as the alternative because it is a mainstream industrial SoC with
first-party OP-TEE support in the vendor BSP, and because it is *not* an NVIDIA
part — so it tests whether anything in the analysis depends on NVIDIA
specifically.

| | |
|---|---|
| CPU | 4× Arm Cortex-A53 @ 1.8 GHz |
| Real-time core | Arm Cortex-M7 @ 800 MHz |
| NPU | Present. Performance figure **unverified** — NXP's fact sheet and reference manual were not retrievable during this study; the widely repeated "2.3 TOPS" could not be confirmed from a primary NXP source |
| Security blocks | TrustZone, TZASC (TrustZone Address Space Controller), CAAM (crypto accelerator, RNG, run-time integrity checker), HABv4/AHAB secure boot, RDC |
| Secure storage | RPMB partition in eMMC, accessed by an early OP-TEE trusted application, storing keys, firmware and rollback counters |
| RAM | UNKNOWN for the specific SoM; board-dependent |
| Price | UNKNOWN — board- and vendor-dependent |

**Why it loses regardless of the unknowns:** an LLM at Qwen2.5-1.5B on four
A53 cores with no CUDA would be very much slower than the ~2.5 s/analysis we
measure on a Ryzen. The security story is comparable to Jetson's; the AI story
is not close. It stays in this document as a control, not as a contender.

---

## 3. Security capability comparison

| Capability | Orin Nano | Orin NX | i.MX 8M Plus |
|---|---|---|---|
| Arm TrustZone | Yes | Yes | Yes |
| OP-TEE, vendor-supported | **Yes** — NVIDIA documents OP-TEE for the AGX Orin, Orin NX and Orin Nano series | Yes | Yes, via NXP BSP |
| Secure Boot | Yes — BootROM root of trust, PKC keys | Yes | Yes — HABv4/AHAB |
| Key revocation | Yes — 3 PKC keys, SHA2-512 hashes in `FUSE_PUBLIC_KEY`, `FUSE_PK_H1`, `FUSE_PK_H2` | Yes | UNKNOWN |
| Firmware TPM | **Yes** — TPM 2.0 as an OP-TEE trusted application | Yes | UNKNOWN |
| Discrete secure element | **No** | No | No (external part would be needed) |
| Hardware-bound key provisioning | Yes — Encrypted Keyblob (EKB), 256-bit fuse key on Orin | Yes | Yes — CAAM + RPMB |
| Attestation | fTPM provides an Endorsement Key for device attestation | Same | UNKNOWN |
| Rust trusted applications | Yes — Apache Teaclave TrustZone SDK | Yes | Yes |
| GPU usable from secure world | **No** | **No** | N/A |

### 3.1 What Jetson secure boot actually is

The root of trust is on-die BootROM code that authenticates boot components
using Public Key Cryptography keys whose SHA2-512 hashes are burned into
write-once fuses by the OEM at manufacturing. Orin supports **three** PKC public
keys with a revocation mechanism, so a compromised signing key after shipping is
recoverable. An optional Secure Boot Key (SBK) — eight 32-bit words on Orin —
additionally encrypts bootloader components; used with PKC this is called
SBKPKC.

**Fuses are one-way. Once a fuse bit is 1 it cannot return to 0.** Fusing a
board is irreversible and a mis-fused board is scrap. This is the single largest
operational risk in Phase 4B and argues for buying two boards.

### 3.2 What OP-TEE on Jetson actually is

- `optee_os` runs at **secure EL-1**; trusted applications run at **secure
  EL-0**. The normal world runs Linux.
- Strictly **client-driven**: "all secure operations are initiated by a client
  application running in the non-secure environment. A trusted application, in
  the secure world, never initiates contact with the non-secure environment."
- The call path is: client → TEE Client API (`libteec.so`) → OP-TEE Linux kernel
  driver → Arm Trusted Firmware → OP-TEE OS → the trusted application.
- Two kinds of TA: **user-mode TAs** at S-EL0 using the GlobalPlatform TEE
  Internal Core API, and **pseudo TAs (PTAs)** at S-EL1 inside the OS layer.
- The bootloader reserves a dedicated **TZ-DRAM carveout** for OP-TEE. Carveouts
  on Orin NX/Nano are reserved physical memory "not accessible to Linux or NVIDIA
  CUDA applications". The exact TZ-DRAM size is **UNKNOWN** and is a
  configuration value — see §9.

### 3.3 Encrypted Keyblob: how a key gets in

Jetson Linux provisions secrets to the secure world using the **EKB** mechanism:

- An **EKB fuse key** is burned into hardware (256-bit on Orin, 128-bit on
  Xavier). It is never visible to software; it exists only in Security Engine
  keyslots.
- `EKB_RK` is derived as `AES-128-ECB(FV, EKB fuse key)`; `EKB_DK` values are
  derived from it with a NIST SP 800-108 KDF, yielding an encryption key
  (`EKB_EK`) and an authentication key (`EKB_AK`).
- A PTA decrypts the EKB during boot. **"PTAs inside OP-TEE must use the SE only
  during boot"**, and SE keyslots "must be cleared immediately after OP-TEE uses
  them."
- **"The EKB content is visible in plaintext only to the secure world."**

That last sentence is the property Phase 4B would be buying. It is real, and it
is narrow: it protects *a key*, not *a computation*.

---

## 4. Running Qwen2.5-1.5B on these boards (normal world)

Separate question from confidentiality, and the easy one.

| | Expectation | Confidence |
|---|---|---|
| Model fits in RAM | 1.04 GB of 8 GB (Nano) or 16 GB (NX), alongside OS + SecureMesh + embedding model | High — arithmetic |
| llama.cpp builds and runs | Yes; llama.cpp supports aarch64 CPU and CUDA | High — both are upstream-supported targets |
| Rust toolchain | aarch64-unknown-linux-gnu is a Tier 1 Rust target | High |
| Tauri/WebKitGTK on Jetson Linux | UNKNOWN. Tauri on aarch64 Linux needs WebKitGTK; not verified on Jetson Linux specifically | **Low — must be tested** |
| Inference latency vs. our 2.5 s CPU baseline | UNKNOWN. Plausibly better with CUDA, plausibly worse on CPU-only A78AE cores | **None — do not quote a number until measured** |

The honest position: **we have no performance number for any candidate board and
must not invent one.** Phase 3's discipline was that every figure was measured;
that discipline does not lapse because the hardware is hypothetical.

---

## 5. Can the model run *inside* the TEE? No.

This is the question the brief asks to be answered without faking. The answer is
no, for three independent reasons, any one of which is sufficient.

### 5.1 Memory

OP-TEE trusted applications are sized for cryptographic operations, not models:

- Default TA sizes are `TA_STACK_SIZE` **2 KB** and `TA_DATA_SIZE` **32 KB**.
- A typical configuration has on the order of **30 MB** available for *all* TAs.
- OP-TEE's own kernel needs >256 KiB; each trusted thread costs roughly 8 KB of
  stack.

Qwen2.5-1.5B Q4_K_M is **1.04 GB** — roughly **35× a typical entire TA_RAM
budget**, before the KV cache. Upstream OP-TEE documentation is blunt about the
reason: TrustZone TEEs "are not equipped to support large memory for trusted
applications" because TrustZone targets embedded and mobile devices that do not
need large memory.

Raising `CFG_TZDRAM_SIZE` to over a gigabyte would take that memory permanently
away from Linux, on a board with 8 GB, to run a model slower than the normal
world would.

### 5.2 No GPU in the secure world

The Ampere GPU is the entire reason to choose a Jetson. It is not reachable from
a trusted application: secure carveouts are not accessible to CUDA, and the
converse — a TA driving the GPU — has no vendor-supported path. **A TEE-resident
model would be CPU-only on Cortex-A78AE cores**, discarding the platform's one
advantage in exchange for the confidentiality.

### 5.3 Software stack

llama.cpp expects a POSIX-ish environment: files, mmap, threads, a full libc,
BLAS-style kernels. A TA has the GlobalPlatform Internal Core API. The Rust
route (Teaclave) defaults to `no-std`. Porting a tensor runtime into that
environment is a research project, not a phase of this one.

**Conclusion: full LLM execution inside the TEE is impractical on every platform
considered. It will not be claimed, attempted, or implied.**

---

## 6. libp2p and non-exportable keys — the load-bearing finding

The brief says not to assume libp2p can use a non-exportable hardware key. It
cannot, and this is verified from the source of the exact version we depend on
(`libp2p-identity 0.2.14`, in the local cargo registry), not from documentation.

```rust
// libp2p-identity-0.2.14/src/keypair.rs
pub struct Keypair { keypair: KeyPairInner }

enum KeyPairInner {           // private, closed
    Ed25519(ed25519::Keypair),
    Rsa(rsa::Keypair),
    Secp256k1(secp256k1::Keypair),
    Ecdsa(ecdsa::Keypair),
}

pub fn sign(&self, msg: &[u8]) -> Result<Vec<u8>, SigningError> {
    match self.keypair {
        KeyPairInner::Ed25519(ref pair) => Ok(pair.sign(msg)),
        // ...
    }
}
```

Three facts follow:

1. **`KeyPairInner` is private and closed.** No external crate can add a variant.
2. **There is no signer trait, callback, or hook.** `sign` dispatches on the
   enum to an in-memory key. There is nowhere to inject "ask the TEE".
3. **Every constructor needs the secret.** `generate_ed25519()` creates it in
   process memory; `ed25519_from_bytes()` takes raw bytes. A non-extractable key
   has no bytes to give.

Therefore the libp2p transport identity **must** be a software key in normal-world
memory. Phase 2 already knew this — `src/identity/transport.rs` says so in a doc
comment written before any of this study:

> "A future hardware-backed `KeyStore` will not be able to implement this,
> because a non-extractable key cannot be handed to libp2p at all."

### 6.1 What this means, and the way through

SecureMesh uses its Ed25519 key for **two different jobs**:

| Use | Who signs | Hardware-backable? |
|---|---|---|
| libp2p transport identity — QUIC/TLS handshake, `PeerId` | libp2p, internally | **No** |
| Application signatures — signed events, envelopes, trust audit entries | SecureMesh's own code, via `NodeIdentity::sign` | **Yes** |

Phase 2 deliberately made these **the same key**, so that completing a QUIC
handshake proves the peer is the SecureMesh node it claims to be, with no custom
certificate scheme. That decision is sound and should not be reversed lightly —
but it is exactly what blocks hardware backing, because a key libp2p can use is
a key that exists in RAM.

Three options, none free:

- **(A) Keep one key, accept it is software-held.** Hardware protects nothing;
  we would have a TEE and not use it for identity. Honest but pointless.
- **(B) Two keys: a hardware identity key and a software transport key, bound
  by a hardware-signed certificate.** The transport key gets an attestation
  signed by the non-exportable identity key; peers verify the chain. This
  reintroduces exactly the custom cryptography Phase 2 removed, and the
  transport key remains stealable — an attacker who takes it can impersonate the
  *transport*, though not sign events or trust decisions. **This is the
  realistic option**, and the trade must be stated plainly rather than sold as
  "hardware-backed identity".
- **(C) Patch or fork rust-libp2p to accept an external signer.** Upstream would
  need a signer trait. Cost UNKNOWN; maintaining a fork of a security-critical
  networking stack is a serious ongoing liability. Worth an upstream issue,
  not worth blocking on.

**Recommendation: (B), described accurately.** Under (B) the honest claim is
"event and trust signatures are hardware-backed; the transport session key is
not" — never "SecureMesh uses hardware-backed identity".

---

## 7. Does the `KeyStore` abstraction survive? Not as written.

`KeyStore` was built as the seam for exactly this, and its doc comment already
names `TpmKeyStore`, `SecureElementKeyStore` and `TeeKeyStore` as intended
implementations. The seam is in the right place. **The signature is wrong.**

```rust
pub trait KeyStore: Send + Sync {
    fn load(&self) -> CoreResult<Option<StoredKey>>;   // <-- returns Secret<32>
    fn store(&self, key: &StoredKey) -> CoreResult<()>;
    fn backend_name(&self) -> &'static str;
    fn is_hardware_backed(&self) -> bool;
}
```

`load` **returns the private key bytes**. A hardware store cannot implement it:
there are no bytes to return. `NodeIdentity::from_stored` then builds an
`ed25519_dalek::SigningKey` from those bytes and holds it, and `NodeIdentity::sign`
uses that in-memory key. The whole chain assumes extractability.

### 7.1 Required interface change — a vault becomes a signer

The trait must expose an **operation**, not a **key**. Sketch, not
implementation:

```rust
pub trait KeyStore: Send + Sync {
    /// Loads the existing identity, or None. Returns a *handle*, not a secret.
    fn load(&self) -> CoreResult<Option<KeyHandle>>;

    /// Creates and persists a new identity. On a hardware store the key is
    /// generated inside the device and never leaves it.
    fn create(&self) -> CoreResult<KeyHandle>;

    /// The public key. Always exportable — it is public.
    fn public_key(&self, handle: &KeyHandle) -> CoreResult<[u8; 32]>;

    /// Signs. The only way the private key is ever used.
    fn sign(&self, handle: &KeyHandle, message: &[u8]) -> CoreResult<[u8; 64]>;

    fn created_at(&self, handle: &KeyHandle) -> CoreResult<DateTime<Utc>>;
    fn backend_name(&self) -> &'static str;
    fn is_hardware_backed(&self) -> bool;

    /// Raw secret, for the one caller that cannot work without it: the libp2p
    /// transport identity (§6). A hardware store returns None, and the node
    /// then runs without mesh networking or with a separate transport key.
    /// Deliberately ugly, because the situation is.
    fn export_secret(&self) -> CoreResult<Option<Secret<32>>>;
}
```

Consequences:

- `NodeIdentity` stops holding a `SigningKey` and holds `Arc<dyn KeyStore>` plus
  the public key and a handle. `sign()` delegates.
- `NodeIdentity::sign` becomes **fallible** — hardware can be absent, busy, or
  refuse. Every call site must handle an error it currently cannot get. This is
  the single largest mechanical change, and it reaches into event creation,
  envelope signing and trust decisions.
- `libp2p_keypair()` becomes `Option`-returning, and the mesh must degrade
  gracefully when it is `None` — which fits the existing offline-first design,
  where a node that cannot network is still fully functional.
- `FileKeyStore` keeps working, implementing `export_secret` as `Some`.
- `export_secret` being explicit and separately named is the point: it makes
  "this code path requires an extractable key" greppable, so the boundary is
  visible instead of implied.

**No implementation is proposed for Phase 4A. Nothing above has been written.**

---

## 8. A defensible split — what would and would not be confidential

Since §5 rules out in-TEE inference, the only honest architecture puts a small
amount of high-value work in the TEE and leaves the rest where it is.

```
┌─────────────────────── NORMAL WORLD (Linux) ───────────────────────┐
│  Tauri UI · React frontend                                          │
│  libp2p / QUIC networking, mDNS, transport session key              │
│  SQLite: incidents, event log, trust store, vectors                 │
│  llama.cpp: Qwen2.5-1.5B + BGE embeddings  ← NOT confidential       │
│  Prompts, retrieved passages, model output ← NOT confidential       │
└──────────────────────────────┬──────────────────────────────────────┘
                               │  GlobalPlatform TEE Client API
                               │  (client always initiates)
┌──────────────────────────────▼───── SECURE WORLD (OP-TEE) ──────────┐
│  SecureMesh signing TA                                              │
│    · non-exportable Ed25519 identity key, provisioned via EKB       │
│    · sign(event) / sign(envelope) / sign(trust decision)            │
│    · monotonic sequence counter (anti-rollback)                     │
│  fTPM TA — PCRs, measured boot, attestation EK                      │
└─────────────────────────────────────────────────────────────────────┘
```

**Would be confidential / integrity-protected:**

- the node's identity private key — generated in and never leaving the secure
  world;
- the act of signing an event, an envelope, or a trust decision;
- potentially the per-origin sequence counter, making local rollback of the
  event log detectable rather than merely append-only by convention;
- boot integrity measurements, via fTPM PCRs.

**Would explicitly NOT be confidential:**

- **the model, the prompts, the retrieved passages, and every answer** — all in
  normal-world RAM, readable by root and by anything that can attach a debugger;
- incident text and the SQLite database (encryption at rest with a TEE-sealed
  key is a separate, later, worthwhile step — it protects data at rest, not in
  use);
- the libp2p transport session key (§6);
- anything against a physical attacker with bus or memory access, or against
  side channels.

**The sentence that must appear in any presentation of this design:**

> SecureMesh's *identity and signing* can be hardware-protected. Its *AI
> inference* cannot be, on this class of hardware. Local inference is not
> confidential computing, and calling it that would be false.

This is not a workaround dressed as a design. Protecting the signing key is the
highest-value thing a TEE can do here: it is what stops a compromised node from
being *impersonated permanently*, which is the worst outcome in a trust mesh —
whereas an attacker who can read prompts has already compromised the host and
can read the incidents in SQLite anyway.

---

## 9. Recommendation

**Jetson Orin Nano 8GB (Super Developer Kit, $249) for Phase 4B development.**

Why:

1. **Everything to be proven is present on it.** OP-TEE is vendor-documented for
   the Orin Nano series specifically; secure boot with fused PKC keys, EKB key
   provisioning, and a firmware TPM are all documented for the same family.
   Nothing in the Phase 4B plan needs the NX.
2. **The security work is identical on Nano and NX.** They share the
   architecture and documentation. Developing on the Nano and moving to the NX
   later is a rebuild, not a redesign.
3. **$249 versus ~$599 for an NX module alone**, and a mis-fused board is scrap
   (§3.1). At this price two boards are affordable — one to fuse, one to keep
   unfused for development. **Budget for two.**
4. **8 GB fits the workload:** 1.04 GB model + 35 MB embeddings + OS +
   SecureMesh.
5. **The i.MX 8M Plus loses on AI, not security.** Four A53 cores and no CUDA
   would make the LLM markedly slower, and its security capabilities are not
   better than Jetson's.

Move to **Orin NX 16GB** only if measurement later shows the Nano's GPU or
memory is the binding constraint — which is not currently known, because nothing
has been measured.

---

## 10. Remaining unknowns

Marked honestly, with what would settle each.

| # | Unknown | How to resolve |
|---|---|---|
| 1 | **Inference latency on Jetson**, CPU and CUDA | Buy a board; run our existing `run_benchmark` harness unchanged. It is already portable |
| 2 | **TZ-DRAM carveout size** on Orin Nano, and how far it can be raised | Read the L4T device tree / platform config on a real board |
| 3 | **Does Tauri + WebKitGTK work on Jetson Linux?** | Build the app on a board. A likely source of unpleasant surprises; consider a headless/served UI fallback |
| 4 | **Effort to write the signing TA** and its per-signature latency | Prototype with Teaclave TrustZone SDK; measure `sign()` round-trip. If a signature costs milliseconds, event creation is affected |
| 5 | **Can EKB provision an Ed25519 key**, or only symmetric AES material? | The documented EKB flow is AES-centric. Read the r36.x OP-TEE and EKB documentation for a real board |
| 6 | **fTPM online provisioning** — documented as offline-only in the release reviewed | Check the current JetPack release notes |
| 7 | **Would upstream rust-libp2p accept a signer trait?** | Open an issue. Cheap to ask, decides option (C) in §6 |
| 8 | **India availability, lead time, customs duty, and landed cost** | UNKNOWN and not guessed. Requires quotes from authorised Indian distributors. US list prices are not Indian prices |
| 9 | **i.MX 8M Plus NPU TOPS** | NXP's fact sheet and reference manual were not retrievable here. Obtain the datasheet directly |
| 10 | **Whether fuse-level secure boot is appropriate for a hackathon deliverable at all** | It is irreversible. Consider demonstrating OP-TEE + fTPM *without* burning fuses |

---

## 11. Sources

Vendor and project documentation, not marketing pages, except where a price is
quoted.

- NVIDIA Jetson Linux Developer Guide — [OP-TEE](https://docs.nvidia.com/jetson/archives/r35.5.0/DeveloperGuide/SD/Security/OpTee.html)
- NVIDIA Jetson Linux Developer Guide — [Secure Boot](https://docs.nvidia.com/jetson/archives/r35.6.4/DeveloperGuide/SD/Security/SecureBoot.html)
- NVIDIA Jetson Linux Developer Guide — [Firmware TPM](https://docs.nvidia.com/jetson/archives/r36.4/DeveloperGuide/SD/Security/FirmwareTPM.html)
- NVIDIA — [Jetson Orin module specifications](https://www.nvidia.com/en-us/autonomous-machines/embedded-systems/jetson-orin/)
- NVIDIA — [Jetson Orin Nano Super Developer Kit](https://www.nvidia.com/en-us/autonomous-machines/embedded-systems/jetson-orin/nano-super-developer-kit/) (price)
- OP-TEE — [Core architecture](https://optee.readthedocs.io/en/latest/architecture/core.html)
- OP-TEE — [FAQ, TA memory sizing](https://optee.readthedocs.io/en/latest/faq/faq.html)
- OP-TEE — [Building with Rust](https://optee.readthedocs.io/en/latest/building/optee_with_rust.html)
- Apache — [Teaclave TrustZone SDK](https://github.com/apache/incubator-teaclave-trustzone-sdk)
- NXP — [i.MX 8 applications processors](https://www.nxp.com/products/processors-and-microcontrollers/arm-processors/i-mx-applications-processors/i-mx-8-applications-processors:IMX8-SERIES)
- `libp2p-identity 0.2.14` source, read locally from the cargo registry — §6

---

## 12. Phase 4A stop condition

This document is the deliverable. Nothing has been implemented: no TEE, no
OP-TEE integration, no TPM, no libp2p identity change, no hardware driver, no
change to the AI runtime, no board flashed, no hardware purchased.

Phase 4B needs explicit approval, and should not begin before unknowns **1, 3
and 8** are resolved — respectively whether the board is fast enough, whether
the application runs on it at all, and whether it can actually be obtained.
