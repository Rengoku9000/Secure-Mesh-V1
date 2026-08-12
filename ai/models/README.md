# SecureMesh — Local Model Provenance

**Nothing in this directory is downloaded by the application.** SecureMesh never
fetches a model, at startup or at any other time. Provisioning is a deliberate
operator step, documented in [`docs/ai/PROVISIONING.md`](../../docs/ai/PROVISIONING.md).

Model weights are excluded from version control (see `.gitignore`). This file
records what a correctly provisioned node holds, so an operator can verify the
files they were given.

---

## Hardware this selection was made for

Measured on the development machine, not assumed:

| Component | Value |
|---|---|
| CPU | AMD Ryzen AI 7 350, 8 cores / 16 threads, 2.0 GHz base |
| RAM | 23 GB |
| GPU | NVIDIA RTX 5050 Laptop, 8 GB VRAM (6.4 GB free), driver 610.88, compute 12.0 |
| iGPU | AMD Radeon 860M |
| NPU | Ryzen AI (present in silicon; **not used** — see below) |
| OS | Windows 11 Home 64-bit |
| Disk free | 573 GB |

**The CPU-only build is used deliberately**, despite a CUDA-capable GPU being
present:

- The models are small enough to be fast on CPU (measured below), so the GPU
  buys little at this size.
- CPU-only is the honest baseline for the edge hardware SecureMesh targets in
  Phase 6, much of which has no discrete GPU at all.
- It keeps the runtime a single portable directory with no CUDA runtime
  dependency, so a provisioned node is reproducible.
- The NPU is unused because no mature, portable local-LLM runtime targets it.

A CUDA build can be dropped in later without touching a line of SecureMesh
code — the runtime is selected by path, behind `LocalInferenceEngine`.

---

## Inference runtime

| | |
|---|---|
| Runtime | llama.cpp, prebuilt release `b10375` |
| Build | `llama-b10375-bin-win-cpu-x64` |
| Source | <https://github.com/ggml-org/llama.cpp/releases/tag/b10375> |
| License | MIT |
| Location | `ai/runtime/llama-cpu/` |

Prebuilt binaries are used rather than Rust FFI bindings because building
llama.cpp from source needs CMake, Clang/libclang and Ninja, none of which are
present on this machine. Adding that toolchain would make the build far heavier
for no functional gain.

---

## Generation model

| | |
|---|---|
| Name | Qwen2.5-1.5B-Instruct |
| File | `llm/qwen2.5-1.5b-instruct-q4_k_m.gguf` |
| Quantisation | Q4_K_M (4-bit) |
| Size | 1.04 GB |
| Parameters | 1.5 B |
| Context used | 2048 tokens |
| License | **Apache-2.0** |
| Source | <https://huggingface.co/Qwen/Qwen2.5-1.5B-Instruct-GGUF> |
| SHA-256 | `6A1A2EB6D15622BF3C96857206351BA97E1AF16C30D7A74EE38970E434E9407E` |

**Why this model.** Selected against the criteria in the Phase 3 brief:

- **Structured output** — this is the deciding criterion. Incident analysis is
  worthless if the output cannot be parsed, and Qwen2.5-Instruct follows a
  JSON schema reliably under llama.cpp's constrained decoding (verified: see
  measurements).
- **Size** — 1.04 GB fits comfortably in RAM alongside the embedding model and
  the application, and is plausible on an edge SBC.
- **Licence** — Apache-2.0 permits the deployments SecureMesh targets. Models
  under research-only or non-commercial licences were excluded regardless of
  quality.
- **Speed on CPU** — measured below; adequate for interactive use.

A larger model was **not** chosen: at 7 B the CPU latency would be several
times worse for a task that is mostly classification and field extraction, and
it would stop fitting on the edge hardware this is meant to run on.

---

## Embedding model

| | |
|---|---|
| Name | BAAI bge-small-en-v1.5 |
| File | `embeddings/bge-small-en-v1.5-q8_0.gguf` |
| Quantisation | Q8_0 |
| Size | 35 MB |
| Dimensions | **384** (measured, not assumed) |
| License | **MIT** |
| Source | <https://huggingface.co/CompendiumLabs/bge-small-en-v1.5-gguf> |
| SHA-256 | `EC38E8DA142596BAA913124AE50550DE284B6916BF59577EF2F0CB9660C2F514` |

Q8_0 rather than Q4: at 35 MB the file is already negligible, and retrieval
quality depends directly on embedding fidelity. The upstream model card notes
that quantisation "will not provide huge benefits" at this size, so the
smaller file would trade recall for nothing worth having.

---

## Measured performance

Taken on the hardware above, CPU only. These are real measurements from the
provisioning smoke test, not estimates:

| Operation | Latency |
|---|---|
| Structured incident extraction (schema-constrained, ~200 tokens) | **2.6 s** |
| Single text embedding | **54 ms** |
| Generation model load | ~1.1 s |

Fuller figures, including accuracy, are produced by the evaluation harness —
see `docs/ai/EVALUATION.md`.

---

## Verifying a provisioned node

```powershell
Get-FileHash ai\models\llm\qwen2.5-1.5b-instruct-q4_k_m.gguf -Algorithm SHA256
Get-FileHash ai\models\embeddings\bge-small-en-v1.5-q8_0.gguf -Algorithm SHA256
```

Both must match the values above. A mismatch means the file is not the model
this node was built and evaluated against, and it should not be loaded.
