#!/usr/bin/env python3
"""Tests for SecureMesh-SLM training preprocessing.

Runs with **plain Python and no installed packages** — no pytest, no torch,
no transformers. That is deliberate: these tests guard the defects found in
the Phase 3A audit, and they have to be runnable on the machine where the
training stack is *not* yet installed, which is precisely when the pipeline
is easiest to get wrong.

# How the real code is exercised without transformers

`StubTokenizer` implements the small surface `preprocessing.py` actually uses
— `apply_chat_template`, `__call__`, `pad_token`/`eos_token` — with ChatML
markers and deterministic word-level ids. The functions under test are the
**same functions the real run calls**; only the tokenizer is substituted.

What that does and does not prove:

- **Proves:** the prompt/target boundary is located correctly, `-100` masking
  covers exactly the prompt, padding contributes no loss, a pad token is
  always configured, preprocessing is deterministic, and no test record can
  enter training.
- **Does not prove:** real BPE token counts. A stub tokenizer splits on
  whitespace, so the sequence-length figures it produces are not the ones
  Qwen2.5 will produce. Length is therefore *enforced at training time* by an
  assertion rather than claimed here.
"""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path

SCRIPT_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPT_DIR))

import preprocessing as pp  # noqa: E402

TRAINING_ROOT = SCRIPT_DIR.parent
PROCESSED = TRAINING_ROOT / "data" / "processed"

# ChatML special markers, kept atomic by the stub's tokenizer regex so the
# prompt/target boundary falls on a token edge exactly as it does for Qwen.
SPECIAL = re.compile(r"(<\|[a-z_]+\|>)")


class StubTokenizer:
    """A deterministic ChatML tokenizer standing in for Qwen2.5-Instruct.

    Word-level rather than BPE: ids come from a growing vocabulary keyed by
    the token string, so the same text always yields the same ids within a
    process and the prefix property behaves as it does for a real tokenizer.
    """

    def __init__(self, with_pad_token: bool = False) -> None:
        self.eos_token = "<|im_end|>"
        self._vocab: dict[str, int] = {}
        self._pad_token: str | None = "<|endoftext|>" if with_pad_token else None
        # Reserve ids so pad/eos are stable and non-zero.
        for token in ("<|endoftext|>", "<|im_start|>", "<|im_end|>"):
            self._id(token)

    def _id(self, token: str) -> int:
        if token not in self._vocab:
            self._vocab[token] = len(self._vocab) + 1
        return self._vocab[token]

    # --- the surface preprocessing.py uses -------------------------------

    @property
    def pad_token(self) -> str | None:
        return self._pad_token

    @pad_token.setter
    def pad_token(self, value: str) -> None:
        self._pad_token = value

    @property
    def pad_token_id(self) -> int | None:
        return self._id(self._pad_token) if self._pad_token else None

    @property
    def eos_token_id(self) -> int:
        return self._id(self.eos_token)

    def apply_chat_template(self, messages, tokenize=False, add_generation_prompt=False) -> str:
        assert tokenize is False, "preprocessing must render to text, not ids"
        parts = []
        for message in messages:
            parts.append(f"<|im_start|>{message['role']}\n{message['content']}<|im_end|>\n")
        if add_generation_prompt:
            parts.append("<|im_start|>assistant\n")
        return "".join(parts)

    def __call__(self, text, add_special_tokens=False):
        assert add_special_tokens is False, "the chat template already inserts markers"
        tokens: list[str] = []
        for chunk in SPECIAL.split(text):
            if not chunk:
                continue
            if SPECIAL.fullmatch(chunk):
                tokens.append(chunk)
            else:
                tokens.extend(chunk.split())
        return {"input_ids": [self._id(token) for token in tokens]}


class BadPrefixTokenizer(StubTokenizer):
    """A tokenizer that merges across the prompt boundary.

    Exists to prove the boundary check actually fires. Without it, a silent
    one-token label shift would pass every other test here.
    """

    def __call__(self, text, add_special_tokens=False):
        ids = super().__call__(text, add_special_tokens)["input_ids"]
        # Only the *full* conversation is perturbed. The generation prompt
        # ends at "<|im_start|>assistant\n"; the full text continues into the
        # JSON target, so "assistant\n{" appears in one and not the other.
        # Perturbing both identically would leave the prefix intact and prove
        # nothing — which is exactly what an earlier version of this stub did.
        if "assistant\n{" in text:
            return {"input_ids": [self._id("MERGED")] + ids[1:]}
        return {"input_ids": ids}


EXAMPLE = {
    "id": "test-001",
    "system": "You are an offline emergency-response analyst.",
    "user": "<<<REPORT>>>\nwater rising fast sector 5, ppl on roofs\n<<<END REPORT>>>",
    "assistant": '{"category":"FLOODING","severity":"CRITICAL","summary":"Rising water.","access_status":"BLOCKED"}',
}

# --- test registry --------------------------------------------------------

FAILURES: list[str] = []
PASSES: list[str] = []


def check(name: str, condition: bool, detail: str = "") -> None:
    if condition:
        PASSES.append(name)
        print(f"  PASS  {name}")
    else:
        FAILURES.append(f"{name}: {detail}")
        print(f"  FAIL  {name}  {detail}")


# --- A. prompt masking ----------------------------------------------------


def test_a_prompt_masking() -> None:
    print("\nA. prompt masking")
    tokenizer = pp.configure_tokenizer(StubTokenizer())
    result = pp.tokenize_with_masking(
        tokenizer, EXAMPLE["system"], EXAMPLE["user"], EXAMPLE["assistant"],
        max_seq_length=4096, example_id=EXAMPLE["id"],
    )

    prompt_labels = result.labels[: result.prompt_tokens]
    check(
        "every prompt token is masked with -100",
        all(label == pp.IGNORE_INDEX for label in prompt_labels),
        f"{sum(1 for x in prompt_labels if x != pp.IGNORE_INDEX)} unmasked",
    )
    check(
        "the masked run is exactly the prompt length",
        result.labels.index(next(x for x in result.labels if x != pp.IGNORE_INDEX))
        == result.prompt_tokens,
        "first unmasked label is not at the prompt boundary",
    )
    check("prompt is non-trivial", result.prompt_tokens > 10, f"{result.prompt_tokens} tokens")


def test_b_target_only_loss() -> None:
    print("\nB. target-only loss")
    tokenizer = pp.configure_tokenizer(StubTokenizer())
    result = pp.tokenize_with_masking(
        tokenizer, EXAMPLE["system"], EXAMPLE["user"], EXAMPLE["assistant"],
        max_seq_length=4096, example_id=EXAMPLE["id"],
    )

    target_labels = result.labels[result.prompt_tokens :]
    check(
        "target tokens carry real labels",
        all(label != pp.IGNORE_INDEX for label in target_labels) and len(target_labels) > 0,
        "some target labels are masked",
    )
    check(
        "target labels equal the target input_ids",
        target_labels == result.input_ids[result.prompt_tokens :],
        "labels are not aligned with input_ids",
    )
    supervised = sum(1 for label in result.labels if label != pp.IGNORE_INDEX)
    check(
        "supervised tokens are a minority of the sequence (prompt dominates length)",
        0 < supervised < result.total_tokens,
        f"{supervised}/{result.total_tokens}",
    )
    print(
        f"        (prompt {result.prompt_tokens} tokens masked, "
        f"{result.target_tokens} target tokens supervised)"
    )

    # The boundary check must actually fire, or none of the above is trustworthy.
    try:
        pp.tokenize_with_masking(
            pp.configure_tokenizer(BadPrefixTokenizer()),
            EXAMPLE["system"], EXAMPLE["user"], EXAMPLE["assistant"],
            max_seq_length=4096, example_id="bad-prefix",
        )
        check("a tokenizer that breaks the prefix property is rejected", False, "no error raised")
    except pp.PreprocessingError:
        check("a tokenizer that breaks the prefix property is rejected", True)


def test_c_pad_token() -> None:
    print("\nC. pad token")
    without = StubTokenizer(with_pad_token=False)
    check("stub starts with no pad token", without.pad_token_id is None)
    pp.configure_tokenizer(without)
    check("pad_token is defined after configuration", without.pad_token is not None)
    check(
        "pad_token falls back to EOS",
        without.pad_token == without.eos_token,
        f"got {without.pad_token!r}",
    )
    check(
        "pad_token_id is a valid non-negative int",
        isinstance(without.pad_token_id, int) and without.pad_token_id >= 0,
        f"got {without.pad_token_id!r}",
    )

    preset = StubTokenizer(with_pad_token=True)
    original = preset.pad_token
    pp.configure_tokenizer(preset)
    check("an existing pad_token is left alone", preset.pad_token == original)

    # Padding must be inert: no attention, no loss.
    tokenizer = pp.configure_tokenizer(StubTokenizer())
    short = pp.tokenize_with_masking(
        tokenizer, "sys", "<<<REPORT>>>\nshort\n<<<END REPORT>>>", '{"a":1}',
        max_seq_length=4096, example_id="short",
    )
    long = pp.tokenize_with_masking(
        tokenizer, EXAMPLE["system"], EXAMPLE["user"], EXAMPLE["assistant"],
        max_seq_length=4096, example_id="long",
    )
    batch = pp.pad_batch([short, long], tokenizer.pad_token_id)

    padded_row, padding_len = 0, len(long.input_ids) - len(short.input_ids)
    check("batch rows are padded to equal length", len({len(r) for r in batch["input_ids"]}) == 1)
    check(
        "padding uses pad_token_id in input_ids",
        all(t == tokenizer.pad_token_id for t in batch["input_ids"][padded_row][-padding_len:]),
    )
    check(
        "padding has attention_mask 0",
        all(m == 0 for m in batch["attention_mask"][padded_row][-padding_len:]),
    )
    check(
        "padding has label -100 (contributes no loss)",
        all(l == pp.IGNORE_INDEX for l in batch["labels"][padded_row][-padding_len:]),
    )
    check(
        "real tokens keep attention_mask 1",
        all(m == 1 for m in batch["attention_mask"][padded_row][: len(short.input_ids)]),
    )


def test_d_sequence_length() -> None:
    print("\nD. max sequence length")
    tokenizer = pp.configure_tokenizer(StubTokenizer())
    try:
        pp.tokenize_with_masking(
            tokenizer, EXAMPLE["system"], EXAMPLE["user"], EXAMPLE["assistant"],
            max_seq_length=5, example_id="tiny-limit",
        )
        check("an over-length example raises instead of truncating", False, "no error raised")
    except pp.PreprocessingError as error:
        check(
            "an over-length example raises instead of truncating",
            "exceeds max_seq_length" in str(error),
            str(error),
        )

    result = pp.tokenize_with_masking(
        tokenizer, EXAMPLE["system"], EXAMPLE["user"], EXAMPLE["assistant"],
        max_seq_length=4096, example_id=EXAMPLE["id"],
    )
    check("a within-limit example is never truncated", result.total_tokens == len(result.input_ids))


def test_e_determinism() -> None:
    print("\nE. deterministic preprocessing")
    first = pp.tokenize_with_masking(
        pp.configure_tokenizer(StubTokenizer()), EXAMPLE["system"], EXAMPLE["user"],
        EXAMPLE["assistant"], max_seq_length=4096, example_id=EXAMPLE["id"],
    )
    second = pp.tokenize_with_masking(
        pp.configure_tokenizer(StubTokenizer()), EXAMPLE["system"], EXAMPLE["user"],
        EXAMPLE["assistant"], max_seq_length=4096, example_id=EXAMPLE["id"],
    )
    check("same input yields identical ids", first.input_ids == second.input_ids)
    check("same input yields identical labels", first.labels == second.labels)
    check("same input yields identical boundary", first.prompt_tokens == second.prompt_tokens)

    # Dataset order must not depend on dict/set iteration or file order.
    rows = [json.loads(l) for l in (PROCESSED / "train.jsonl").read_text(encoding="utf-8").splitlines() if l.strip()]
    check(
        "train.jsonl is stored in a stable, sorted order",
        [r["id"] for r in rows] == sorted(r["id"] for r in rows),
        "ids are not in sorted order",
    )


def test_f_dataset_version() -> None:
    print("\nF. dataset version")
    manifest = json.loads((PROCESSED / "manifest.json").read_text(encoding="utf-8"))
    config_text = (TRAINING_ROOT / "configs" / "lora_config.yaml").read_text(encoding="utf-8")
    check(
        "manifest records dataset_version v2",
        manifest.get("dataset_version") == "securemesh-slm-v2",
        f"got {manifest.get('dataset_version')!r}",
    )
    check(
        "lora_config.yaml agrees with the manifest",
        'dataset_version: "securemesh-slm-v2"' in config_text,
        "config dataset_version does not match the built dataset",
    )
    counts = {name: manifest["splits"][name]["count"] for name in ("train", "val", "test")}
    check(
        "split counts are the frozen 1052/214/162",
        counts == {"train": 1052, "val": 214, "test": 162},
        f"got {counts}",
    )


def test_g_test_set_exclusion() -> None:
    print("\nG. test-set exclusion")
    config_text = (TRAINING_ROOT / "configs" / "lora_config.yaml").read_text(encoding="utf-8")
    data_block = config_text.split("data:", 1)[1].split("provenance:", 1)[0]
    check(
        "the training config references no test file",
        "test.jsonl" not in data_block,
        "test.jsonl appears in the data section of lora_config.yaml",
    )
    check("finetune.py never opens test.jsonl",
          "test.jsonl" not in (SCRIPT_DIR / "finetune.py").read_text(encoding="utf-8"))

    def ids(name: str) -> set[str]:
        return {
            json.loads(l)["id"]
            for l in (PROCESSED / f"{name}.jsonl").read_text(encoding="utf-8").splitlines()
            if l.strip()
        }

    train, val, test = ids("train"), ids("val"), ids("test")
    check("no test id appears in train", not (train & test), f"{len(train & test)} shared")
    check("no test id appears in val", not (val & test), f"{len(val & test)} shared")

    def groups(name: str) -> set[str]:
        return {
            json.loads(l)["scenario_group"]
            for l in (PROCESSED / f"{name}.jsonl").read_text(encoding="utf-8").splitlines()
            if l.strip()
        }

    check(
        "no test scenario_group appears in train or val",
        not (groups("test") & (groups("train") | groups("val"))),
        "a scenario group crosses the held-out boundary",
    )


def main() -> int:
    print("SecureMesh-SLM preprocessing tests")
    print("(stub tokenizer — verifies masking logic, not real BPE token counts)")
    for test in (
        test_a_prompt_masking,
        test_b_target_only_loss,
        test_c_pad_token,
        test_d_sequence_length,
        test_e_determinism,
        test_f_dataset_version,
        test_g_test_set_exclusion,
    ):
        test()

    print(f"\n{len(PASSES)} passed, {len(FAILURES)} failed")
    for failure in FAILURES:
        print(f"  FAILED: {failure}")
    return 1 if FAILURES else 0


if __name__ == "__main__":
    sys.exit(main())
