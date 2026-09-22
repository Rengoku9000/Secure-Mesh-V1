"""Loads the real Qwen2.5 tokenizer from the local production GGUF.

# Why from the GGUF

Measuring sequence lengths needs the *actual* tokenizer the fine-tuned model
will use — a stub, a BERT vocab, or a character estimate all give numbers
that mean nothing for a decision about `max_seq_length`. The Hugging Face
weights are not on this machine and are not to be downloaded, but the
tokenizer is already here: it is embedded in the GGUF an operator
provisioned for the running application.

`ai/models/llm/qwen2.5-1.5b-instruct-q4_k_m.gguf` is opened **read-only**.
Nothing is written, converted, or replaced. It is the same file
`run_baseline.py` served through `llama-server`, and its SHA-256 is checked
against `ai/models/README.md` so the tokenizer is provably from the model
the baseline was measured on.

# The chat template is the part that matters

Token counts depend on the chat template as much as on the vocabulary — the
ChatML markers, the `<|im_start|>assistant\\n` generation prompt, and the
trailing `<|im_end|>` are all tokens. GGUF stores the template in its
metadata under `tokenizer.chat_template`, but transformers' GGUF loader does
not always carry it onto the tokenizer object. When it does not, the
template is read directly from the GGUF metadata and attached.

That is recovery of the model's own template from the model's own file, not
a substitution: [`verify_qwen_tokenizer`] refuses anything that does not
look like Qwen2.5 ChatML, so a wrong or defaulted template cannot silently
produce a comfortable number.
"""

from __future__ import annotations

import hashlib
from pathlib import Path

# Recorded in ai/models/README.md. The tokenizer is only meaningful for this
# phase if it comes from the exact model the baseline was measured against.
EXPECTED_GGUF_SHA256 = "6A1A2EB6D15622BF3C96857206351BA97E1AF16C30D7A74EE38970E434E9407E"

# Qwen2.5 ChatML control tokens. Their presence is what distinguishes the
# real tokenizer from any substitute.
REQUIRED_SPECIAL_TOKENS = ["<|im_start|>", "<|im_end|>", "<|endoftext|>"]

# Qwen2.5-1.5B-Instruct vocabulary. Checked as a lower bound rather than an
# equality, because padded vocab sizes differ between exports.
MIN_EXPECTED_VOCAB = 150_000


class TokenizerVerificationError(RuntimeError):
    """The loaded tokenizer is not demonstrably the Qwen2.5 one."""


def gguf_sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest().upper()


def read_chat_template_from_gguf(path: Path) -> str | None:
    """Reads `tokenizer.chat_template` straight out of the GGUF metadata."""
    from gguf import GGUFReader

    reader = GGUFReader(str(path), "r")
    for field_name, field in reader.fields.items():
        if field_name == "tokenizer.chat_template":
            return field.contents()
    return None


def verify_qwen_tokenizer(tokenizer, strict: bool = True) -> dict:
    """Proves the tokenizer is Qwen2.5 ChatML before any number is trusted."""
    findings = {}
    problems = []

    vocab_size = len(tokenizer)
    findings["vocab_size"] = vocab_size
    if vocab_size < MIN_EXPECTED_VOCAB:
        problems.append(f"vocabulary is {vocab_size}, expected >= {MIN_EXPECTED_VOCAB}")

    findings["tokenizer_class"] = type(tokenizer).__name__

    # Each ChatML marker must be a single token, not spelled out character by
    # character — that is the difference between the real vocabulary and a
    # lookalike.
    special_token_ids = {}
    for token in REQUIRED_SPECIAL_TOKENS:
        ids = tokenizer(token, add_special_tokens=False)["input_ids"]
        special_token_ids[token] = ids
        if len(ids) != 1:
            problems.append(f"{token!r} tokenizes to {len(ids)} tokens, expected 1")
    findings["special_token_ids"] = special_token_ids

    template = getattr(tokenizer, "chat_template", None)
    findings["has_chat_template"] = template is not None
    if template is None:
        problems.append("no chat template")
    else:
        findings["chat_template_sha256"] = hashlib.sha256(template.encode()).hexdigest()[:16]
        if "im_start" not in template:
            problems.append("chat template does not reference ChatML markers")

    if problems and strict:
        raise TokenizerVerificationError("; ".join(problems))
    findings["problems"] = problems
    return findings


def load_tokenizer_from_gguf(gguf_path: Path, verify_hash: bool = True):
    """Returns (tokenizer, info). Opens the GGUF read-only."""
    from transformers import AutoTokenizer

    if not gguf_path.exists():
        raise FileNotFoundError(f"GGUF not found: {gguf_path}")

    info: dict = {"gguf_path": str(gguf_path), "gguf_size_bytes": gguf_path.stat().st_size}

    if verify_hash:
        actual = gguf_sha256(gguf_path)
        info["gguf_sha256"] = actual
        info["gguf_sha256_matches_readme"] = actual == EXPECTED_GGUF_SHA256
        if not info["gguf_sha256_matches_readme"]:
            raise TokenizerVerificationError(
                f"GGUF SHA-256 {actual} does not match the value recorded in "
                f"ai/models/README.md ({EXPECTED_GGUF_SHA256}). Refusing to measure "
                "against a model that is not the one the baseline used."
            )

    tokenizer = AutoTokenizer.from_pretrained(
        str(gguf_path.parent), gguf_file=gguf_path.name, local_files_only=True
    )

    # Recover the chat template from GGUF metadata if the loader dropped it.
    if getattr(tokenizer, "chat_template", None) is None:
        template = read_chat_template_from_gguf(gguf_path)
        if template:
            tokenizer.chat_template = template
            info["chat_template_source"] = "recovered from GGUF metadata"
        else:
            info["chat_template_source"] = "MISSING"
    else:
        info["chat_template_source"] = "carried by the transformers GGUF loader"

    info.update(verify_qwen_tokenizer(tokenizer))
    return tokenizer, info
