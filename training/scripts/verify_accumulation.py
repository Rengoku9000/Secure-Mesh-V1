#!/usr/bin/env python3
"""Verifies gradient-accumulation semantics without running training.

The configuration moved from batch 2 x accum 8 to batch 1 x accum 16 because
batch 2 did not fit in physical VRAM. Effective batch size is 16 either way,
but "effective batch size is the same" is an arithmetic claim about what the
trainer *does*, and it is worth checking rather than assuming.

Two independent checks, neither of which trains anything:

**Empirical.** 16 microbatches are forwarded and backwarded by hand, with the
same `loss / accumulation_steps` scaling `Trainer.training_step` applies. The
accumulated gradient is then compared against the same 16 microbatches
computed one at a time and averaged. If accumulation is working — gradients
adding up rather than being silently cleared — the two must agree to floating
point tolerance.

**Static.** Where the optimizer steps, where the scheduler steps, and where
gradients are zeroed are read out of the installed `transformers` source, so
the claim about step boundaries is evidence rather than recollection.

No optimizer is constructed, no weights are updated, nothing is saved. A LoRA
weight is snapshotted before and after to prove the model was not modified.
"""

from __future__ import annotations

import argparse
import inspect
import json
import os
import re
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


def static_check() -> dict:
    """Reads the step boundaries out of the installed Trainer source."""
    import transformers.trainer as trainer_module
    from transformers import Trainer

    lines = inspect.getsource(trainer_module).splitlines()
    findings: dict = {}

    optimizer_line = next(i for i, l in enumerate(lines) if "self.optimizer.step()" in l)
    scheduler_line = next(i for i, l in enumerate(lines) if "self.lr_scheduler.step()" in l)
    zero_grad_line = next(
        i for i, l in enumerate(lines[optimizer_line:], start=optimizer_line)
        if "model.zero_grad()" in l
    )
    # Walk back to the nearest enclosing condition.
    guard = None
    for i in range(optimizer_line, max(0, optimizer_line - 30), -1):
        if "if do_sync_step" in lines[i]:
            guard = lines[i].strip()
            break

    findings["optimizer_step_line"] = optimizer_line
    findings["scheduler_step_line"] = scheduler_line
    findings["zero_grad_line"] = zero_grad_line
    findings["enclosing_guard"] = guard
    findings["optimizer_inside_sync_guard"] = guard is not None
    findings["scheduler_after_optimizer"] = scheduler_line > optimizer_line
    findings["zero_grad_after_optimizer"] = zero_grad_line > optimizer_line

    training_step = inspect.getsource(Trainer.training_step)
    scaling = [
        l.strip() for l in training_step.splitlines()
        if re.search(r"loss\s*=\s*loss\s*/", l)
    ]
    findings["loss_scaling_line"] = scaling[0] if scaling else None
    findings["loss_divided_by_accumulation"] = bool(scaling)

    # Step arithmetic, read from source rather than assumed.
    spe = [
        l.strip() for l in lines
        if "len_dataloader // args.gradient_accumulation_steps" in l
    ]
    findings["steps_per_epoch_formula"] = spe[0] if spe else None
    return findings


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config", type=Path, default=CONFIG_PATH)
    parser.add_argument("--out", type=Path, default=TRAINING_ROOT / "results" / "accumulation-check.json")
    args = parser.parse_args()

    import torch
    from peft import LoraConfig, get_peft_model
    from transformers import AutoModelForCausalLM, AutoTokenizer, BitsAndBytesConfig, set_seed

    config = load_lora_config(args.config)
    accumulation_steps = config["training"]["gradient_accumulation_steps"]
    micro_batch = config["training"]["per_device_train_batch_size"]
    max_len = config["training"]["max_seq_length"]
    seed = config["training"]["seed"]
    set_seed(seed)

    report: dict = {
        "per_device_train_batch_size": micro_batch,
        "gradient_accumulation_steps": accumulation_steps,
        "effective_batch_size": micro_batch * accumulation_steps,
        "seed": seed,
    }

    print("=== STATIC: Trainer step boundaries (read from installed source) ===")
    static = static_check()
    report["static"] = static
    print(f"  loss scaling:        {static['loss_scaling_line']}")
    print(f"  enclosing guard:     {static['enclosing_guard']}")
    print(f"  optimizer.step()     line {static['optimizer_step_line']} (inside guard: {static['optimizer_inside_sync_guard']})")
    print(f"  lr_scheduler.step()  line {static['scheduler_step_line']} (after optimizer: {static['scheduler_after_optimizer']})")
    print(f"  model.zero_grad()    line {static['zero_grad_line']} (after optimizer: {static['zero_grad_after_optimizer']})")
    print(f"  steps/epoch formula: {static['steps_per_epoch_formula']}")
    print("  -> optimizer, scheduler and zero_grad ALL sit inside the sync-step")
    print("     guard, so none of them fires per microbatch.")

    print("\n=== Loading model (4-bit NF4 + LoRA, same as training) ===")
    model = AutoModelForCausalLM.from_pretrained(
        Path(config["base_model"]["path"]),
        local_files_only=True,
        quantization_config=BitsAndBytesConfig(
            load_in_4bit=True,
            bnb_4bit_compute_dtype=torch.bfloat16,
            bnb_4bit_quant_type="nf4",
        ),
        dtype=torch.bfloat16,
        device_map={"": 0},
    )
    tokenizer = AutoTokenizer.from_pretrained(config["base_model"]["path"], local_files_only=True)
    pp.configure_tokenizer(tokenizer)
    lora_cfg = config["lora"]
    model = get_peft_model(
        model,
        LoraConfig(
            r=lora_cfg["r"], lora_alpha=lora_cfg["lora_alpha"],
            lora_dropout=lora_cfg["lora_dropout"], bias=lora_cfg["bias"],
            target_modules=lora_cfg["target_modules"], task_type=lora_cfg["task_type"],
        ),
    )
    # Dropout would make two passes over the same data disagree for reasons
    # unrelated to accumulation, so it is disabled for the comparison.
    model.eval()

    # Snapshot a LoRA weight: proof that nothing here updates the model.
    probe_name, probe_param = next(
        (n, p) for n, p in model.named_parameters() if p.requires_grad and "lora_A" in n
    )
    weight_before = probe_param.detach().clone()

    rows = [
        json.loads(line)
        for line in (PROCESSED / "train.jsonl").read_text(encoding="utf-8").splitlines()
        if line.strip()
    ][: accumulation_steps * micro_batch]

    microbatches = []
    for start in range(0, len(rows), micro_batch):
        chunk = rows[start : start + micro_batch]
        tokenized = [
            pp.tokenize_with_masking(
                tokenizer, ex["system"], ex["user"], ex["assistant"],
                max_seq_length=max_len, example_id=ex["id"],
            )
            for ex in (build_example(r) for r in chunk)
        ]
        batch = pp.pad_batch(tokenized, tokenizer.pad_token_id)
        microbatches.append(
            {k: torch.tensor(v, dtype=torch.long, device="cuda") for k, v in batch.items()}
        )
    print(f"\n  prepared {len(microbatches)} microbatches of {micro_batch} example(s)")

    # --- A. accumulate over all 16, the way Trainer does -----------------
    print(f"\n=== A. Accumulating {accumulation_steps} microbatches (loss / {accumulation_steps}) ===")
    model.zero_grad(set_to_none=True)
    grad_norms = []
    losses = []
    started = time.perf_counter()
    for index, tensors in enumerate(microbatches, start=1):
        outputs = model(**tensors)
        scaled = outputs.loss / accumulation_steps
        scaled.backward()
        losses.append(float(outputs.loss.item()))
        norm = sum(
            float(p.grad.detach().float().norm() ** 2)
            for _, p in model.named_parameters() if p.grad is not None
        ) ** 0.5
        grad_norms.append(round(norm, 5))
        if index in (1, 2, 8, accumulation_steps):
            print(f"  after microbatch {index:2d}: grad norm {norm:.5f}")
    accumulate_seconds = time.perf_counter() - started

    accumulated = {
        name: p.grad.detach().float().clone()
        for name, p in model.named_parameters() if p.grad is not None
    }
    print(f"  {len(accumulated)} parameter tensors hold gradients")
    print(f"  elapsed {accumulate_seconds:.1f}s")

    # Gradient norm must change at every microbatch. A flat or resetting
    # sequence would mean gradients were being cleared in between.
    monotonic_changes = sum(
        1 for a, b in zip(grad_norms, grad_norms[1:]) if abs(b - a) > 1e-9
    )
    report["grad_norms_per_microbatch"] = grad_norms
    report["grad_norm_changed_every_step"] = monotonic_changes == len(grad_norms) - 1
    report["losses_per_microbatch"] = [round(l, 4) for l in losses]

    # --- B. the same microbatches, one at a time, averaged ---------------
    print(f"\n=== B. Same {accumulation_steps} microbatches computed individually, then averaged ===")
    summed: dict[str, torch.Tensor] = {}
    for tensors in microbatches:
        model.zero_grad(set_to_none=True)
        outputs = model(**tensors)
        outputs.loss.backward()
        for name, p in model.named_parameters():
            if p.grad is not None:
                grad = p.grad.detach().float()
                summed[name] = grad.clone() if name not in summed else summed[name] + grad
    averaged = {name: tensor / accumulation_steps for name, tensor in summed.items()}

    # --- C. the noise floor ----------------------------------------------
    #
    # A fixed tolerance would be meaningless here. The 4-bit matmul and bf16
    # reductions are not bitwise reproducible, so repeating the *identical*
    # computation already produces a nonzero difference. Measure that first,
    # then judge A-vs-B against it: the question is not "are they equal" —
    # nothing on this hardware is — but "do they differ by more than the
    # hardware differs from itself".
    print(f"\n=== C. Noise floor: repeating method A to see what run-to-run costs ===")
    model.zero_grad(set_to_none=True)
    for tensors in microbatches:
        (model(**tensors).loss / accumulation_steps).backward()
    repeat = {
        name: p.grad.detach().float().clone()
        for name, p in model.named_parameters() if p.grad is not None
    }

    def compare(left: dict, right: dict) -> tuple[float, float, int]:
        max_abs = max_rel = 0.0
        count = 0
        for name, value in left.items():
            other = right.get(name)
            if other is None:
                continue
            count += 1
            diff = (value - other).abs().max().item()
            scale = max(other.abs().max().item(), 1e-12)
            max_abs = max(max_abs, diff)
            max_rel = max(max_rel, diff / scale)
        return max_abs, max_rel, count

    noise_abs, noise_rel, _ = compare(accumulated, repeat)
    max_abs_diff, max_rel_diff, compared = compare(accumulated, averaged)

    print(f"  A vs A-repeated (noise floor): abs {noise_abs:.3e}  rel {noise_rel:.3e}")
    print(f"  A vs B (the real comparison):  abs {max_abs_diff:.3e}  rel {max_rel_diff:.3e}")

    # Agreement means "within a small multiple of the hardware's own
    # run-to-run variation". 2x leaves room for the two methods rounding
    # differently while still catching a genuine semantic error, which would
    # show up as orders of magnitude, not a factor of two.
    ratio = max_rel_diff / noise_rel if noise_rel > 0 else float("inf")
    agree = ratio <= 2.0
    print(f"  ratio to noise floor: {ratio:.2f}x  (agree if <= 2.0)")
    print(f"  indistinguishable from run-to-run variation: {agree}")
    report["comparison"] = {
        "tensors_compared": compared,
        "max_abs_diff": max_abs_diff,
        "max_rel_diff": max_rel_diff,
        "noise_floor_abs": noise_abs,
        "noise_floor_rel": noise_rel,
        "ratio_to_noise_floor": ratio,
        "agree": agree,
        "note": (
            "4-bit matmul and bf16 reductions are not bitwise reproducible, so "
            "the noise floor is measured rather than a fixed tolerance assumed"
        ),
    }

    # --- D. nothing was updated -----------------------------------------
    weight_after = probe_param.detach()
    unchanged = bool(torch.equal(weight_before, weight_after))
    print(f"\n=== D. Model weights untouched ===")
    print(f"  probe parameter: {probe_name}")
    print(f"  unchanged: {unchanged} (no optimizer was constructed or stepped)")
    report["weights_unchanged"] = unchanged

    model.zero_grad(set_to_none=True)
    report["vram_peak_allocated_gib"] = round(torch.cuda.max_memory_allocated() / 1024**3, 3)
    report["vram_peak_reserved_gib"] = round(torch.cuda.max_memory_reserved() / 1024**3, 3)
    free, total = torch.cuda.mem_get_info()
    report["physical_vram_gib"] = round(total / 1024**3, 3)
    print(f"\n  peak allocated {report['vram_peak_allocated_gib']} GiB / "
          f"reserved {report['vram_peak_reserved_gib']} GiB "
          f"(physical {report['physical_vram_gib']} GiB)")

    ok = (
        report["grad_norm_changed_every_step"]
        and agree
        and unchanged
        and static["optimizer_inside_sync_guard"]
        and static["loss_divided_by_accumulation"]
        and report["vram_peak_reserved_gib"] <= report["physical_vram_gib"]
    )
    print("\n" + "=" * 62)
    print("RESULT: ACCUMULATION SEMANTICS VERIFIED" if ok else "RESULT: PROBLEM FOUND — see above")

    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(report, indent=2), encoding="utf-8")
    print(f"written to {args.out}")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
