"""Tokenization, label masking, and batching for SecureMesh-SLM fine-tuning.

Separated from `finetune.py` so the logic that decides *what the model learns
from* can be tested without a GPU, without `torch`, and without a downloaded
model. Every function here takes the tokenizer as an argument and touches no
global state, so a stub tokenizer can drive the identical code path the real
run will take.

# The objective: learn the target, not the prompt

The first implementation set `labels = input_ids`, which computes loss over
the system prompt and the report as well as the answer. That is wrong here in
a way that matters: the system prompt is ~1,400 characters and **byte-identical
in all 1,428 examples**, while the answer is a short JSON object. Most of the
gradient would go into reproducing a fixed string the model is already given
at inference time, and a model rewarded for emitting prompt text may start
emitting it instead of JSON.

Masking is done by **length, not by string search**:

```
prompt_text = chat_template([system, user], add_generation_prompt=True)
full_text   = chat_template([system, user, assistant])

prompt_ids = encode(prompt_text)      # e.g. 412 tokens
full_ids   = encode(full_text)        # e.g. 556 tokens

labels     = [-100] * 412  +  full_ids[412:]
             └── prompt ──┘  └── answer ──┘
```

Only the tokens after the prompt boundary carry a label; everything before it
is `-100`, which `CrossEntropyLoss` ignores. The boundary is exact because
`prompt_text` is a **string prefix** of `full_text` under ChatML — the
generation prompt ends at `<|im_start|>assistant\\n`, and the assistant turn
continues from there.

That prefix relationship is *checked, not assumed*
([`assert_prompt_is_prefix`]): a tokenizer that merged characters across the
boundary would silently shift every label by a token or two, which no metric
would reveal and which would quietly corrupt the run. If it ever fails,
preprocessing stops rather than training on misaligned labels.
"""

from __future__ import annotations

from dataclasses import dataclass

# The value `torch.nn.CrossEntropyLoss` ignores. Hard-coded rather than
# imported so this module stays importable without torch.
IGNORE_INDEX = -100


class PreprocessingError(RuntimeError):
    """Raised when an example cannot be prepared safely.

    Always fatal by design. Every condition that raises here would otherwise
    produce a run that trains successfully on subtly wrong data, which is
    worse than not training at all.
    """


@dataclass
class TokenizedExample:
    """One training example, ready for the collator."""

    input_ids: list[int]
    attention_mask: list[int]
    labels: list[int]
    prompt_tokens: int
    target_tokens: int

    @property
    def total_tokens(self) -> int:
        return len(self.input_ids)


def configure_tokenizer(tokenizer):
    """Ensures a usable pad token, preferring the model's EOS.

    Qwen2.5-Instruct normally defines `<|endoftext|>` as its pad token, but
    that is a property of a downloaded tokenizer config rather than a
    guarantee, and a `None` pad token surfaces much later as an unhelpful
    padding error deep inside the collator.

    EOS is the right fallback here because **padding is masked out of the
    loss anyway** (`pad_batch` writes `IGNORE_INDEX` into label positions and
    `0` into the attention mask), so reusing EOS as the pad id cannot teach
    the model anything about EOS placement.
    """
    if getattr(tokenizer, "pad_token_id", None) is None:
        eos_token = getattr(tokenizer, "eos_token", None)
        if eos_token is None:
            raise PreprocessingError(
                "tokenizer has neither a pad_token nor an eos_token; cannot pad a batch"
            )
        tokenizer.pad_token = eos_token

    if getattr(tokenizer, "pad_token_id", None) is None:
        raise PreprocessingError("setting pad_token did not produce a pad_token_id")
    if not isinstance(tokenizer.pad_token_id, int) or tokenizer.pad_token_id < 0:
        raise PreprocessingError(f"invalid pad_token_id: {tokenizer.pad_token_id!r}")
    return tokenizer


def build_texts(tokenizer, system: str, user: str, assistant: str) -> tuple[str, str]:
    """Renders the prompt and the full conversation through the chat template.

    The model's own template is used rather than a hand-written format string,
    so training sees exactly the turn markers Qwen2.5-Instruct was
    instruction-tuned with — and exactly what `llama-server` will construct at
    inference time from the same messages.
    """
    prompt_messages = [
        {"role": "system", "content": system},
        {"role": "user", "content": user},
    ]
    full_messages = prompt_messages + [{"role": "assistant", "content": assistant}]

    prompt_text = tokenizer.apply_chat_template(
        prompt_messages, tokenize=False, add_generation_prompt=True
    )
    full_text = tokenizer.apply_chat_template(full_messages, tokenize=False)
    return prompt_text, full_text


def encode(tokenizer, text: str) -> list[int]:
    """Token ids for a rendered string.

    `add_special_tokens=False` because the chat template has already inserted
    every marker the model expects; letting the tokenizer add more would put
    a stray BOS in front of a `<|im_start|>`.
    """
    encoded = tokenizer(text, add_special_tokens=False)
    return list(encoded["input_ids"])


def assert_prompt_is_prefix(prompt_ids: list[int], full_ids: list[int], example_id: str) -> None:
    """Verifies the label boundary is where the token counts say it is.

    Tokenizers merge greedily across character boundaries. If the last prompt
    character and the first answer character were ever merged into one token,
    `full_ids` would not begin with `prompt_ids`, the `-100` run would end in
    the wrong place, and the model would be trained to predict part of its own
    prompt while the first answer token went unsupervised.

    Nothing downstream can detect that, so it is checked here on every example.
    """
    if len(full_ids) <= len(prompt_ids):
        raise PreprocessingError(
            f"{example_id}: the full sequence ({len(full_ids)} tokens) is not longer than "
            f"the prompt ({len(prompt_ids)} tokens) — the target would be empty"
        )
    if full_ids[: len(prompt_ids)] != prompt_ids:
        raise PreprocessingError(
            f"{example_id}: the tokenized prompt is not a prefix of the tokenized "
            "conversation, so the prompt/target boundary cannot be located by length. "
            "This usually means the chat template does not extend the generation "
            "prompt verbatim. Refusing to train on misaligned labels."
        )


def tokenize_with_masking(
    tokenizer,
    system: str,
    user: str,
    assistant: str,
    max_seq_length: int,
    example_id: str = "<unknown>",
) -> TokenizedExample:
    """Tokenizes one example and masks everything before the assistant turn.

    **Never truncates.** An example longer than `max_seq_length` raises, so a
    silently clipped target — which would teach the model to emit unparsable
    JSON — cannot happen. The caller decides what to do about it; the
    configured limit is expected to be validated against the whole corpus
    before training starts.
    """
    prompt_text, full_text = build_texts(tokenizer, system, user, assistant)
    prompt_ids = encode(tokenizer, prompt_text)
    full_ids = encode(tokenizer, full_text)

    assert_prompt_is_prefix(prompt_ids, full_ids, example_id)

    if len(full_ids) > max_seq_length:
        raise PreprocessingError(
            f"{example_id}: {len(full_ids)} tokens exceeds max_seq_length "
            f"({max_seq_length}). Raise max_seq_length rather than truncating — "
            "a clipped target trains the model to produce invalid JSON."
        )

    prompt_tokens = len(prompt_ids)
    labels = [IGNORE_INDEX] * prompt_tokens + full_ids[prompt_tokens:]

    if all(label == IGNORE_INDEX for label in labels):
        raise PreprocessingError(f"{example_id}: every label is masked; nothing would be learned")

    return TokenizedExample(
        input_ids=full_ids,
        attention_mask=[1] * len(full_ids),
        labels=labels,
        prompt_tokens=prompt_tokens,
        target_tokens=len(full_ids) - prompt_tokens,
    )


def pad_batch(
    features: list[TokenizedExample] | list[dict], pad_token_id: int
) -> dict[str, list[list[int]]]:
    """Right-pads a batch, preserving `-100` and adding no trainable padding.

    Three parallel paddings, each with the value that makes padding inert:

    | field | pad value | why |
    |---|---|---|
    | `input_ids` | `pad_token_id` | must be a real id the embedding table has |
    | `attention_mask` | `0` | attention must not look at padding |
    | `labels` | `IGNORE_INDEX` | padding must contribute no loss |

    This exists because `DataCollatorForLanguageModeling` — the collator the
    first implementation used — **overwrites** whatever labels it is given
    with a clone of `input_ids`. Under it the masking above would have been
    computed, discarded, and never applied.
    """
    rows = [f if isinstance(f, dict) else f.__dict__ for f in features]
    if not rows:
        raise PreprocessingError("cannot collate an empty batch")

    longest = max(len(row["input_ids"]) for row in rows)

    batch: dict[str, list[list[int]]] = {"input_ids": [], "attention_mask": [], "labels": []}
    for row in rows:
        input_ids = list(row["input_ids"])
        attention_mask = list(row["attention_mask"])
        labels = list(row["labels"])
        if not (len(input_ids) == len(attention_mask) == len(labels)):
            raise PreprocessingError(
                "input_ids, attention_mask and labels must be the same length before padding"
            )

        padding = longest - len(input_ids)
        batch["input_ids"].append(input_ids + [pad_token_id] * padding)
        batch["attention_mask"].append(attention_mask + [0] * padding)
        batch["labels"].append(labels + [IGNORE_INDEX] * padding)

    return batch


class MaskedCausalCollator:
    """Batches examples for `Trainer`, preserving the `-100` masking.

    A thin wrapper over [`pad_batch`] that converts to tensors. All of the
    logic worth testing lives in `pad_batch`, which needs no torch.
    """

    def __init__(self, pad_token_id: int) -> None:
        if not isinstance(pad_token_id, int) or pad_token_id < 0:
            raise PreprocessingError(f"invalid pad_token_id: {pad_token_id!r}")
        self.pad_token_id = pad_token_id

    def __call__(self, features):
        import torch  # local import: the collator is only used inside a real run

        batch = pad_batch(features, self.pad_token_id)
        return {key: torch.tensor(value, dtype=torch.long) for key, value in batch.items()}


def measure_corpus(
    tokenizer, examples: list[dict], max_seq_length: int
) -> dict:
    """Tokenizes every example and reports the length distribution.

    Run before training so `max_seq_length` is validated against the corpus
    rather than assumed. Collects *all* over-length examples instead of
    stopping at the first, so one pass tells the operator the full picture.
    """
    lengths: list[int] = []
    prompt_lengths: list[int] = []
    target_lengths: list[int] = []
    over_length: list[tuple[str, int]] = []

    for example in examples:
        try:
            tokenized = tokenize_with_masking(
                tokenizer,
                example["system"],
                example["user"],
                example["assistant"],
                max_seq_length=10**9,  # measure first, judge after
                example_id=example.get("id", "<unknown>"),
            )
        except PreprocessingError:
            raise
        lengths.append(tokenized.total_tokens)
        prompt_lengths.append(tokenized.prompt_tokens)
        target_lengths.append(tokenized.target_tokens)
        if tokenized.total_tokens > max_seq_length:
            over_length.append((example.get("id", "<unknown>"), tokenized.total_tokens))

    lengths.sort()
    return {
        "examples": len(lengths),
        "max_tokens": lengths[-1] if lengths else 0,
        "min_tokens": lengths[0] if lengths else 0,
        "mean_tokens": round(sum(lengths) / len(lengths)) if lengths else 0,
        "median_tokens": lengths[len(lengths) // 2] if lengths else 0,
        "mean_prompt_tokens": round(sum(prompt_lengths) / len(prompt_lengths)) if prompt_lengths else 0,
        "mean_target_tokens": round(sum(target_lengths) / len(target_lengths)) if target_lengths else 0,
        "max_seq_length": max_seq_length,
        "over_length": over_length,
        "fits": not over_length,
    }
