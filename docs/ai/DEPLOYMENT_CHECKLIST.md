# SecureMesh-SLM candidate — deployment checklist

**Status: NOT DEPLOYED.** The candidate model is a separate artifact. Production
runs the stock model. Nothing in this document has been executed.

This is the checklist to work through *when* deployment is authorised, plus the
evidence gathered so far and the limits of that evidence.

---

## Model

| | |
|---|---|
| Candidate path | `training/results/models/securemesh-slm-v2-qlora-r1/securemesh-slm-v2-qlora-r1-q4_k_m.gguf` |
| Candidate SHA-256 | `bf03ee464040cafc5883c48afee2785a56955fc737c8033a68fefb255f36fc16` |
| Size | 986,047,968 bytes (940.37 MiB) |
| Quantization | Q4_K_M (338 tensors, `general.file_type = 15`) |
| llama.cpp | b10375, commit `ba360efe1f574ebae727aad64112d18ecedca85a` |
| Proposed `model_id` | `securemesh-slm-v2-qlora-r1-q4_k_m` |
| Derived from | checkpoint-66, adapter `ff833cf78fc1c5ec5499189b0cc1d75b6466336955cef3f1fd2618550c4d3bf6` |

**Currently active in production (unchanged):**

| | |
|---|---|
| Path | `ai/models/llm/qwen2.5-1.5b-instruct-q4_k_m.gguf` |
| SHA-256 | `6a1a2eb6d15622bf3c96857206351ba97e1af16c30d7a74ee38970e434e9407e` |
| `model_id` | `qwen2.5-1.5b-instruct-q4_k_m` |

---

## Software

- **Schema** — `ai::prompt::analysis_schema()`; nine properties,
  `additionalProperties: false`, `required` = category, severity, summary,
  access_status. **`confidence` deliberately absent.**
- **Prompt** — `analysis_system_prompt()` + `analysis_user_message_with_facts()`.
  `fence_report()` strips the report fence markers *and* chat-template control
  markers (`<|`, `|>`).
- **Mirror** — `training/scripts/securemesh_prompt.py` must match the Rust
  schema and fencing. Enforced by `tests/prompt_mirror_drift.rs`, which fails
  loudly on divergence and on a missing mirror. Do not deploy with that test
  failing or skipped.
- **Consistency layer** — `ai::consistency` runs after `RawAnalysis::validate`
  and before the result is operator-facing. It produces `AnalysisOutcome
  { analysis, consistency }`. It never modifies the analysis.
- **Integrity** — `LlamaConfig::expected_sha256` is verified in
  `ensure_running()` immediately before the runtime is spawned. A wrong digest
  stops the load; there is no fallback to another artifact.

### Switching the model (source change + rebuild)

There is no configuration switch. `LlamaConfig::generation()` hardcodes the
path and `model_id`, and `SECUREMESH_AI_ROOT` overrides only the *root
directory*, not the filename.

1. Place the candidate GGUF under `ai/models/llm/` **under its own filename**.
2. Update `LlamaConfig::generation()`: `model_path`, `model_id`,
   `display_name`, and `expected_sha256` → `bf03ee46…36fc16`.
3. Rebuild. Confirm `cargo test` passes, including the integrity tests.
4. Start, and confirm the reported `model_id` is the candidate's.

⚠️ **Do not deploy by overwriting the production filename.** `availability()`
checks existence, not identity. Before Phase 8's pinning, a swapped file loaded
silently while every analysis was still stamped `qwen2.5-1.5b-instruct-q4_k_m`
— mislabelling one model's judgement as another's. The digest pin now prevents
the load, but the correct procedure is still a source change, not a file swap.

---

## Security

- **Control-marker containment — deterministic.** 14/14 adversarial inputs
  fully contained at the prompt layer; no `<|`/`|>` survives fencing, and
  exactly one fence pair remains.
- **Format integrity under attack — held.** Both models: valid JSON 14/14,
  schema-valid 14/14, and **0 of 6** format-break attempts succeeded.
- **Prose-level injection — NOT mitigated in the model.** Measured: the stock
  model adopted 10/10 demanded field values, the candidate 9/10. Both leaked
  attacker text into schema-valid fields on 2/14. **No injection immunity is
  claimed.**
- **Mitigation is downstream**, not in the model: strict schema validation,
  closed-set degradation, bounded text, and the consistency layer raising
  `needsOperatorReview`.
- **Operator review is required, not advisory** — a coerced answer is still
  produced. Safety depends on an operator reading the flag.
- **AI advisory boundary** — the model reaches no capability. `networking/`,
  `sync/`, `identity/` and `security/` contain zero references to analysis
  types; `EventKind` has two variants (`IncidentCreated`,
  `IncidentObservation`); analyses never replicate.

---

## Networking — FROZEN, and independent of the model

libp2p + QUIC + mDNS, `PROTOCOL_VERSION = 1`, capabilities
`["sync/1", "incidents/1"]` (compile-time literals, no model input).

No model identifier, path or AI concept appears anywhere in `networking/`,
`sync/` or `identity/`. Derived intelligence cannot cross the mesh, so two
nodes running **different local models remain interoperable by construction**.

Verified by suite: `mesh_libp2p` 4/4, `mesh_sync` 25/25, `peer_trust` 26/26,
`sync_determinism` 16/16.

---

## Raspberry Pi

- The Pi **keeps its existing model**. The candidate is not copied to it.
- Nothing about this deployment requires a Pi-side change.
- **Physical laptop ↔ Pi validation has NOT been performed.** Code-level
  compatibility passes; hardware validation is outstanding and is the next
  phase. No simulated substitute has been produced or accepted.

---

## Rollback

1. Revert the `LlamaConfig::generation()` change (path, `model_id`,
   `display_name`, `expected_sha256`) and rebuild.
2. If `SECUREMESH_AI_ROOT` was set, unset it.
3. Verify the stock GGUF digest is
   `6a1a2eb6d15622bf3c96857206351ba97e1af16c30d7a74ee38970e434e9407e`.
4. Restart, and confirm the reported `model_id` is
   `qwen2.5-1.5b-instruct-q4_k_m`.
5. No database migration is involved. Stored analyses carry their own
   `model_id`, so intelligence from both models stays distinguishable and
   readable, and historical analyses are **not** relabelled.
6. Embeddings are unaffected — a separate engine and a separate pin.

---

## Evaluation data must not be tuned against

The following are **consumed or frozen**. No model, prompt, schema or rule
change may be made to improve a score on any of them, and any regression test
needing a fixture gets a **new** one:

- Phase 5 test set — `training/data/processed/test.jsonl` (consumed)
- Phase 6 sanity corpus — `training/data/sanity/phase6_sanity_inputs.jsonl`
- Phase 8 holdout — `training/data/holdout/holdout_v3.jsonl`
- Phase 8 security corpus — `training/data/sanity/phase8_security_corpus.jsonl`

The Phase 8 holdout was authored *after* earlier results were known. It is a
newly authored set, not a pre-existing holdout, and should not be treated as
definitive real-world validation.
