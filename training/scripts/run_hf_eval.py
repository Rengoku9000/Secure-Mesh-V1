#!/usr/bin/env python3
"""Runs the HF Qwen2.5 model (optionally with a LoRA adapter) over a split.

# Why this exists alongside run_baseline.py

`run_baseline.py` drives the production GGUF through `llama-server`. That path
cannot evaluate a fine-tuned adapter: llama.cpp does not load a PEFT LoRA, and
exporting one to GGUF is out of scope for evaluation. The adapter therefore has
to be measured through transformers.

That creates a trap. The recorded Phase 3 baseline was produced with **Q4_K_M
quantisation, the llama.cpp runtime on CPU, and `json_schema`
grammar-constrained decoding**. An adapter measured through transformers runs
with NF4 quantisation and PyTorch on GPU. Comparing the two would attribute
unrelated runtime differences to the fine-tuning.

So the control arm is re-measured on the identical path: this script runs the
base model *without* an adapter and *with* one, and nothing else differs —
same weights, same NF4 quantisation, same prompt, same tokenizer, same
decoding, same token budget.

# Grammar-constrained decoding (`--grammar`)

Measured on validation data before the protocol was fixed: without a grammar,
the base model emits `{"category": ...}` and stops — 0 of 20 outputs contained
`severity`, `summary` or `access_status`, so it scored 0% on severity and
access purely because nothing forced it to write those fields. Production
never runs that way: `LlamaServerEngine::generate_structured` always applies
the schema from `ai/prompt.rs::analysis_schema()`.

`--grammar` applies that same schema through lm-format-enforcer, so the
control arm behaves like production and the comparison isolates the adapter's
effect on *content* rather than on whether it remembers to write every field.
The protocol was chosen on validation evidence before any test record was
read; it is applied identically to both arms.

# Faithful to the evaluator

Output is written in exactly the format `evaluate.py` already consumes
(`id`, `predicted`, `latency_ms`, `error`) and `predicted` is the **raw**
completion. No code fences are stripped and no JSON is repaired.

# Test integrity

When `--split test` is used, the file's SHA-256 is checked against the frozen
value before a single record is read, and the run refuses to proceed on a
mismatch. Nothing is written back to any dataset file.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import sys
import time
from datetime import datetime, timezone
from pathlib import Path

os.environ.setdefault("HF_HUB_OFFLINE", "1")
os.environ.setdefault("TRANSFORMERS_OFFLINE", "1")

SCRIPT_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPT_DIR))

import preprocessing as pp  # noqa: E402
import securemesh_prompt as smp  # noqa: E402
from finetune import load_lora_config  # noqa: E402

TRAINING_ROOT = SCRIPT_DIR.parent
PROCESSED = TRAINING_ROOT / "data" / "processed"
CONFIG_PATH = TRAINING_ROOT / "configs" / "lora_config.yaml"

FROZEN_HASHES = {
    "test": "e9ea356f205673c7e59c01e25b8546c59f7539d89d1de0309c763525017196b4",
    "val": "a421ee02faf19a7e7d7649a060f8e9e98494d5f5e52c7da34a1cb1ecde13a0f4",
}


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--split", choices=("test", "val"), default="test")
    parser.add_argument("--adapter", type=Path, help="LoRA adapter directory; omit for the base model")
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--label", required=True)
    parser.add_argument("--limit", type=int, help="First N records only (harness smoke test)")
    parser.add_argument(
        "--grammar",
        action="store_true",
        help="Constrain decoding to ai/prompt.rs::analysis_schema() via lm-format-enforcer, "
        "as production does",
    )
    args = parser.parse_args()

    split_path = PROCESSED / f"{args.split}.jsonl"
    actual_hash = sha256_file(split_path)
    if actual_hash != FROZEN_HASHES[args.split]:
        print(f"REFUSING: {split_path.name} sha256 {actual_hash} != frozen {FROZEN_HASHES[args.split]}")
        return 1
    print(f"{split_path.name} sha256 verified against the frozen value")

    import torch
    from transformers import AutoModelForCausalLM, AutoTokenizer, BitsAndBytesConfig, set_seed

    config = load_lora_config(CONFIG_PATH)
    set_seed(config["training"]["seed"])
    base_path = Path(config["base_model"]["path"])

    # Identical to training and to the smoke tests: NF4, bf16 compute.
    model = AutoModelForCausalLM.from_pretrained(
        base_path,
        local_files_only=True,
        quantization_config=BitsAndBytesConfig(
            load_in_4bit=True,
            bnb_4bit_compute_dtype=torch.bfloat16,
            bnb_4bit_quant_type="nf4",
        ),
        dtype=torch.bfloat16,
        device_map={"": 0},
    )
    tokenizer = AutoTokenizer.from_pretrained(base_path, local_files_only=True)
    pp.configure_tokenizer(tokenizer)

    adapter_hash = None
    if args.adapter:
        from peft import PeftModel

        adapter_file = args.adapter / "adapter_model.safetensors"
        if not adapter_file.exists():
            print(f"REFUSING: no adapter at {adapter_file}")
            return 1
        adapter_hash = sha256_file(adapter_file)
        model = PeftModel.from_pretrained(model, str(args.adapter), local_files_only=True)
        attached = sum(1 for n, _ in model.named_modules() if n.endswith("lora_A.default"))
        print(f"adapter attached to {attached} layers: {args.adapter}")
    else:
        print("no adapter: base model only")
    model.eval()

    schema = smp.analysis_schema()
    grammar_version = None
    tokenizer_data = None
    if args.grammar:
        import importlib.metadata as metadata

        from grammar_adapter import build_token_enforcer_tokenizer_data

        grammar_version = metadata.version("lm-format-enforcer")
        # Expensive (walks the whole vocabulary), so built once and reused.
        tokenizer_data = build_token_enforcer_tokenizer_data(tokenizer)
        print(f"grammar: production analysis_schema via lm-format-enforcer {grammar_version}")
    else:
        print("grammar: none (unconstrained decoding)")

    im_end = tokenizer.convert_tokens_to_ids("<|im_end|>")
    endoftext = tokenizer.convert_tokens_to_ids("<|endoftext|>")
    # Pure greedy, stated explicitly. generation_config.json ships sampling
    # defaults and repetition_penalty 1.1, which generate() inherits unless
    # overridden. A repetition penalty penalises repeated tokens — and JSON is
    # made of repeated quotes, braces, commas and field names.
    generation = {
        "do_sample": False,
        "repetition_penalty": 1.0,
        "max_new_tokens": smp.ANALYSIS_TOKENS,
        "eos_token_id": [im_end, endoftext],
        "pad_token_id": tokenizer.pad_token_id,
        "constrained_decoding": (
            f"json_schema (ai/prompt.rs::analysis_schema) via lm-format-enforcer {grammar_version}"
            if args.grammar
            else None
        ),
    }

    rows = [
        json.loads(line)
        for line in split_path.read_text(encoding="utf-8").splitlines()
        if line.strip()
    ]
    if args.limit:
        rows = rows[: args.limit]
    print(f"{len(rows)} {args.split} records, label {args.label}")

    predictions = []
    started_all = time.perf_counter()
    for index, row in enumerate(rows, start=1):
        record = {"id": row["id"], "model": args.label}
        try:
            text = tokenizer.apply_chat_template(
                smp.chat_messages(row["report_text"]), tokenize=False, add_generation_prompt=True
            )
            inputs = tokenizer(text, add_special_tokens=False, return_tensors="pt").to("cuda")

            extra = {}
            if args.grammar:
                from lmformatenforcer import JsonSchemaParser
                from grammar_adapter import (
                    build_transformers_prefix_allowed_tokens_fn,
                )

                # A fresh parser per record: parser state must not carry over
                # from the previous analysis.
                extra["prefix_allowed_tokens_fn"] = build_transformers_prefix_allowed_tokens_fn(
                    tokenizer_data, JsonSchemaParser(schema)
                )

            torch.cuda.synchronize()
            started = time.perf_counter()
            with torch.no_grad():
                output = model.generate(
                    **inputs,
                    do_sample=False,
                    temperature=None,
                    top_p=None,
                    top_k=None,
                    repetition_penalty=generation["repetition_penalty"],
                    max_new_tokens=generation["max_new_tokens"],
                    eos_token_id=generation["eos_token_id"],
                    pad_token_id=generation["pad_token_id"],
                    **extra,
                )
            torch.cuda.synchronize()
            latency_ms = int((time.perf_counter() - started) * 1000)

            new_tokens = output[0][inputs["input_ids"].shape[1] :]
            completion = tokenizer.decode(new_tokens, skip_special_tokens=True).strip()
            record.update(
                {
                    "predicted": completion,
                    "latency_ms": latency_ms,
                    "new_tokens": int(new_tokens.shape[0]),
                    "hit_token_limit": int(new_tokens.shape[0]) >= generation["max_new_tokens"],
                    "fenced": completion.startswith("```"),
                }
            )
        except Exception as error:  # noqa: BLE001 - one failure must not end the run
            record.update({"predicted": None, "error": f"{type(error).__name__}: {error}"})
        predictions.append(record)

        if index % 20 == 0 or index == len(rows):
            elapsed = time.perf_counter() - started_all
            print(f"  {index}/{len(rows)} ({elapsed:.0f}s, {elapsed / index:.1f}s/record)", flush=True)

    args.out.parent.mkdir(parents=True, exist_ok=True)
    with args.out.open("w", encoding="utf-8") as handle:
        for record in predictions:
            handle.write(json.dumps(record, ensure_ascii=False) + "\n")

    manifest = {
        "timestamp_utc": datetime.now(timezone.utc).isoformat(),
        "label": args.label,
        "split": args.split,
        "split_sha256": actual_hash,
        "records": len(predictions),
        "base_model": str(base_path),
        "adapter": str(args.adapter) if args.adapter else None,
        "adapter_sha256": adapter_hash,
        "quantization": "NF4 4-bit, bf16 compute (bitsandbytes)",
        "runtime": "transformers generate() on CUDA",
        "prompt_sha256": hashlib.sha256(smp.SYSTEM_PROMPT.encode()).hexdigest(),
        "schema_sha256": hashlib.sha256(json.dumps(schema, sort_keys=True).encode()).hexdigest(),
        "generation": generation,
        "request_errors": sum(1 for p in predictions if p.get("error")),
        "hit_token_limit": sum(1 for p in predictions if p.get("hit_token_limit")),
        "fenced_outputs": sum(1 for p in predictions if p.get("fenced")),
        "wall_seconds": round(time.perf_counter() - started_all),
    }
    args.out.with_suffix(".manifest.json").write_text(json.dumps(manifest, indent=2), encoding="utf-8")
    print(f"\nwrote {len(predictions)} predictions to {args.out}")
    print(f"errors {manifest['request_errors']}, hit token limit {manifest['hit_token_limit']}, "
          f"fenced {manifest['fenced_outputs']}, wall {manifest['wall_seconds']}s")
    return 0


if __name__ == "__main__":
    sys.exit(main())
