#!/usr/bin/env python3
"""QLoRA initialization smoke test. Loads, forwards once, and stops.

Answers whether a real training run would start and fit, without starting
one:

1. Does the HF base model load in 4-bit NF4 with bf16 compute?
2. Does the LoRA adapter attach to the modules the config names?
3. Does one masked example forward through it on the GPU with a finite loss?
4. Optionally: does one backward pass produce gradients on the adapter only?
5. How much VRAM does that take, against the ~6.9 GiB available?

**This is not training.** One forward, optionally one backward, no optimizer
step, no epoch, nothing saved. Every hyperparameter comes from
`training/configs/lora_config.yaml` unchanged — the point is to validate the
existing configuration, not to tune it.
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import time
from pathlib import Path

os.environ.setdefault("HF_HUB_OFFLINE", "1")
os.environ.setdefault("TRANSFORMERS_OFFLINE", "1")

SCRIPT_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPT_DIR))

import preprocessing as pp  # noqa: E402
from finetune import build_example, load_lora_config  # noqa: E402

TRAINING_ROOT = SCRIPT_DIR.parent
PROCESSED = TRAINING_ROOT / "data" / "processed"
CONFIG_PATH = TRAINING_ROOT / "configs" / "lora_config.yaml"


def gib(value: int) -> float:
    return value / 1024**3


def vram(torch) -> dict:
    free, total = torch.cuda.mem_get_info()
    return {
        "free_gib": round(gib(free), 3),
        "total_gib": round(gib(total), 3),
        "allocated_gib": round(gib(torch.cuda.memory_allocated()), 3),
        "reserved_gib": round(gib(torch.cuda.memory_reserved()), 3),
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config", type=Path, default=CONFIG_PATH)
    parser.add_argument(
        "--backward",
        action="store_true",
        help="Also run ONE backward pass to verify gradient flow (no optimizer step)",
    )
    parser.add_argument(
        "--batch-size",
        type=int,
        default=1,
        help="Examples per batch for the smoke test. Does NOT change the config; "
        "this only sizes the single forward/backward being measured.",
    )
    parser.add_argument(
        "--example-id",
        help="Test this exact record instead of the longest. Searched across "
        "train AND val: the true longest sequence lives in val, and eval runs "
        "at epoch boundaries, so it reaches the GPU too.",
    )
    parser.add_argument("--out", type=Path, default=TRAINING_ROOT / "results" / "qlora-smoke-test.json")
    args = parser.parse_args()

    import torch
    from peft import LoraConfig, get_peft_model
    from transformers import AutoModelForCausalLM, AutoTokenizer, BitsAndBytesConfig, set_seed

    report: dict = {}
    config = load_lora_config(args.config)

    seed = config["training"]["seed"]
    set_seed(seed)
    print(f"seed: {seed}")

    base_model_path = Path(config["base_model"]["path"])
    load_in_4bit = config["base_model"].get("load_in_4bit", False)
    use_bf16 = config["training"].get("bf16", False)
    max_len = config["training"]["max_seq_length"]

    if not load_in_4bit:
        raise SystemExit("config has load_in_4bit=false; this smoke test is for QLoRA")
    if not use_bf16:
        raise SystemExit("config has bf16=false; this smoke test expects bf16 compute")

    torch.cuda.reset_peak_memory_stats()
    initial = vram(torch)
    report["vram_initial"] = initial
    print(f"\ninitial VRAM: {initial['free_gib']} GiB free of {initial['total_gib']} GiB")

    # --- 4. base model, 4-bit NF4, bf16 compute --------------------------
    print(f"\n=== Loading base model (4-bit NF4, bf16 compute) ===\n  {base_model_path}")
    quantization_config = BitsAndBytesConfig(
        load_in_4bit=True,
        bnb_4bit_compute_dtype=torch.bfloat16,
        bnb_4bit_quant_type="nf4",
    )
    report["quantization"] = {
        "load_in_4bit": True,
        "bnb_4bit_quant_type": "nf4",
        "bnb_4bit_compute_dtype": "bfloat16",
        "source": "training/configs/lora_config.yaml",
    }

    load_started = time.perf_counter()
    model = AutoModelForCausalLM.from_pretrained(
        base_model_path,
        local_files_only=True,
        quantization_config=quantization_config,
        dtype=torch.bfloat16,
        device_map={"": 0},
    )
    load_seconds = time.perf_counter() - load_started
    report["model_load_seconds"] = round(load_seconds, 1)
    print(f"  loaded in {load_seconds:.1f}s")

    tokenizer = AutoTokenizer.from_pretrained(base_model_path, local_files_only=True)
    pp.configure_tokenizer(tokenizer)
    print(f"  pad token: {tokenizer.pad_token!r} (id {tokenizer.pad_token_id})")

    after_base = vram(torch)
    report["vram_after_base_model"] = after_base
    print(f"  VRAM after base model: {after_base['allocated_gib']} GiB allocated, "
          f"{after_base['free_gib']} GiB free")

    # Confirm the weights really are 4-bit, not silently upcast.
    from bitsandbytes.nn import Linear4bit

    linear4bit_count = sum(1 for m in model.modules() if isinstance(m, Linear4bit))
    report["linear4bit_modules"] = linear4bit_count
    print(f"  Linear4bit modules in the loaded model: {linear4bit_count}")
    if linear4bit_count == 0:
        raise SystemExit("no Linear4bit modules — the model did NOT load in 4-bit")

    # --- 5. LoRA ---------------------------------------------------------
    lora_cfg = config["lora"]
    print("\n=== Initializing LoRA adapter (values from config, unchanged) ===")
    for key in ("r", "lora_alpha", "lora_dropout", "bias", "task_type"):
        print(f"  {key}: {lora_cfg[key]}")
    print(f"  target_modules: {lora_cfg['target_modules']}")

    peft_config = LoraConfig(
        r=lora_cfg["r"],
        lora_alpha=lora_cfg["lora_alpha"],
        lora_dropout=lora_cfg["lora_dropout"],
        bias=lora_cfg["bias"],
        target_modules=lora_cfg["target_modules"],
        task_type=lora_cfg["task_type"],
    )
    model = get_peft_model(model, peft_config)
    report["lora"] = {
        "r": lora_cfg["r"],
        "lora_alpha": lora_cfg["lora_alpha"],
        "lora_dropout": lora_cfg["lora_dropout"],
        "bias": lora_cfg["bias"],
        "task_type": lora_cfg["task_type"],
        "target_modules_configured": lora_cfg["target_modules"],
    }

    trainable = sum(p.numel() for p in model.parameters() if p.requires_grad)
    total = sum(p.numel() for p in model.parameters())
    report["parameters"] = {
        "total": total,
        "trainable": trainable,
        "trainable_pct": round(100.0 * trainable / total, 4),
    }
    print(f"\n  total parameters:     {total:,}")
    print(f"  trainable parameters: {trainable:,}")
    print(f"  trainable percentage: {100.0 * trainable / total:.4f}%")

    # Which modules actually received an adapter, as opposed to were asked for.
    adapted = sorted({
        name.split(".lora_A")[0].split(".")[-1]
        for name, _ in model.named_modules()
        if ".lora_A" in name
    })
    adapted_full = [name for name, _ in model.named_modules() if name.endswith("lora_A.default")]
    report["target_modules_adapted"] = adapted
    report["adapted_layer_count"] = len(adapted_full)
    print(f"  modules actually adapted: {adapted}")
    print(f"  adapted layer instances:  {len(adapted_full)}")

    after_lora = vram(torch)
    report["vram_after_lora"] = after_lora
    print(f"  VRAM after LoRA: {after_lora['allocated_gib']} GiB allocated, "
          f"{after_lora['free_gib']} GiB free")

    # --- 6. one masked example, one forward ------------------------------
    print("\n=== Forward pass on one real training example ===")
    rows = []
    for split_name in ("train", "val"):
        for line in (PROCESSED / f"{split_name}.jsonl").read_text(encoding="utf-8").splitlines():
            if line.strip():
                row = json.loads(line)
                row["_split"] = split_name
                rows.append(row)

    if args.example_id:
        match = [r for r in rows if r["id"] == args.example_id]
        if not match:
            raise SystemExit(f"no record with id {args.example_id!r} in train or val")
        print(f"  targeting {args.example_id} (from {match[0]['_split']} split)")
        rows = match + [r for r in rows if r["id"] != args.example_id]
    # Select by TOKEN length, not character length. An earlier version sorted
    # by characters and picked a 574-token example when the true maximum was
    # 596 — close enough to look right, and wrong for a memory test, whose
    # whole purpose is to hit the worst case rather than a comfortable one.
    print(f"  tokenizing {len(rows)} records to find the longest...")
    tokenized_all = []
    for row in rows:
        candidate = build_example(row)
        candidate["_split"] = row.get("_split", "train")
        tokenized_all.append(
            (
                candidate,
                pp.tokenize_with_masking(
                    tokenizer, candidate["system"], candidate["user"], candidate["assistant"],
                    max_seq_length=max_len, example_id=candidate["id"],
                ),
            )
        )
    if args.example_id:
        # Keep the requested record first; fill any remaining batch slots with
        # the next longest, so a batch>1 test still exercises a worst case.
        head = [p for p in tokenized_all if p[0]["id"] == args.example_id]
        tail = sorted(
            (p for p in tokenized_all if p[0]["id"] != args.example_id),
            key=lambda pair: pair[1].total_tokens,
            reverse=True,
        )
        tokenized_all = head + tail
    else:
        tokenized_all.sort(key=lambda pair: pair[1].total_tokens, reverse=True)
    selected = tokenized_all[: args.batch_size]

    print(f"  batch size: {args.batch_size} (longest {args.batch_size} records by token count)")
    report["example"] = []
    for candidate, tok in selected:
        print(f"    {candidate['id']} [{candidate.get('_split','train')}]: "
              f"{tok.total_tokens} tokens "
              f"({tok.prompt_tokens} masked / {tok.target_tokens} supervised)")
        report["example"].append(
            {
                "id": candidate["id"],
                "split": candidate.get("_split", "train"),
                "total_tokens": tok.total_tokens,
                "prompt_tokens": tok.prompt_tokens,
                "target_tokens": tok.target_tokens,
                "truncated": False,
                "within_max_seq_length": tok.total_tokens <= max_len,
            }
        )
    report["batch_size"] = args.batch_size
    tokenized = selected[0][1]
    print(f"  all within max_seq_length={max_len}: "
          f"{all(t.total_tokens <= max_len for _, t in selected)}")

    batch = pp.pad_batch([tok for _, tok in selected], tokenizer.pad_token_id)
    tensors = {k: torch.tensor(v, dtype=torch.long, device="cuda") for k, v in batch.items()}
    print(f"  batch shapes: " + ", ".join(f"{k}={tuple(v.shape)}" for k, v in tensors.items()))
    print(f"  labels contain -100: {(tensors['labels'] == -100).any().item()}")
    print(f"  supervised label count: {(tensors['labels'] != -100).sum().item()}")

    def report_oom(stage: str, error: Exception) -> None:
        """Records an OOM precisely instead of letting it surface as a crash.

        Nothing is retried, resized, or worked around: batch size, sequence
        length, LoRA rank and quantization all stay exactly as configured.
        The measurement at the moment of failure is the deliverable.
        """
        report["oom"] = {
            "stage": stage,
            "batch_size": args.batch_size,
            "sequence_length": len(batch["input_ids"][0]),
            "error": str(error)[:500],
            "peak_allocated_gib": round(gib(torch.cuda.max_memory_allocated()), 3),
            "peak_reserved_gib": round(gib(torch.cuda.max_memory_reserved()), 3),
            "vram_at_failure": vram(torch),
        }
        print(f"\n  *** OUT OF MEMORY during {stage} ***")
        print(f"      batch size {args.batch_size}, sequence {report['oom']['sequence_length']}")
        print(f"      peak allocated {report['oom']['peak_allocated_gib']} GiB")
        print(f"      peak reserved  {report['oom']['peak_reserved_gib']} GiB")
        args.out.parent.mkdir(parents=True, exist_ok=True)
        args.out.write_text(json.dumps(report, indent=2), encoding="utf-8")
        print(f"      written to {args.out}")

    model.eval()
    torch.cuda.synchronize()
    forward_started = time.perf_counter()
    try:
        with torch.no_grad():
            outputs = model(**tensors)
    except torch.OutOfMemoryError as error:
        report_oom("forward", error)
        return 2
    torch.cuda.synchronize()
    forward_ms = (time.perf_counter() - forward_started) * 1000

    loss = outputs.loss
    import math

    finite = bool(loss is not None and math.isfinite(loss.item()))
    print(f"\n  loss: {loss.item():.4f}  (finite: {finite})")
    print(f"  logits: {tuple(outputs.logits.shape)} dtype {outputs.logits.dtype} "
          f"device {outputs.logits.device}")
    print(f"  forward latency: {forward_ms:.0f} ms")
    report["forward"] = {
        "loss": float(loss.item()),
        "loss_finite": finite,
        "logits_shape": list(outputs.logits.shape),
        "logits_dtype": str(outputs.logits.dtype),
        "device": str(outputs.logits.device),
        "latency_ms": round(forward_ms),
        "labels_accepted": loss is not None,
    }
    if not finite:
        raise SystemExit("loss is not finite — stopping")

    peak_forward = vram(torch)
    report["vram_peak_after_forward"] = peak_forward
    report["vram_peak_allocated_gib"] = round(gib(torch.cuda.max_memory_allocated()), 3)
    print(f"  peak VRAM allocated so far: {report['vram_peak_allocated_gib']} GiB")

    # --- optional: ONE backward, to prove gradients flow -----------------
    if args.backward:
        print("\n=== ONE backward pass (gradient flow only, no optimizer step) ===")
        model.train()
        torch.cuda.synchronize()
        backward_started = time.perf_counter()
        try:
            outputs = model(**tensors)
            outputs.loss.backward()
        except torch.OutOfMemoryError as error:
            report_oom("backward", error)
            return 2
        torch.cuda.synchronize()
        backward_ms = (time.perf_counter() - backward_started) * 1000

        with_grad = [(n, p) for n, p in model.named_parameters() if p.requires_grad]
        got_grad = [(n, p) for n, p in with_grad if p.grad is not None]
        base_with_grad = [
            n for n, p in model.named_parameters()
            if p.grad is not None and "lora_" not in n
        ]
        grad_norm = sum(float(p.grad.detach().float().norm() ** 2) for _, p in got_grad) ** 0.5
        print(f"  fwd+bwd latency: {backward_ms:.0f} ms")
        print(f"  trainable params with gradients: {len(got_grad)}/{len(with_grad)}")
        print(f"  BASE (non-LoRA) params that received gradients: {len(base_with_grad)}")
        print(f"  global grad norm: {grad_norm:.4f}")
        report["backward"] = {
            "latency_ms": round(backward_ms),
            "trainable_with_grad": len(got_grad),
            "trainable_total": len(with_grad),
            "base_params_with_grad": len(base_with_grad),
            "grad_norm": round(grad_norm, 4),
            "grad_finite": bool(grad_norm == grad_norm and grad_norm != float("inf")),
        }
        report["vram_peak_allocated_gib"] = round(gib(torch.cuda.max_memory_allocated()), 3)
        model.zero_grad(set_to_none=True)

    final = vram(torch)
    report["vram_final"] = final
    report["vram_peak_reserved_gib"] = round(gib(torch.cuda.max_memory_reserved()), 3)

    # "No OOM" is not the same as "fits". On Windows/WDDM the driver silently
    # spills past physical VRAM into shared system memory over PCIe rather
    # than raising — the run completes and is correct, but an order of
    # magnitude slower. Absence of an exception would report that as success,
    # so peak memory is compared against the physical device instead.
    physical_gib = final["total_gib"]
    peak_gib = max(report["vram_peak_allocated_gib"], report["vram_peak_reserved_gib"])
    spilled = peak_gib > physical_gib
    report["physical_vram_gib"] = physical_gib
    report["peak_exceeds_physical_vram"] = spilled
    report["spill_gib"] = round(peak_gib - physical_gib, 3) if spilled else 0.0

    print("\n" + "=" * 62)
    print(f"  initial free   {initial['free_gib']} GiB")
    print(f"  peak allocated {report['vram_peak_allocated_gib']} GiB")
    print(f"  peak reserved  {report['vram_peak_reserved_gib']} GiB")
    print(f"  physical VRAM  {physical_gib} GiB")
    print(f"  remaining free {final['free_gib']} GiB")

    if spilled:
        print(f"\n  *** PEAK EXCEEDS PHYSICAL VRAM BY {report['spill_gib']} GiB ***")
        print("      The driver spilled into shared system memory instead of raising")
        print("      OutOfMemoryError. The computation is correct but runs over PCIe,")
        print("      which is why latency scales far worse than the batch size.")
        print("\nRESULT: COMPLETED WITHOUT OOM, BUT DOES NOT FIT IN PHYSICAL VRAM")
    else:
        print(f"\n  fits within physical VRAM with {round(physical_gib - peak_gib, 3)} GiB to spare")
        print("\nRESULT: QLoRA INITIALIZATION SMOKE TEST PASSED (no training performed)")

    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(report, indent=2), encoding="utf-8")
    print(f"written to {args.out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
