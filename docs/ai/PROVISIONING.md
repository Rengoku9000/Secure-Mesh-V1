# SecureMesh — Provisioning Local Intelligence

**SecureMesh never downloads a model.** Not at startup, not on first use, not
in the background. Provisioning is a deliberate operator action, and a node
without it is fully functional — it simply reports intelligence as unavailable.

That is not a limitation to work around. A field node must not fetch several
gigabytes over a metered or absent link because a user clicked something.

---

## What a provisioned node holds

```
ai/
├── runtime/
│   └── llama-cpu/
│       └── llama-server.exe        inference runtime (llama.cpp b10375, MIT)
└── models/
    ├── llm/
    │   └── qwen2.5-1.5b-instruct-q4_k_m.gguf     1.04 GB, Apache-2.0
    └── embeddings/
        └── bge-small-en-v1.5-q8_0.gguf            35 MB, MIT
```

Full provenance, licences, checksums and the reasoning behind each choice are in
[`ai/models/README.md`](../../ai/models/README.md).

Total: **~1.1 GB**. Weights are excluded from version control.

---

## Provisioning

Any machine with the files can provision one that does not — a USB stick is a
perfectly good transport, and is the expected one for an air-gapped deployment.

### 1. Inference runtime

Download the prebuilt llama.cpp release for the target platform from
<https://github.com/ggml-org/llama.cpp/releases/tag/b10375> and extract it to
`ai/runtime/llama-cpu/`.

| Platform | Asset |
|---|---|
| Windows x64, CPU | `llama-b10375-bin-win-cpu-x64.zip` |
| Windows x64, CUDA | `llama-b10375-bin-win-cuda-12.4-x64.zip` |
| Windows ARM64 | `llama-b10375-bin-win-cpu-arm64.zip` |

The CPU build is the default. See `ai/models/README.md` for why, despite a
CUDA-capable GPU being present on the development machine.

### 2. Models

| File | Source |
|---|---|
| `ai/models/llm/qwen2.5-1.5b-instruct-q4_k_m.gguf` | <https://huggingface.co/Qwen/Qwen2.5-1.5B-Instruct-GGUF> |
| `ai/models/embeddings/bge-small-en-v1.5-q8_0.gguf` | <https://huggingface.co/CompendiumLabs/bge-small-en-v1.5-gguf> |

### 3. Verify before use

```powershell
Get-FileHash ai\models\llm\qwen2.5-1.5b-instruct-q4_k_m.gguf -Algorithm SHA256
Get-FileHash ai\models\embeddings\bge-small-en-v1.5-q8_0.gguf -Algorithm SHA256
```

Both must match `ai/models/README.md`. A mismatch means the file is not the
model this build was evaluated against; do not load it.

A model file is **executable input** — it is interpreted by the runtime. Treat
one from an unverified source the way you would treat an unsigned binary.

### 4. Confirm

Start SecureMesh. The **Intelligence** panel should read:

```
Status:             READY
Model:              qwen2.5-1.5b-instruct-q4_k_m · Q4_K_M
Embeddings:         BGE Small EN v1.5
Inference:          LOCAL
Network dependency: NONE
```

`UNAVAILABLE` states the reason: a missing runtime and a missing model are
reported differently, because the fix differs.

---

## Where the files are looked for

The application looks for `ai/models` beside its executable, then walks up a few
directories — which covers both a staged build and running from `target/debug`.

`SECUREMESH_AI_ROOT` overrides this, for a deployment that keeps models on
separate storage:

```powershell
$env:SECUREMESH_AI_ROOT = "D:\securemesh-models"
```

Models live beside the application rather than in each node's data directory:
they are large, read-only, and shared by every node on the machine.

---

## Confirming no network is used

After provisioning, disconnect the machine entirely — Wi-Fi off, Ethernet
unplugged — and confirm that analysis and questions still work.

The claim is also structural, and can be checked without unplugging anything:

```powershell
cd src-tauri
cargo tree | Select-String "reqwest|hyper|ureq|isahc|curl"   # no results
```

There is no HTTP client in the dependency tree. The runtime is reached through
`src/ai/loopback_http.rs`, which takes a **port** and builds its address from
`Ipv4Addr::LOCALHOST` — there is no hostname parameter, no URL parsing, and no
name resolution on that path, so reaching a remote host is not forbidden but
unrepresentable.

`rustls` and `hickory-proto` do appear in the tree, both via `libp2p` for the
Phase 2 mesh (`hickory-proto` through `libp2p-mdns`, which parses DNS-format
packets on the local multicast group rather than resolving names against a
server). Neither is reachable from the AI path.

---

## Operating without intelligence

A node with no model is a normal node. It creates incidents, replicates them,
enrols peers, and serves its dashboard exactly as before; only the Intelligence
panel differs. This is enforced structurally — the service is an `Option` the
runtime holds, and analysis is never on the path of incident creation or
synchronisation.

Deleting the model files at any time returns a node to that state. Derived
intelligence already stored remains readable and can be cleared; the incidents
and the signed event log are untouched, because analyses are derived data and
nothing depends on them.

---

## Changing model

Vectors from different embedding models are **not comparable**, so changing the
embedding model invalidates the index. The stored `model_id` on every vector
means this is detected rather than silently mis-scored: retrieval simply ignores
vectors from a different model, and the next indexing pass re-embeds everything.

Changing the *generation* model does not invalidate the index, but existing
analyses were produced by the old one. Each records its `model_id`, so they can
be identified and re-run.

---

## Running the evaluation

```powershell
cd src-tauri
cargo run --example run_benchmark -- 42 300 25 20
#                                     │   │   │  └ questions
#                                     │   │   └ incidents analysed
#                                     │   └ corpus size
#                                     └ dataset seed
```

Writes `docs/ai/benchmark-latest.json` and prints a summary. It refuses to run
without a provisioned model rather than reporting zeros that could be mistaken
for measurements. Results and method: [`EVALUATION.md`](EVALUATION.md).
