"""Transformers-5 adapter for lm-format-enforcer's JSON-schema enforcement.

lm-format-enforcer 0.11.3 — the newest release — ships a transformers
integration whose import block begins
`from transformers.tokenization_utils import PreTrainedTokenizerBase`.
transformers 5.x removed that module path (the class now lives at the top
level), so the integration fails to import even though everything it actually
uses still exists. Its failure is also misreported: the `except ImportError`
around that line re-raises as "transformers is not installed".

The library's core — `JsonSchemaParser`, `TokenEnforcer`,
`TokenEnforcerTokenizerData` — imports and works. So the two small functions
evaluation needs are mirrored here on top of that core, copied from
`lmformatenforcer/integrations/transformers.py` at 0.11.3 with **the import
path fixed and nothing else changed**, so enforcement behaves exactly as the
library intends.

Alternatives rejected: downgrading transformers would break the validated
training stack; editing site-packages is unreproducible and silently undone
by any reinstall; injecting a stand-in module into `sys.modules` hides the
fix somewhere a reader would never look. The diagnostics-only helpers in the
original (`generate_enforced`, the logits-saver warper) are not mirrored,
because evaluation does not use them.
"""

from __future__ import annotations

import functools
from typing import List, Optional, Tuple, Union

from lmformatenforcer import CharacterLevelParser, TokenEnforcer, TokenEnforcerTokenizerData
from transformers import PreTrainedTokenizerBase  # the one line that changed


def _build_regular_tokens_list(
    tokenizer: PreTrainedTokenizerBase, vocab_size: int
) -> List[Tuple[int, str, bool]]:
    token_0 = tokenizer.encode("0")[-1]
    regular_tokens = []
    for token_idx in range(vocab_size):
        if token_idx in tokenizer.all_special_ids:
            continue
        # Prepend token "0" and drop the first character of the result, so a
        # token that starts a word decodes with its leading space intact.
        decoded_after_0 = tokenizer.decode([token_0, token_idx])[1:]
        decoded_regular = tokenizer.decode([token_idx])
        is_word_start_token = len(decoded_after_0) > len(decoded_regular)
        regular_tokens.append((token_idx, decoded_after_0, is_word_start_token))
    return regular_tokens


def _decode_function(tokenizer: PreTrainedTokenizerBase, tokens: List[int]) -> str:
    return tokenizer.decode(tokens).rstrip("�")


def build_token_enforcer_tokenizer_data(
    tokenizer: PreTrainedTokenizerBase,
    use_bitmask: bool = False,
    vocab_size: Optional[int] = None,
) -> TokenEnforcerTokenizerData:
    vocab_size = vocab_size or len(tokenizer)
    regular_tokens = _build_regular_tokens_list(tokenizer, vocab_size)
    decode_fn = functools.partial(_decode_function, tokenizer)
    return TokenEnforcerTokenizerData(
        regular_tokens, decode_fn, tokenizer.eos_token_id, use_bitmask, vocab_size
    )


class TransformersPrefixAllowedTokensFn:
    def __init__(self, token_enforcer: TokenEnforcer) -> None:
        self.token_enforcer = token_enforcer

    def __call__(self, batch_id: int, sent) -> List[int]:
        return self.token_enforcer.get_allowed_tokens(sent.tolist()).allowed_tokens


def build_transformers_prefix_allowed_tokens_fn(
    tokenizer_data: Union[PreTrainedTokenizerBase, TokenEnforcerTokenizerData],
    character_level_parser: CharacterLevelParser,
) -> TransformersPrefixAllowedTokensFn:
    if isinstance(tokenizer_data, PreTrainedTokenizerBase):
        tokenizer_data = build_token_enforcer_tokenizer_data(tokenizer_data)
    return TransformersPrefixAllowedTokensFn(TokenEnforcer(tokenizer_data, character_level_parser))
