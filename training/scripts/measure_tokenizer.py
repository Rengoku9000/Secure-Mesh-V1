#!/usr/bin/env python3
"""Measures real Qwen2.5 token lengths over the training and validation splits.

Answers one question with authority: **is `max_seq_length = 1024` enough?**

Every earlier answer was a character-based bound. This one uses the actual
Qwen2.5 tokenizer, read from the local production GGUF, and the exact prompt
construction `finetune.py` uses — same system prompt, same fencing, same chat
template, same assistant boundary. Nothing is estimated.

Reads only. It does not train, does not touch the test split, does not modify
the GGUF, and writes a single JSON report under `training/results/`.
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

SCRIPT_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPT_DIR))

import preprocessing as pp  # noqa: E402
from finetune import build_example  # noqa: E402  the exact builder training uses
from gguf_tokenizer import load_tokenizer_from_gguf  # noqa: E402

TRAINING_ROOT = SCRIPT_DIR.parent
REPO_ROOT = TRAINING_ROOT.parent
PROCESSED = TRAINING_ROOT / "data" / "processed"
DEFAULT_GGUF = REPO_ROOT / "ai" / "models" / "llm" / "qwen2.5-1.5b-instruct-q4_k_m.gguf"


def percentile(sorted_values: list[int], fraction: float) -> int:
    if not sorted_values:
        return 0
    index = min(len(sorted_values) - 1, int(round(fraction * (len(sorted_values) - 1))))
    return sorted_values[index]


def load_split(name: str) -> list[dict]:
    path = PROCESSED / f"{name}.jsonl"
    return [
        json.loads(line)
        for line in path.read_text(encoding="utf-8").splitlines()
        if line.strip()
    ]


def measure(tokenizer, records: list[dict], split: str, limit: int) -> dict:
    """Tokenizes every record, enforcing the prefix and masking invariants."""
    rows = []
    prefix_failures = []
    masking_failures = []

    for record in records:
        example = build_example(record)
        try:
            # max_seq_length is deliberately huge here: measure first, judge
            # after, so an over-length record is reported rather than raised.
            tokenized = pp.tokenize_with_masking(
                tokenizer,
                example["system"],
                example["user"],
                example["assistant"],
                max_seq_length=10**9,
                example_id=example["id"],
            )
        except pp.PreprocessingError as error:
            prefix_failures.append({"id": example["id"], "error": str(error)})
            continue

        # Re-verify the masking invariants on every record, not just a sample.
        prompt_labels = tokenized.labels[: tokenized.prompt_tokens]
        target_labels = tokenized.labels[tokenized.prompt_tokens :]
        if not all(label == pp.IGNORE_INDEX for label in prompt_labels):
            masking_failures.append({"id": example["id"], "problem": "prompt not fully masked"})
        elif any(label == pp.IGNORE_INDEX for label in target_labels):
            masking_failures.append({"id": example["id"], "problem": "target partially masked"})
        elif target_labels != tokenized.input_ids[tokenized.prompt_tokens :]:
            masking_failures.append({"id": example["id"], "problem": "labels misaligned"})

        rows.append(
            {
                "id": example["id"],
                "scenario_group": record["scenario_group"],
                "total": tokenized.total_tokens,
                "prompt": tokenized.prompt_tokens,
                "target": tokenized.target_tokens,
            }
        )

    totals = sorted(row["total"] for row in rows)
    longest = max(rows, key=lambda row: row["total"]) if rows else None
    over = [row for row in rows if row["total"] > limit]

    return {
        "split": split,
        "records": len(records),
        "measured": len(rows),
        "max_total_tokens": totals[-1] if totals else 0,
        "min_total_tokens": totals[0] if totals else 0,
        "mean_total_tokens": round(sum(totals) / len(totals)) if totals else 0,
        "median_total_tokens": percentile(totals, 0.50),
        "p95_total_tokens": percentile(totals, 0.95),
        "p99_total_tokens": percentile(totals, 0.99),
        "max_prompt_tokens": max((row["prompt"] for row in rows), default=0),
        "min_prompt_tokens": min((row["prompt"] for row in rows), default=0),
        "max_target_tokens": max((row["target"] for row in rows), default=0),
        "mean_target_tokens": round(sum(row["target"] for row in rows) / len(rows)) if rows else 0,
        "longest_example": longest,
        "over_limit_count": len(over),
        "over_limit": [
            {"id": row["id"], "total": row["total"], "scenario_group": row["scenario_group"]}
            for row in over
        ],
        "prefix_assertion_failures": prefix_failures,
        "masking_failures": masking_failures,
        "rows": rows,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--gguf", type=Path, default=DEFAULT_GGUF)
    parser.add_argument(
        "--hf",
        type=Path,
        help="Measure with an HF-format checkout instead of the GGUF. Used in "
        "Phase 3F to confirm the acquired weights tokenize identically.",
    )
    parser.add_argument("--limit", type=int, default=1024, help="max_seq_length being tested")
    parser.add_argument("--out", type=Path, default=TRAINING_ROOT / "results" / "tokenizer-measurement.json")
    args = parser.parse_args()

    if args.hf:
        from transformers import AutoTokenizer
        from gguf_tokenizer import verify_qwen_tokenizer

        print(f"=== Loading the Qwen2.5 tokenizer from the HF checkout: {args.hf} ===")
        tokenizer = AutoTokenizer.from_pretrained(str(args.hf), local_files_only=True)
        info = verify_qwen_tokenizer(tokenizer)
        info["chat_template_source"] = "HF tokenizer_config.json"
        info["gguf_sha256_matches_readme"] = "n/a (HF source)"
        info["source"] = str(args.hf)
    else:
        print("=== Loading the real Qwen2.5 tokenizer (read-only, from the local GGUF) ===")
        tokenizer, info = load_tokenizer_from_gguf(args.gguf)
    pp.configure_tokenizer(tokenizer)
    print(f"  class:          {info['tokenizer_class']}")
    print(f"  vocab size:     {info['vocab_size']}")
    print(f"  chat template:  {info['chat_template_source']}")
    print(f"  GGUF hash matches ai/models/README.md: {info['gguf_sha256_matches_readme']}")
    print(f"  pad token:      {tokenizer.pad_token!r} (id {tokenizer.pad_token_id})")

    results = {}
    for split in ("train", "val"):
        records = load_split(split)
        print(f"\n=== Measuring {split} ({len(records)} records) ===")
        result = measure(tokenizer, records, split, args.limit)
        results[split] = result
        print(f"  max total    {result['max_total_tokens']}")
        print(f"  p99 / p95    {result['p99_total_tokens']} / {result['p95_total_tokens']}")
        print(f"  median/mean  {result['median_total_tokens']} / {result['mean_total_tokens']}")
        print(f"  max prompt   {result['max_prompt_tokens']}   max target {result['max_target_tokens']}")
        print(f"  over {args.limit}:     {result['over_limit_count']}")
        print(f"  prefix assertion failures: {len(result['prefix_assertion_failures'])}")
        print(f"  masking failures:          {len(result['masking_failures'])}")

    combined = results["train"]["rows"] + results["val"]["rows"]
    totals = sorted(row["total"] for row in combined)
    overall_longest = max(combined, key=lambda row: row["total"])
    over_all = [row for row in combined if row["total"] > args.limit]

    summary = {
        "limit_tested": args.limit,
        "tokenizer": {k: v for k, v in info.items() if k != "special_token_ids"},
        "special_token_ids": info["special_token_ids"],
        "train_records": results["train"]["records"],
        "val_records": results["val"]["records"],
        "combined_records": len(combined),
        "max_total_tokens": totals[-1],
        "max_prompt_tokens": max(row["prompt"] for row in combined),
        "max_target_tokens": max(row["target"] for row in combined),
        "p95_total_tokens": percentile(totals, 0.95),
        "p99_total_tokens": percentile(totals, 0.99),
        "median_total_tokens": percentile(totals, 0.50),
        "mean_total_tokens": round(sum(totals) / len(totals)),
        "over_limit_count": len(over_all),
        "over_limit_ids": [row["id"] for row in over_all],
        "longest_example": overall_longest,
        "prefix_assertion_failures": (
            results["train"]["prefix_assertion_failures"] + results["val"]["prefix_assertion_failures"]
        ),
        "masking_failures": results["train"]["masking_failures"] + results["val"]["masking_failures"],
        "headroom_tokens": args.limit - totals[-1],
        "per_split": {
            split: {k: v for k, v in result.items() if k != "rows"} for split, result in results.items()
        },
    }

    print("\n" + "=" * 62)
    print(f"COMBINED train+val: {summary['combined_records']} records")
    print(f"  max total tokens   {summary['max_total_tokens']}  (limit {args.limit}, headroom {summary['headroom_tokens']})")
    print(f"  max prompt tokens  {summary['max_prompt_tokens']}")
    print(f"  max target tokens  {summary['max_target_tokens']}")
    print(f"  p95 / p99          {summary['p95_total_tokens']} / {summary['p99_total_tokens']}")
    print(f"  records over {args.limit}: {summary['over_limit_count']}")
    print(f"  longest: {overall_longest['id']} ({overall_longest['total']} tokens)")
    print(f"           scenario_group = {overall_longest['scenario_group']}")

    ok = (
        summary["over_limit_count"] == 0
        and not summary["prefix_assertion_failures"]
        and not summary["masking_failures"]
    )
    print()
    if ok:
        print(f"RESULT: {args.limit} VERIFIED SAFE")
    else:
        print(f"RESULT: {args.limit} NOT SAFE — see over_limit_ids / failures above")

    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(summary, indent=2), encoding="utf-8")
    print(f"\nwritten to {args.out}")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
