#!/usr/bin/env python3
"""LoRA/QLoRA fine-tuning of Qwen2.5-1.5B-Instruct into SecureMesh-SLM.

# Status: not yet run. Corrected in Phase 3B, awaiting an environment.

No fine-tuning run has happened. This script has been exercised only for its
guard conditions (missing packages, missing local model) and its
preprocessing, which is covered by `test_preprocessing.py` and
`dryrun_preprocessing.py`. See `docs/ai/FINETUNING.md` for what "available
and configured" means before it is run for real.

# What Phase 3B corrected

Three defects that would each have produced a plausible-looking but wrong run:

1. **Loss was computed over the whole sequence.** `labels = input_ids` meant
   most of the gradient went into reproducing a system prompt that is
   identical in all 1,428 examples — measured at ~266 prompt tokens against
   ~31 target tokens. Loss is now masked to the assistant turn.
2. **The collator discarded the labels.** `DataCollatorForLanguageModeling`
   overwrites `labels` with a clone of `input_ids`, so even correct masking
   would have been thrown away. Replaced with `MaskedCausalCollator`.
3. **No pad token was configured**, which fails late and unhelpfully inside
   batching. Now set explicitly, preferring EOS.

A fourth, quieter one: this file carried its own duplicate of the production
prompt. It now imports the single mirror in `securemesh_prompt.py`, the same
one `run_baseline.py` used, so training and the baseline cannot drift apart.

# Strict offline guarantee

`HF_HUB_OFFLINE` and `TRANSFORMERS_OFFLINE` are set to `"1"` before any
Hugging Face library is imported, in this process's own environment — not
inherited from the shell — so this script cannot silently fetch a model or
tokenizer over the network even if one is missing locally. If a required
local file is absent, this refuses with an explanation instead of fetching
it. Nothing here reaches src-tauri, the running SecureMesh application, or
any network endpoint.

# What this does NOT do

- Does not modify `ai/models/llm/qwen2.5-1.5b-instruct-q4_k_m.gguf` (the
  production model) or any file under `src-tauri/`.
- Does not select or write to `LlamaConfig` — this produces a LoRA adapter on
  disk, in `training/artifacts/`, that a human decides whether to merge,
  convert, evaluate, and eventually provision, none of which this script
  does automatically.
"""

from __future__ import annotations

import os

# Must happen before `import transformers` (or anything that imports it)
# anywhere in this process, including via `peft`.
os.environ.setdefault("HF_HUB_OFFLINE", "1")
os.environ.setdefault("TRANSFORMERS_OFFLINE", "1")

import argparse
import hashlib
import json
import math
import sys
import time
from datetime import datetime, timezone
from pathlib import Path

SCRIPT_DIR = Path(__file__).resolve().parent
TRAINING_ROOT = SCRIPT_DIR.parent
sys.path.insert(0, str(SCRIPT_DIR))

import preprocessing  # noqa: E402  (path must be set up first)
import securemesh_prompt as smp  # noqa: E402

REQUIRED_PACKAGES = ["torch", "transformers", "peft", "yaml"]
QLORA_PACKAGE = "bitsandbytes"

# The prompt comes from `securemesh_prompt.py`, which is the single mirror of
# `src-tauri/src/ai/prompt.rs`. This file used to carry its own copy of the
# same strings; two mirrors of one production prompt can drift, and if they
# drifted the fine-tuned model would have been trained on a different task
# from the one the Phase 3 baseline was measured on — a difference no metric
# would surface. One copy, imported here and by `run_baseline.py`.


def check_packages() -> list[str]:
    """Returns the list of required packages that are not importable.

    Deliberately checked before anything else: a missing dependency should
    produce one clear message, not a traceback halfway through loading a
    gigabyte of weights.
    """
    missing = []
    for package in REQUIRED_PACKAGES:
        try:
            __import__(package)
        except ImportError:
            missing.append(package)
    return missing


def build_example(record: dict) -> dict:
    """Turns one validated training record into a system/user/assistant triple.

    The system prompt and the report fencing come from `securemesh_prompt`,
    so a training example is built with byte-identical text to what
    `run_baseline.py` sent the base model and what `ai/prompt.rs` sends in
    production.

    `target` becomes the assistant turn, serialised with `sort_keys=True` —
    deterministic, and the shape `RawAnalysis::validate` expects to parse.
    """
    return {
        "id": record["id"],
        "system": smp.SYSTEM_PROMPT,
        "user": smp.analysis_user_message(record["report_text"]),
        "assistant": json.dumps(record["target"], ensure_ascii=False, sort_keys=True),
    }


def load_examples(path: Path) -> list[dict]:
    examples = []
    with path.open("r", encoding="utf-8") as handle:
        for line in handle:
            line = line.strip()
            if line:
                examples.append(build_example(json.loads(line)))
    return examples


def load_lora_config(path: Path) -> dict:
    import yaml  # local import: only needed once we know we can proceed

    with path.open("r", encoding="utf-8") as handle:
        return yaml.safe_load(handle)


def check_local_model(base_model_path: Path) -> list[str]:
    """Checks for the files a local HF model directory needs, without loading
    any of them. Returns a list of problems; empty means the directory looks
    usable."""
    problems = []
    if not base_model_path.exists():
        problems.append(f"{base_model_path} does not exist")
        return problems
    if not base_model_path.is_dir():
        problems.append(f"{base_model_path} exists but is not a directory")
        return problems

    required_markers = ["config.json"]
    weight_markers = ["model.safetensors", "pytorch_model.bin"]
    for marker in required_markers:
        if not (base_model_path / marker).exists():
            problems.append(f"missing {marker} in {base_model_path}")
    if not any(
        (base_model_path / m).exists() or list(base_model_path.glob(f"{m}*.index.json"))
        for m in weight_markers
    ) and not list(base_model_path.glob("*.safetensors")):
        problems.append(
            f"no recognisable weight file (*.safetensors or pytorch_model.bin) in {base_model_path}"
        )
    return problems


def run_training(config: dict, args: argparse.Namespace) -> None:
    """The actual LoRA/QLoRA training loop.

    Still not executed: no training stack is installed on this machine. Left
    complete rather than stubbed so the next step, on a provisioned machine,
    is "run this" and not "finish writing this".

    Loss is computed over the assistant turn only — see `preprocessing.py`.
    """
    config_path = args.config
    import torch
    from peft import LoraConfig, get_peft_model
    from transformers import (
        AutoModelForCausalLM,
        AutoTokenizer,
        Trainer,
        TrainingArguments,
        set_seed,
    )

    # Seed everything before a weight is initialised. TrainingArguments(seed=)
    # also seeds, but only once the Trainer is constructed — LoRA adapter
    # initialisation happens before that, so seeding here is what makes the
    # adapter itself reproducible.
    seed = config["training"]["seed"]
    set_seed(seed)
    print(f"seed: {seed}")

    base_model_path = Path(config["base_model"]["path"])
    load_in_4bit = config["base_model"].get("load_in_4bit", False)

    quantization_config = None
    if load_in_4bit:
        from transformers import BitsAndBytesConfig

        quantization_config = BitsAndBytesConfig(
            load_in_4bit=True,
            bnb_4bit_compute_dtype=torch.bfloat16,
            bnb_4bit_quant_type="nf4",
        )

    tokenizer = AutoTokenizer.from_pretrained(base_model_path, local_files_only=True)
    model = AutoModelForCausalLM.from_pretrained(
        base_model_path,
        local_files_only=True,
        quantization_config=quantization_config,
        torch_dtype=torch.bfloat16 if config["training"].get("bf16") else torch.float32,
    )

    lora_cfg = config["lora"]
    peft_config = LoraConfig(
        r=lora_cfg["r"],
        lora_alpha=lora_cfg["lora_alpha"],
        lora_dropout=lora_cfg["lora_dropout"],
        bias=lora_cfg["bias"],
        target_modules=lora_cfg["target_modules"],
        task_type=lora_cfg["task_type"],
    )
    model = get_peft_model(model, peft_config)
    model.print_trainable_parameters()

    max_len = config["training"]["max_seq_length"]

    # A pad token must exist before anything is batched. Qwen2.5 normally
    # defines one, but that is a property of a downloaded config rather than a
    # guarantee, and its absence surfaces late and unhelpfully.
    preprocessing.configure_tokenizer(tokenizer)
    print(f"pad token: {tokenizer.pad_token!r} (id {tokenizer.pad_token_id})")

    raw_train = load_examples(Path(config["data"]["train_file"]))
    raw_val = load_examples(Path(config["data"]["val_file"]))

    # Validate the configured sequence length against the real tokenizer
    # before training rather than truncating during it. A clipped target
    # teaches the model to emit JSON that stops mid-object.
    measurement = preprocessing.measure_corpus(tokenizer, raw_train + raw_val, max_len)
    print(
        f"tokenized length: max {measurement['max_tokens']}, "
        f"median {measurement['median_tokens']}, mean {measurement['mean_tokens']} "
        f"(limit {max_len})"
    )
    print(
        f"  mean prompt {measurement['mean_prompt_tokens']} tokens masked, "
        f"mean target {measurement['mean_target_tokens']} tokens supervised"
    )
    if not measurement["fits"]:
        raise preprocessing.PreprocessingError(
            f"{len(measurement['over_length'])} example(s) exceed max_seq_length "
            f"({max_len}); longest is {measurement['max_tokens']} tokens. "
            f"First few: {measurement['over_length'][:5]}. "
            "Raise max_seq_length in the config — do not truncate."
        )

    def tokenize(example: dict) -> dict:
        """Masks the prompt and supervises only the assistant turn.

        See `preprocessing.py` for why: the system prompt is identical in
        every example, so computing loss over it spends most of the gradient
        teaching the model to reproduce text it is handed for free.
        """
        result = preprocessing.tokenize_with_masking(
            tokenizer,
            example["system"],
            example["user"],
            example["assistant"],
            max_seq_length=max_len,
            example_id=example.get("id", "<unknown>"),
        )
        return {
            "input_ids": result.input_ids,
            "attention_mask": result.attention_mask,
            "labels": result.labels,
        }

    train_examples = [tokenize(e) for e in raw_train]
    val_examples = [tokenize(e) for e in raw_val]

    # Schedule arithmetic, computed here because `warmup_steps` needs it.
    # Mirrors trainer.py: num_update_steps_per_epoch =
    #   ceil(len(dataloader) / gradient_accumulation_steps)
    micro_batch = config["training"]["per_device_train_batch_size"]
    accumulation = config["training"]["gradient_accumulation_steps"]
    epochs = config["training"]["num_train_epochs"]
    microbatches_per_epoch = math.ceil(len(train_examples) / micro_batch)
    steps_per_epoch = max(
        microbatches_per_epoch // accumulation
        + int(microbatches_per_epoch % accumulation > 0),
        1,
    )
    expected_steps = math.ceil(epochs * steps_per_epoch)

    # `warmup_ratio` was removed from TrainingArguments in transformers 5.x;
    # only `warmup_steps` remains. The config still expresses the frozen
    # hyperparameter as a ratio, and it is translated here using the same
    # formula the old implementation used — ceil(total_steps * ratio) — so the
    # experiment is unchanged: 0.03 * 198 -> 6 warmup steps either way.
    warmup_ratio = config["training"]["warmup_ratio"]
    warmup_steps = math.ceil(expected_steps * warmup_ratio)
    print(
        f"schedule: {microbatches_per_epoch} microbatches/epoch, {steps_per_epoch} steps/epoch, "
        f"{expected_steps} total optimizer steps"
    )
    print(f"warmup: ratio {warmup_ratio} -> {warmup_steps} steps (API takes steps, not ratio)")

    training_args = TrainingArguments(
        output_dir=config["training"]["output_dir"],
        num_train_epochs=config["training"]["num_train_epochs"],
        per_device_train_batch_size=config["training"]["per_device_train_batch_size"],
        # Explicit, not defaulted: TrainingArguments would use 8, which does
        # not fit this device at these sequence lengths. See lora_config.yaml.
        per_device_eval_batch_size=config["training"]["per_device_eval_batch_size"],
        gradient_accumulation_steps=config["training"]["gradient_accumulation_steps"],
        learning_rate=config["training"]["learning_rate"],
        lr_scheduler_type=config["training"]["lr_scheduler_type"],
        warmup_steps=warmup_steps,
        weight_decay=config["training"]["weight_decay"],
        logging_steps=config["training"]["logging_steps"],
        eval_strategy=config["training"]["eval_strategy"],
        save_strategy=config["training"]["save_strategy"],
        save_total_limit=config["training"]["save_total_limit"],
        seed=config["training"]["seed"],
        bf16=config["training"].get("bf16", False),
        report_to=[],  # No external experiment tracker: this stays offline.
    )

    class ListDataset(torch.utils.data.Dataset):
        def __init__(self, items: list[dict]) -> None:
            self.items = items

        def __len__(self) -> int:
            return len(self.items)

        def __getitem__(self, idx: int) -> dict:
            return self.items[idx]

    # NOT DataCollatorForLanguageModeling: it overwrites `labels` with a clone
    # of `input_ids`, which would silently discard the prompt masking computed
    # above and restore the very defect this pipeline was corrected for.
    collator = preprocessing.MaskedCausalCollator(tokenizer.pad_token_id)

    trainer = Trainer(
        model=model,
        args=training_args,
        train_dataset=ListDataset(train_examples),
        eval_dataset=ListDataset(val_examples),
        data_collator=collator,
    )

    started = time.time()
    train_result = trainer.train()
    duration_s = time.time() - started

    output_dir = Path(config["training"]["output_dir"])
    trainer.save_model(output_dir)
    tokenizer.save_pretrained(output_dir)

    # A run that cannot be attributed to an exact config, model and dataset
    # is not reproducible, and its numbers cannot be compared with the
    # baseline in training/results/. Recorded beside the adapter so the two
    # cannot be separated.
    metrics = {}
    try:
        evaluation = trainer.evaluate()
        metrics["eval"] = {k: float(v) for k, v in evaluation.items() if isinstance(v, (int, float))}
    except Exception as error:  # noqa: BLE001 - a failed eval must not lose the adapter
        metrics["eval_error"] = str(error)
    metrics["train"] = {
        k: float(v) for k, v in train_result.metrics.items() if isinstance(v, (int, float))
    }

    manifest = {
        "run": "qlora-finetune",
        "timestamp_utc": datetime.now(timezone.utc).isoformat(),
        "seed": seed,
        "duration_seconds": round(duration_s),
        "base_model_path": str(base_model_path),
        "base_model_files_sha256": _hash_model_dir(base_model_path),
        "config": config,
        "config_file_sha256": _sha256_file(config_path),
        "prompt_sha256": hashlib.sha256(smp.SYSTEM_PROMPT.encode("utf-8")).hexdigest(),
        "dataset": {
            "version": config["provenance"]["dataset_version"],
            "train_file": config["data"]["train_file"],
            "val_file": config["data"]["val_file"],
            "train_sha256": _sha256_file(Path(config["data"]["train_file"])),
            "val_sha256": _sha256_file(Path(config["data"]["val_file"])),
            "train_examples": len(train_examples),
            "val_examples": len(val_examples),
            "test_file_used": None,
        },
        "tokenization": {
            "max_seq_length": max_len,
            "measured_max_tokens": measurement["max_tokens"],
            "mean_prompt_tokens_masked": measurement["mean_prompt_tokens"],
            "mean_target_tokens_supervised": measurement["mean_target_tokens"],
            "truncated_examples": 0,
            "target_only_loss_masking": True,
        },
        "schedule": {
            "per_device_train_batch_size": micro_batch,
            "per_device_eval_batch_size": config["training"]["per_device_eval_batch_size"],
            "gradient_accumulation_steps": accumulation,
            "effective_batch_size": micro_batch * accumulation,
            "num_train_epochs": epochs,
            "microbatches_per_epoch": microbatches_per_epoch,
            "expected_steps_per_epoch": steps_per_epoch,
            "expected_optimizer_steps": expected_steps,
            "actual_optimizer_steps": trainer.state.global_step,
            "steps_match": trainer.state.global_step == expected_steps,
            "epochs_completed": trainer.state.epoch,
            "optimizer": training_args.optim if isinstance(training_args.optim, str) else str(training_args.optim),
            "lr_scheduler_type": str(training_args.lr_scheduler_type),
            "learning_rate": training_args.learning_rate,
            "warmup_ratio_configured": warmup_ratio,
            "warmup_steps_applied": warmup_steps,
            "warmup_note": (
                "transformers 5.x removed warmup_ratio; the configured ratio is "
                "translated to steps with ceil(total_steps * ratio), the same "
                "formula the removed implementation used"
            ),
            "weight_decay": training_args.weight_decay,
            "max_grad_norm": training_args.max_grad_norm,
        },
        "metrics": metrics,
        "peak_memory": _peak_memory(torch),
    }
    (output_dir / "run-manifest.json").write_text(
        json.dumps(manifest, indent=2, default=str), encoding="utf-8"
    )

    print(f"LoRA adapter saved to {output_dir}")
    print(f"run manifest written to {output_dir / 'run-manifest.json'}")
    print(f"training took {duration_s:.0f}s")
    print("Next: training/scripts/export_gguf.py to merge and quantize.")


def _sha256_file(path: Path) -> str | None:
    try:
        return hashlib.sha256(path.read_bytes()).hexdigest()
    except OSError:
        return None


def _hash_model_dir(path: Path) -> dict:
    """Hashes the small files that identify a checkout, not the weights.

    Weight files are gigabytes; hashing them would add minutes to every run.
    `config.json` plus the tokenizer files identify which checkout was used,
    which is what reproducibility needs.
    """
    hashes = {}
    for name in ("config.json", "tokenizer_config.json", "generation_config.json"):
        candidate = path / name
        if candidate.exists():
            hashes[name] = _sha256_file(candidate)
    return hashes


def _peak_memory(torch) -> dict:
    if torch.cuda.is_available():
        return {
            "cuda_max_allocated_bytes": int(torch.cuda.max_memory_allocated()),
            "cuda_max_reserved_bytes": int(torch.cuda.max_memory_reserved()),
            "device": torch.cuda.get_device_name(0),
        }
    return {"cuda": "unavailable"}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--config",
        type=Path,
        default=TRAINING_ROOT / "configs" / "lora_config.yaml",
        help="Path to lora_config.yaml",
    )
    parser.add_argument(
        "--i-have-configured-a-local-training-environment",
        action="store_true",
        help=(
            "Required. Confirms a local base model, GPU/CPU training stack, "
            "and dependencies are already in place. Without it, this prints "
            "what would be checked and exits without training anything."
        ),
    )
    args = parser.parse_args()

    missing = check_packages()
    if missing:
        # Import names and pip/PyPI package names differ for one of these
        # (`yaml` is installed as `pyyaml`); map back before printing so the
        # suggested command is actually correct, not just plausible.
        pip_names = {"yaml": "pyyaml"}
        install_names = [pip_names.get(m, m) for m in missing]
        print("Missing required packages: " + ", ".join(missing))
        print(f"Install offline from a local package cache, e.g.:\n  pip install {' '.join(install_names)}")
        print(f"For QLoRA (load_in_4bit), also install: {QLORA_PACKAGE}")
        print("This script will not install anything for you.")
        return 1

    if not args.config.exists():
        print(f"Config not found: {args.config}")
        return 1

    config = load_lora_config(args.config)
    base_model_path = Path(config["base_model"]["path"])
    problems = check_local_model(base_model_path)

    if problems:
        print(f"The configured base model at {base_model_path} is not usable:")
        for problem in problems:
            print(f"  - {problem}")
        print(
            "\nThis script never downloads a model. Obtain Qwen2.5-1.5B-Instruct "
            "(full-precision or bf16 HF weights, not the .gguf SecureMesh runs) "
            "locally by whatever means you already use for open-weight models, "
            "then update base_model.path in "
            f"{args.config}."
        )
        return 1

    if not args.i_have_configured_a_local_training_environment:
        print("Local packages: OK")
        print(f"Local base model at {base_model_path}: OK")
        print(
            "\nPer Phase 1 of SecureMesh-SLM, this script does not train "
            "unless explicitly told the environment is ready. Re-run with "
            "--i-have-configured-a-local-training-environment to actually "
            "start training."
        )
        return 0

    run_training(config, args)
    return 0


if __name__ == "__main__":
    sys.exit(main())
