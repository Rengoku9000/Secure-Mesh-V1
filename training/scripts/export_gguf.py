#!/usr/bin/env python3
"""Merges a LoRA adapter and exports a quantized GGUF for SecureMesh-SLM.

# Phase 1 status: SCAFFOLD ONLY, not executed

Three steps, each requiring local tools this script never installs or
downloads:

1. Merge the LoRA adapter (from `finetune.py`) into the base model's weights,
   via `peft`'s `merge_and_unload()`, producing a full HF model directory.
2. Convert that merged model to GGUF (f16) using llama.cpp's own
   `convert_hf_to_gguf.py`, which must already exist locally (this project
   already runs llama.cpp — see `ai/models/README.md` — so a checkout or
   release tree is expected to be available, not fetched here).
3. Quantize the f16 GGUF to the target format (default `Q4_K_M`, matching the
   quantisation already used for the production model) with llama.cpp's
   `llama-quantize` binary.

# This does NOT touch the production model

Output goes to an explicit `--output-path` chosen by the operator, never to
`ai/models/llm/qwen2.5-1.5b-instruct-q4_k_m.gguf`. Deploying a new file to
that path — and updating `LlamaConfig` to point at it — is a separate,
later, human decision, documented in `docs/ai/FINETUNING.md`, not something
this script does.
"""

from __future__ import annotations

import argparse
import subprocess
import sys
import tempfile
from pathlib import Path


def check_llama_cpp_tools(llama_cpp_dir: Path) -> list[str]:
    problems = []
    convert_script = llama_cpp_dir / "convert_hf_to_gguf.py"
    if not convert_script.exists():
        problems.append(f"missing {convert_script}")

    quantize_bin_candidates = [
        llama_cpp_dir / "llama-quantize",
        llama_cpp_dir / "llama-quantize.exe",
        llama_cpp_dir / "build" / "bin" / "llama-quantize",
        llama_cpp_dir / "build" / "bin" / "llama-quantize.exe",
    ]
    if not any(p.exists() for p in quantize_bin_candidates):
        problems.append(
            f"no llama-quantize binary found under {llama_cpp_dir} "
            f"(checked: {', '.join(str(p) for p in quantize_bin_candidates)})"
        )
    return problems


def find_quantize_binary(llama_cpp_dir: Path) -> Path | None:
    for candidate in [
        llama_cpp_dir / "llama-quantize",
        llama_cpp_dir / "llama-quantize.exe",
        llama_cpp_dir / "build" / "bin" / "llama-quantize",
        llama_cpp_dir / "build" / "bin" / "llama-quantize.exe",
    ]:
        if candidate.exists():
            return candidate
    return None


def merge_adapter(base_model_path: Path, adapter_dir: Path, merged_out_dir: Path) -> None:
    """Merges a LoRA adapter into the base weights using peft.

    Imported lazily so `--check-only` and argument-parsing errors don't
    require torch/peft to be installed just to report themselves.
    """
    import torch
    from peft import PeftModel
    from transformers import AutoModelForCausalLM, AutoTokenizer

    print(f"Loading base model from {base_model_path} (local only)...")
    base_model = AutoModelForCausalLM.from_pretrained(
        base_model_path, local_files_only=True, torch_dtype=torch.bfloat16
    )
    tokenizer = AutoTokenizer.from_pretrained(base_model_path, local_files_only=True)

    print(f"Applying LoRA adapter from {adapter_dir}...")
    merged = PeftModel.from_pretrained(base_model, adapter_dir, local_files_only=True)
    merged = merged.merge_and_unload()

    merged_out_dir.mkdir(parents=True, exist_ok=True)
    merged.save_pretrained(merged_out_dir)
    tokenizer.save_pretrained(merged_out_dir)
    print(f"Merged model written to {merged_out_dir}")


def convert_to_gguf(llama_cpp_dir: Path, merged_dir: Path, f16_out_path: Path) -> None:
    convert_script = llama_cpp_dir / "convert_hf_to_gguf.py"
    cmd = [
        sys.executable,
        str(convert_script),
        str(merged_dir),
        "--outfile",
        str(f16_out_path),
        "--outtype",
        "f16",
    ]
    print("Running:", " ".join(cmd))
    subprocess.run(cmd, check=True)


def quantize(llama_cpp_dir: Path, f16_path: Path, quantized_out_path: Path, quant_type: str) -> None:
    quantize_bin = find_quantize_binary(llama_cpp_dir)
    if quantize_bin is None:
        raise FileNotFoundError(f"no llama-quantize binary found under {llama_cpp_dir}")
    cmd = [str(quantize_bin), str(f16_path), str(quantized_out_path), quant_type]
    print("Running:", " ".join(cmd))
    subprocess.run(cmd, check=True)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base-model-path", type=Path, required=True)
    parser.add_argument("--adapter-dir", type=Path, required=True)
    parser.add_argument(
        "--llama-cpp-dir",
        type=Path,
        required=True,
        help="Local llama.cpp checkout or release tree containing "
        "convert_hf_to_gguf.py and llama-quantize.",
    )
    parser.add_argument("--output-path", type=Path, required=True)
    parser.add_argument("--quant-type", default="Q4_K_M")
    parser.add_argument(
        "--check-only",
        action="store_true",
        help="Verify all required local tools and files are present, then exit "
        "without merging, converting, or quantizing anything.",
    )
    args = parser.parse_args()

    problems = []
    if not args.base_model_path.exists():
        problems.append(f"base model not found: {args.base_model_path}")
    if not args.adapter_dir.exists():
        problems.append(f"adapter directory not found: {args.adapter_dir}")
    problems.extend(check_llama_cpp_tools(args.llama_cpp_dir))

    if problems:
        print("Cannot proceed:")
        for problem in problems:
            print(f"  - {problem}")
        print(
            "\nThis script never downloads llama.cpp, a base model, or an "
            "adapter. Obtain llama.cpp locally the same way ai/runtime/ was "
            "provisioned (see docs/ai/PROVISIONING.md) and point "
            "--llama-cpp-dir at that checkout."
        )
        return 1

    print("All required local tools and files found.")
    if args.check_only:
        return 0

    if args.output_path.exists():
        print(f"Refusing to overwrite existing file: {args.output_path}")
        print("Choose a different --output-path, or remove it first yourself.")
        return 1

    with tempfile.TemporaryDirectory(prefix="securemesh-slm-export-") as tmp:
        tmp_path = Path(tmp)
        merged_dir = tmp_path / "merged"
        f16_path = tmp_path / "securemesh-slm-f16.gguf"

        merge_adapter(args.base_model_path, args.adapter_dir, merged_dir)
        convert_to_gguf(args.llama_cpp_dir, merged_dir, f16_path)

        args.output_path.parent.mkdir(parents=True, exist_ok=True)
        quantize(args.llama_cpp_dir, f16_path, args.output_path, args.quant_type)

    print(f"\nDone: {args.output_path}")
    print(
        "This file has NOT been copied to ai/models/llm/, and LlamaConfig has "
        "NOT been changed. Evaluate it first with evaluate.py / the existing "
        "Rust benchmark harness before deciding whether to deploy it — see "
        "docs/ai/FINETUNING.md."
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
