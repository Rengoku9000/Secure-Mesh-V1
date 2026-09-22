#!/usr/bin/env python3
"""Downloads the HF-format Qwen2.5-1.5B-Instruct for fine-tuning.

# Training model is not the production model

The quantised GGUF under `ai/models/llm/` is what the running SecureMesh
application serves. It is an inference format and cannot be fine-tuned. This
fetches the original Hugging Face weights into a **separate** tree under
`training/models/`, and refuses to write anywhere near `ai/models/`.

Those two copies are deliberately distinct artefacts: the production one is
provisioned and hash-verified by an operator (`docs/ai/PROVISIONING.md`), the
training one is an input to a build step. Nothing here converts, replaces, or
touches the production file.

# What is fetched

Only the files PEFT/QLoRA needs: config, generation config, tokenizer, and
safetensors weights. GGUF variants, ONNX exports and PyTorch `.bin` mirrors
in the same repository are excluded — they would double the download for no
use here.
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

SCRIPT_DIR = Path(__file__).resolve().parent
TRAINING_ROOT = SCRIPT_DIR.parent
REPO_ROOT = TRAINING_ROOT.parent

REPO_ID = "Qwen/Qwen2.5-1.5B-Instruct"
DEFAULT_DEST = TRAINING_ROOT / "models" / "Qwen2.5-1.5B-Instruct"

# Everything PEFT needs to construct the model and tokenizer, and nothing else.
ALLOW_PATTERNS = [
    "config.json",
    "generation_config.json",
    "model.safetensors",
    "model-*.safetensors",
    "model.safetensors.index.json",
    "tokenizer.json",
    "tokenizer_config.json",
    "vocab.json",
    "merges.txt",
    "added_tokens.json",
    "special_tokens_map.json",
]

PRODUCTION_MODELS = REPO_ROOT / "ai" / "models"


def assert_not_production(destination: Path) -> None:
    """Refuses any destination inside the production model tree.

    A guard rather than a convention: the production GGUF is a provisioned,
    hash-verified asset, and a download that landed on top of it would be
    both destructive and hard to notice.
    """
    resolved = destination.resolve()
    production = PRODUCTION_MODELS.resolve()
    if resolved == production or production in resolved.parents:
        raise SystemExit(
            f"REFUSING: {resolved} is inside the production model tree {production}. "
            "The training copy must stay separate."
        )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--dest", type=Path, default=DEFAULT_DEST)
    parser.add_argument("--repo-id", default=REPO_ID)
    args = parser.parse_args()

    assert_not_production(args.dest)

    from huggingface_hub import snapshot_download

    print(f"repo:        {args.repo_id}")
    print(f"destination: {args.dest}")
    print(f"patterns:    {ALLOW_PATTERNS}")
    print()

    args.dest.mkdir(parents=True, exist_ok=True)
    path = snapshot_download(
        repo_id=args.repo_id,
        local_dir=str(args.dest),
        allow_patterns=ALLOW_PATTERNS,
    )
    print(f"\ndownloaded to {path}")

    files = sorted(p for p in args.dest.rglob("*") if p.is_file())
    total = sum(p.stat().st_size for p in files)
    print(f"{len(files)} files, {total / 1024**3:.2f} GiB")
    return 0


if __name__ == "__main__":
    sys.exit(main())
