#!/usr/bin/env python3
"""Loads base + trained LoRA adapter and generates one structured response.

A load/generation sanity check, nothing more: does the adapter attach to the
base model, and does the result produce output the SecureMesh schema accepts?

**This is not an evaluation.** The input is a hand-written report belonging to
no split — not train, not validation, and emphatically not the frozen 162
test records. No accuracy is claimed and no comparison is made; the only
question is whether the artefact loads and speaks.
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
import securemesh_prompt as smp  # noqa: E402
import validate_dataset as vd  # noqa: E402
from finetune import load_lora_config  # noqa: E402

TRAINING_ROOT = SCRIPT_DIR.parent

# Written for this check. Deliberately not drawn from any split, so nothing
# about it can be mistaken for an evaluation result.
PROBE_REPORT = (
    "brdg on the mill rd has dropped abt a foot on the north side, cracks "
    "widening, we've stopped all veh traffic n put cones out. 2 ppl were "
    "walking across when it moved, both fine. need an engineer."
)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--adapter",
        type=Path,
        default=TRAINING_ROOT / "results" / "runs" / "securemesh-slm-v2-qlora-r1",
    )
    parser.add_argument("--config", type=Path, default=TRAINING_ROOT / "configs" / "lora_config.yaml")
    parser.add_argument("--out", type=Path)
    args = parser.parse_args()

    import torch
    from peft import PeftModel
    from transformers import AutoModelForCausalLM, AutoTokenizer, BitsAndBytesConfig

    config = load_lora_config(args.config)
    base_path = Path(config["base_model"]["path"])

    report: dict = {"adapter": str(args.adapter), "base_model": str(base_path)}

    print("=== 1. Adapter files ===")
    required = ["adapter_config.json", "adapter_model.safetensors"]
    for name in required:
        path = args.adapter / name
        exists = path.exists()
        size = path.stat().st_size if exists else 0
        print(f"  {'OK  ' if exists else 'MISS'}  {name}  ({size:,} bytes)")
        if not exists:
            raise SystemExit(f"missing {name}")

    adapter_config = json.loads((args.adapter / "adapter_config.json").read_text())
    print(f"  r={adapter_config.get('r')}  alpha={adapter_config.get('lora_alpha')}  "
          f"dropout={adapter_config.get('lora_dropout')}")
    print(f"  target_modules={sorted(adapter_config.get('target_modules', []))}")
    print(f"  base_model_name_or_path={adapter_config.get('base_model_name_or_path')}")
    report["adapter_config"] = {
        "r": adapter_config.get("r"),
        "lora_alpha": adapter_config.get("lora_alpha"),
        "lora_dropout": adapter_config.get("lora_dropout"),
        "target_modules": sorted(adapter_config.get("target_modules", [])),
    }

    print("\n=== 2. Loading base model (4-bit NF4) + adapter ===")
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

    model = PeftModel.from_pretrained(model, str(args.adapter), local_files_only=True)
    model.eval()

    adapted = sum(1 for name, _ in model.named_modules() if name.endswith("lora_A.default"))
    print(f"  adapter attached to {adapted} layers")
    report["adapted_layers"] = adapted
    if adapted == 0:
        raise SystemExit("adapter loaded but attached to no layers")

    # Prove the adapter is actually doing something: its B matrices start at
    # zero, so a trained adapter must have non-zero norms. All-zero would mean
    # the weights never updated, which would load fine and change nothing.
    b_norms = [
        float(p.detach().float().norm())
        for n, p in model.named_parameters() if "lora_B" in n
    ]
    nonzero = sum(1 for v in b_norms if v > 0)
    print(f"  lora_B tensors with non-zero norm: {nonzero}/{len(b_norms)}")
    print(f"  mean lora_B norm: {sum(b_norms)/len(b_norms):.4f}")
    report["lora_B_nonzero"] = f"{nonzero}/{len(b_norms)}"
    report["lora_B_mean_norm"] = round(sum(b_norms) / len(b_norms), 4)
    if nonzero == 0:
        raise SystemExit("every lora_B is zero — the adapter encodes no learned change")

    print("\n=== 3. One structured generation (input belongs to no split) ===")
    print(f"  report: {PROBE_REPORT[:90]}...")
    messages = smp.chat_messages(PROBE_REPORT)
    text = tokenizer.apply_chat_template(messages, tokenize=False, add_generation_prompt=True)
    inputs = tokenizer(text, add_special_tokens=False, return_tensors="pt").to("cuda")

    started = time.perf_counter()
    with torch.no_grad():
        generated = model.generate(
            **inputs,
            max_new_tokens=smp.ANALYSIS_TOKENS,
            do_sample=False,
            temperature=None,
            top_p=None,
            top_k=None,
            pad_token_id=tokenizer.pad_token_id,
        )
    latency_ms = int((time.perf_counter() - started) * 1000)

    completion = tokenizer.decode(
        generated[0][inputs["input_ids"].shape[1] :], skip_special_tokens=True
    ).strip()
    print(f"  generated in {latency_ms} ms, {len(completion)} chars")
    print(f"\n  raw output:\n{completion}")
    report["generation"] = {"latency_ms": latency_ms, "raw": completion}

    print("\n=== 4. Does it satisfy the SecureMesh schema? ===")
    try:
        parsed = json.loads(completion)
        print("  parses as JSON: True")
        errors = vd.validate_target(parsed)
        print(f"  schema errors: {errors if errors else 'none'}")
        for field in ("category", "severity", "access_status", "summary"):
            if field in parsed:
                value = parsed[field]
                shown = value if not isinstance(value, str) or len(value) < 80 else value[:80] + "..."
                print(f"    {field}: {shown}")
        report["generation"]["parsed"] = True
        report["generation"]["schema_errors"] = errors
        report["generation"]["target"] = parsed
        ok = not errors
    except json.JSONDecodeError as error:
        print(f"  parses as JSON: False ({error})")
        report["generation"]["parsed"] = False
        ok = False

    print("\n" + "=" * 62)
    print("RESULT: ADAPTER LOADS AND PRODUCES SCHEMA-VALID OUTPUT" if ok
          else "RESULT: adapter loaded but output did not satisfy the schema")
    print("(load/generation sanity check only — no accuracy claim, no test data)")

    if args.out:
        args.out.parent.mkdir(parents=True, exist_ok=True)
        args.out.write_text(json.dumps(report, indent=2), encoding="utf-8")
        print(f"written to {args.out}")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
