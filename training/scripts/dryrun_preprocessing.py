#!/usr/bin/env python3
"""Dry-run of the training preprocessing. Loads and checks; never trains.

Answers the questions that decide whether a fine-tuning run is safe to start:

1. Does the configured dataset load, and is it the frozen v2 corpus?
2. Are the prompt/target boundary and `-100` masking correct?
3. Is padding inert?
4. How long is the longest tokenized example, against `max_seq_length`?
5. Is the test set kept out of training?

**No model is loaded and no training step is executed**, even when the full
stack is installed.

# Two modes, and the difference is reported honestly

- **Real tokenizer** (`transformers` installed and a local model path
  configured): every example is tokenized, so the reported lengths are the
  ones training will actually see.
- **Static / stub** (the default on a machine without the training stack):
  the same preprocessing functions run against a deterministic stub
  tokenizer. This verifies *logic* — boundary, masking, padding — but the
  token counts are word-level, not BPE, and are reported as **not
  authoritative**. A character-based bound is given instead, with the
  assumption it rests on stated plainly.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import sys
from pathlib import Path

SCRIPT_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPT_DIR))

import preprocessing as pp  # noqa: E402
import securemesh_prompt as smp  # noqa: E402

TRAINING_ROOT = SCRIPT_DIR.parent
PROCESSED = TRAINING_ROOT / "data" / "processed"
CONFIG_PATH = TRAINING_ROOT / "configs" / "lora_config.yaml"


def read_config_value(text: str, key: str) -> str | None:
    """Minimal YAML scalar lookup, so a dry run needs no PyYAML."""
    for line in text.splitlines():
        stripped = line.strip()
        if stripped.startswith(f"{key}:"):
            value = stripped.split(":", 1)[1].split("#", 1)[0].strip()
            return value.strip('"').strip("'")
    return None


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def build_examples(split: str) -> list[dict]:
    """Builds training triples exactly as finetune.py does."""
    rows = [
        json.loads(line)
        for line in (PROCESSED / f"{split}.jsonl").read_text(encoding="utf-8").splitlines()
        if line.strip()
    ]
    return [
        {
            "id": row["id"],
            "system": smp.SYSTEM_PROMPT,
            "user": smp.analysis_user_message(row["report_text"]),
            "assistant": json.dumps(row["target"], ensure_ascii=False, sort_keys=True),
        }
        for row in rows
    ]


def try_real_tokenizer(config_text: str, gguf_path: Path | None = None):
    """Returns a real tokenizer, or None with the reason printed.

    Two sources, in order of preference:

    1. `--gguf`: the Qwen2.5 tokenizer embedded in the local production GGUF.
       This is what makes a real measurement possible before the Hugging Face
       weights exist on the machine — the tokenizer is already here, inside
       the file the running application serves.
    2. `base_model.path` from the config, once an HF checkout is provisioned.

    Both give the same tokenizer; the GGUF route simply gets there without a
    download.
    """
    try:
        import transformers  # noqa: F401
    except ImportError:
        print("  transformers is NOT installed — tokenization cannot be executed")
        return None

    if gguf_path is not None:
        if not gguf_path.exists():
            print(f"  --gguf given but not found: {gguf_path}")
            return None
        try:
            from gguf_tokenizer import load_tokenizer_from_gguf

            print(f"  loading the Qwen2.5 tokenizer from {gguf_path.name} (read-only)")
            tokenizer, info = load_tokenizer_from_gguf(gguf_path)
            print(f"  {info['tokenizer_class']}, vocab {info['vocab_size']}, "
                  f"chat template {info['chat_template_source']}")
            print(f"  GGUF hash matches ai/models/README.md: {info['gguf_sha256_matches_readme']}")
            return tokenizer
        except Exception as error:  # noqa: BLE001 - report, never silently fall back
            print(f"  GGUF tokenizer load FAILED: {type(error).__name__}: {error}")
            return None

    model_path = read_config_value(config_text, "path")
    if not model_path or model_path.startswith("PATH_TO_LOCAL"):
        print(f"  base_model.path is not configured ({model_path!r})")
        return None
    if not Path(model_path).exists():
        print(f"  base_model.path does not exist locally: {model_path}")
        return None

    from transformers import AutoTokenizer

    print(f"  loading tokenizer from {model_path} (local only)")
    return AutoTokenizer.from_pretrained(model_path, local_files_only=True)


def character_bound(examples: list[dict], max_seq_length: int) -> None:
    """A tokenizer-free bound on sequence length, with its assumption stated.

    Not a substitute for measuring. It answers one question: how bad would the
    tokenizer have to be for `max_seq_length` to be breached?
    """
    worst = max(len(e["system"]) + len(e["user"]) + len(e["assistant"]) for e in examples)
    worst_id = max(
        examples, key=lambda e: len(e["system"]) + len(e["user"]) + len(e["assistant"])
    )["id"]
    print(f"  longest example: {worst} characters ({worst_id})")
    for ratio in (4.0, 3.5, 3.0, 2.5):
        print(f"    at {ratio} chars/token -> ~{worst / ratio:.0f} tokens")
    breach = worst / max_seq_length
    print(
        f"  {max_seq_length} tokens would only be breached if the tokenizer averaged "
        f"worse than {breach:.2f} chars/token,"
    )
    print("    which would be anomalous for English prose plus JSON.")
    print("  This is a bound, not a measurement. finetune.py measures for real and")
    print("  refuses to start if any example exceeds the limit.")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--max-seq-length", type=int, help="Override the configured limit")
    parser.add_argument(
        "--gguf",
        type=Path,
        help="Load the real Qwen2.5 tokenizer from this local GGUF instead of "
        "base_model.path (read-only; needs no Hugging Face download)",
    )
    args = parser.parse_args()

    print("SecureMesh-SLM preprocessing dry run")
    print("=" * 62)
    print("No model is loaded. No training step is executed.\n")

    config_text = CONFIG_PATH.read_text(encoding="utf-8")
    configured_max = args.max_seq_length or int(read_config_value(config_text, "max_seq_length"))
    dataset_version = read_config_value(config_text, "dataset_version")
    seed = read_config_value(config_text, "seed")

    # --- 1. dataset integrity -------------------------------------------
    print("1. Dataset")
    manifest = json.loads((PROCESSED / "manifest.json").read_text(encoding="utf-8"))
    counts = {name: manifest["splits"][name]["count"] for name in ("train", "val", "test")}
    print(f"  dataset_version (config):   {dataset_version}")
    print(f"  dataset_version (manifest): {manifest['dataset_version']}")
    print(f"  split counts: {counts}")
    version_ok = dataset_version == manifest["dataset_version"] == "securemesh-slm-v2"
    counts_ok = counts == {"train": 1052, "val": 214, "test": 162}
    print(f"  version agrees: {version_ok}   counts frozen at 1052/214/162: {counts_ok}")
    for name in ("train", "val", "test"):
        print(f"    {name}.jsonl sha256 {sha256(PROCESSED / f'{name}.jsonl')[:32]}…")

    # --- 2. test-set exclusion -------------------------------------------
    print("\n2. Test-set exclusion")
    data_block = config_text.split("data:", 1)[1].split("provenance:", 1)[0]
    referenced = [
        line.strip() for line in data_block.splitlines() if "file:" in line
    ]
    for line in referenced:
        print(f"  config references: {line}")
    test_referenced = "test.jsonl" in data_block
    finetune_src = (SCRIPT_DIR / "finetune.py").read_text(encoding="utf-8")
    print(f"  config loads test.jsonl:      {test_referenced}")
    print(f"  finetune.py mentions test.jsonl: {'test.jsonl' in finetune_src}")

    train_examples = build_examples("train")
    val_examples = build_examples("val")
    test_ids = {
        json.loads(line)["id"]
        for line in (PROCESSED / "test.jsonl").read_text(encoding="utf-8").splitlines()
        if line.strip()
    }
    loaded_ids = {e["id"] for e in train_examples} | {e["id"] for e in val_examples}
    leaked = loaded_ids & test_ids
    print(f"  examples the training process would load: {len(loaded_ids)}")
    print(f"  of those, present in the test set: {len(leaked)}")

    # --- 3. tokenization --------------------------------------------------
    print("\n3. Tokenizer")
    tokenizer = try_real_tokenizer(config_text, args.gguf)
    real = tokenizer is not None
    if not real:
        print("  falling back to the deterministic stub tokenizer")
        print("  >>> TOKEN COUNTS BELOW ARE NOT AUTHORITATIVE (word-level, not BPE) <<<")
        from test_preprocessing import StubTokenizer

        tokenizer = StubTokenizer()

    pp.configure_tokenizer(tokenizer)
    print(f"  pad_token: {tokenizer.pad_token!r}  pad_token_id: {tokenizer.pad_token_id}")

    # --- 4. boundary and masking -----------------------------------------
    print("\n4. Prompt/target boundary and masking")
    sample = train_examples[0]
    tokenized = pp.tokenize_with_masking(
        tokenizer, sample["system"], sample["user"], sample["assistant"],
        max_seq_length=10**9, example_id=sample["id"],
    )
    prompt_labels = tokenized.labels[: tokenized.prompt_tokens]
    target_labels = tokenized.labels[tokenized.prompt_tokens :]
    print(f"  example: {sample['id']}")
    print(f"  prompt tokens masked:     {tokenized.prompt_tokens}")
    print(f"  target tokens supervised: {tokenized.target_tokens}")
    print(f"  all prompt labels == -100:        {all(x == pp.IGNORE_INDEX for x in prompt_labels)}")
    print(f"  no target label == -100:          {all(x != pp.IGNORE_INDEX for x in target_labels)}")
    print(
        f"  target labels match input_ids:    "
        f"{target_labels == tokenized.input_ids[tokenized.prompt_tokens:]}"
    )

    # --- 5. padding -------------------------------------------------------
    print("\n5. Padding")
    shortest = min(train_examples, key=lambda e: len(e["user"]))
    short = pp.tokenize_with_masking(
        tokenizer, shortest["system"], shortest["user"], shortest["assistant"],
        max_seq_length=10**9, example_id=shortest["id"],
    )
    batch = pp.pad_batch([short, tokenized], tokenizer.pad_token_id)
    padding = len(batch["input_ids"][0]) - len(short.input_ids)
    print(f"  batch padded to {len(batch['input_ids'][0])} tokens ({padding} added to the shorter row)")
    if padding:
        print(f"  padded input_ids use pad_token_id: {all(t == tokenizer.pad_token_id for t in batch['input_ids'][0][-padding:])}")
        print(f"  padded attention_mask is 0:        {all(m == 0 for m in batch['attention_mask'][0][-padding:])}")
        print(f"  padded labels are -100:            {all(l == pp.IGNORE_INDEX for l in batch['labels'][0][-padding:])}")

    # --- 6. sequence length ----------------------------------------------
    print(f"\n6. Sequence length (configured max_seq_length = {configured_max})")
    if real:
        measurement = pp.measure_corpus(tokenizer, train_examples + val_examples, configured_max)
        print(f"  MEASURED over {measurement['examples']} examples with the real tokenizer:")
        print(f"    max {measurement['max_tokens']}, median {measurement['median_tokens']}, mean {measurement['mean_tokens']}")
        print(f"    mean prompt {measurement['mean_prompt_tokens']} masked / target {measurement['mean_target_tokens']} supervised")
        if measurement["fits"]:
            print(f"    all examples fit within {configured_max} — no truncation needed")
        else:
            print(f"    {len(measurement['over_length'])} EXAMPLE(S) EXCEED {configured_max}:")
            for example_id, length in measurement["over_length"][:10]:
                print(f"      {example_id}: {length} tokens")
            print("    STOP: raise max_seq_length rather than truncating.")
    else:
        print("  Real tokenization unavailable; using a character-based bound instead.")
        character_bound(train_examples + val_examples, configured_max)

    # --- 7. reproducibility ----------------------------------------------
    print("\n7. Reproducibility")
    print(f"  seed in config: {seed}")
    print(f"  dataset order: sorted by id in the split files (build_dataset.py)")
    print(f"  preprocessing: pure functions, no randomness, no augmentation")
    first = pp.tokenize_with_masking(
        tokenizer, sample["system"], sample["user"], sample["assistant"],
        max_seq_length=10**9, example_id=sample["id"],
    )
    print(f"  repeat tokenization identical: {first.input_ids == tokenized.input_ids and first.labels == tokenized.labels}")

    # --- verdict ----------------------------------------------------------
    print("\n" + "=" * 62)
    checks = {
        "dataset version agrees and counts frozen": version_ok and counts_ok,
        "no test record loaded for training": not leaked and not test_referenced,
        "prompt fully masked": all(x == pp.IGNORE_INDEX for x in prompt_labels),
        "target fully supervised": all(x != pp.IGNORE_INDEX for x in target_labels),
        "padding inert": (not padding) or all(l == pp.IGNORE_INDEX for l in batch["labels"][0][-padding:]),
        "pad token configured": isinstance(tokenizer.pad_token_id, int),
    }
    for label, ok in checks.items():
        print(f"  {'OK  ' if ok else 'FAIL'}  {label}")
    if not real:
        print("\n  NOTE: tokenization was NOT executed with the real tokenizer.")
        print("        Sequence length remains unverified until the training stack")
        print("        and a local model are provisioned. finetune.py enforces it.")
    return 0 if all(checks.values()) else 1


if __name__ == "__main__":
    sys.exit(main())
