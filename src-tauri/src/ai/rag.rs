//! Retrieval-augmented generation over local records.
//!
//! ```text
//!   question ──▶ local embedding ──▶ local retrieval ──▶ top-k passages
//!                                                             │
//!                          local model ◀── context + question ┘
//!                                │
//!                                ▼
//!                    answer + the sources it was built from
//! ```
//!
//! # Grounding is enforced here, not requested of the model
//!
//! A model told to cite its sources will sometimes cite ones that do not exist,
//! and a small one will often not cite at all. Measured against the evaluation
//! corpus, asking a 1.5B model in prose for `[S1]` markers produced citations in
//! **0 of 19** answerable questions. Asking nicely is not a mechanism. So:
//!
//! - the model is given **only** the retrieved passages and the question;
//! - the answer is **schema-constrained** ([`crate::ai::prompt::rag_schema`]) to
//!   carry `sources` and `sufficient` fields, so it cannot return an answer
//!   without also stating which passages it used;
//! - those passage numbers are checked against what was actually supplied, and
//!   invented ones are dropped — as are any inline markers ([`extract_citations`]);
//! - if retrieval found nothing, the model is never called at all — there is
//!   nothing it could ground an answer in, so the refusal is returned directly;
//! - `sufficient: false`, an empty answer, or output that will not parse all
//!   become the same canonical refusal, with no sources attached;
//! - an answer that cites nothing is reported as ungrounded rather than
//!   presented as fact.
//!
//! What this does **not** do is prevent a model from stating something false
//! that happens to resemble the context, or from citing a passage it did not
//! actually use. Grounding is checked at the level of citation, not of meaning,
//! and that limit is documented rather than glossed.

use crate::ai::embedding::EmbeddingEngine;
use crate::ai::engine::{LocalInferenceEngine, StructuredRequest};
use crate::ai::prompt::{self, INSUFFICIENT_CONTEXT_REPLY};
use crate::error::CoreResult;
use crate::storage::intelligence::{EmbeddingKind, RetrievedPassage};
use crate::storage::Database;
use serde::Serialize;

/// Passages retrieved for one question.
pub const DEFAULT_TOP_K: usize = 5;

/// Minimum cosine similarity for a passage to be considered relevant.
///
/// Chosen empirically against the synthetic corpus: below roughly this, matches
/// are topically unrelated, and feeding them to the model invites an answer
/// assembled from noise. Set as a named constant because it is a judgement
/// call, not a fact — see `docs/ai/EVALUATION.md` for how it was checked.
pub const MIN_RELEVANCE: f32 = 0.35;

/// Longest answer a model may produce for a question.
const ANSWER_TOKENS: u32 = 400;

/// A source an answer was built from.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnswerSource {
    /// The passage number the model was shown, as `"1"`, `"2"`, … The UI
    /// renders it as `[1]`, so an operator sees the same label the model did.
    pub marker: String,
    pub kind: EmbeddingKind,
    /// Chunk ID or incident ID.
    pub subject_id: String,
    pub title: String,
    pub score: f32,
    /// A short excerpt, so an operator can judge the citation without opening
    /// the whole document.
    pub excerpt: String,
    /// Whether the model actually cited this passage, as opposed to it merely
    /// being offered. Retrieved-but-uncited is useful signal.
    pub cited: bool,
}

/// The result of a grounded question.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GroundedAnswer {
    pub question: String,
    pub answer: String,
    pub sources: Vec<AnswerSource>,
    /// True when the answer cites at least one supplied passage.
    ///
    /// The UI must distinguish this from a confident-sounding answer that cites
    /// nothing — the latter is model interpretation, not something the data
    /// supports.
    pub grounded: bool,
    /// True when the corpus could not answer and the refusal was returned.
    pub refused: bool,
    /// How many source numbers the model returned that did not correspond to a
    /// supplied passage.
    ///
    /// They are dropped before they reach here, so this is never a citation a
    /// user could follow — it is the fabrication filter reporting that it fired.
    /// A non-zero count on an otherwise confident answer is a reason to distrust
    /// the rest of it.
    pub dropped_citations: usize,
    pub model_id: String,
    pub retrieval_ms: u64,
    pub generation_ms: u64,
}

impl GroundedAnswer {
    /// The refusal, returned without calling the model.
    fn refusal(question: &str, model_id: &str, retrieval_ms: u64) -> Self {
        Self {
            question: question.to_string(),
            answer: INSUFFICIENT_CONTEXT_REPLY.to_string(),
            sources: Vec::new(),
            grounded: false,
            refused: true,
            dropped_citations: 0,
            model_id: model_id.to_string(),
            retrieval_ms,
            generation_ms: 0,
        }
    }
}

/// Splits text into overlapping passages.
///
/// Chunking on sentence boundaries keeps passages readable when quoted back to
/// an operator. The overlap exists because a fact that straddles a boundary
/// would otherwise be retrievable from neither half.
pub fn chunk_text(text: &str, target_chars: usize, overlap_sentences: usize) -> Vec<String> {
    let normalised = normalise(text);
    if normalised.is_empty() {
        return Vec::new();
    }

    let sentences = split_sentences(&normalised);
    if sentences.is_empty() {
        return Vec::new();
    }

    let mut chunks = Vec::new();
    let mut current: Vec<String> = Vec::new();
    let mut length = 0usize;

    for sentence in sentences {
        let sentence_len = sentence.chars().count();

        // A single sentence longer than the target becomes its own chunk rather
        // than being split mid-thought.
        if length > 0 && length + sentence_len > target_chars {
            chunks.push(current.join(" "));
            let keep = current.len().saturating_sub(overlap_sentences);
            current = current.split_off(keep);
            length = current.iter().map(|s| s.chars().count()).sum();
        }

        length += sentence_len;
        current.push(sentence);
    }

    if !current.is_empty() {
        chunks.push(current.join(" "));
    }

    chunks
        .into_iter()
        .filter(|c| !c.trim().is_empty())
        .collect()
}

/// Collapses whitespace so chunking is not thrown off by document formatting.
pub fn normalise(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Splits on sentence terminators, keeping the terminator.
fn split_sentences(text: &str) -> Vec<String> {
    let mut sentences = Vec::new();
    let mut current = String::new();

    for character in text.chars() {
        current.push(character);
        if matches!(character, '.' | '!' | '?') {
            let trimmed = current.trim();
            if !trimmed.is_empty() {
                sentences.push(trimmed.to_string());
            }
            current.clear();
        }
    }

    let remainder = current.trim();
    if !remainder.is_empty() {
        sentences.push(remainder.to_string());
    }
    sentences
}

/// SHA-256 of normalised text, for detecting a re-import of the same document.
pub fn content_hash(text: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(normalise(text).as_bytes()))
}

/// Pulls `[S1]`-style markers out of an answer, keeping only real ones.
///
/// Secondary to the schema's `sources` field: some models also write markers
/// into their prose, and counting those costs nothing. A model that invents
/// `[S9]` when it was shown three passages has fabricated a citation, so
/// out-of-range markers are dropped.
pub fn extract_citations(answer: &str, available: usize) -> Vec<usize> {
    let mut cited = Vec::new();
    let bytes = answer.as_bytes();
    let mut index = 0usize;

    while index < bytes.len() {
        if bytes[index] != b'[' {
            index += 1;
            continue;
        }

        // Expect [S<digits>]
        let mut cursor = index + 1;
        if cursor >= bytes.len() || (bytes[cursor] != b'S' && bytes[cursor] != b's') {
            index += 1;
            continue;
        }
        cursor += 1;

        let digits_start = cursor;
        while cursor < bytes.len() && bytes[cursor].is_ascii_digit() {
            cursor += 1;
        }

        if cursor > digits_start && cursor < bytes.len() && bytes[cursor] == b']' {
            if let Ok(number) = answer[digits_start..cursor].parse::<usize>() {
                // 1-based, and only if it was actually supplied.
                if number >= 1 && number <= available && !cited.contains(&number) {
                    cited.push(number);
                }
            }
            index = cursor + 1;
        } else {
            index += 1;
        }
    }

    cited.sort_unstable();
    cited
}

/// Answers a question from local records only.
///
/// Both engines are borrowed rather than owned, and neither is given access to
/// anything beyond the text it is handed — see the trust boundary note in
/// [`crate::ai`].
pub fn answer_question(
    database: &Database,
    embedder: &dyn EmbeddingEngine,
    generator: &dyn LocalInferenceEngine,
    question: &str,
    top_k: usize,
) -> CoreResult<GroundedAnswer> {
    let question = question.trim();
    if question.is_empty() {
        return Err(crate::error::CoreError::validation(
            "a question cannot be empty",
        ));
    }

    let model_id = generator
        .model_info()
        .map(|info| info.model_id)
        .unwrap_or_else(|| "unknown".to_string());

    // --- Retrieve ---------------------------------------------------------
    let retrieval_start = std::time::Instant::now();
    let query = embedder.embed(question)?;
    let passages = database.search_embeddings(&query, top_k.max(1), MIN_RELEVANCE)?;
    let retrieval_ms = retrieval_start.elapsed().as_millis() as u64;

    // Nothing relevant: the model is not called. It could only invent.
    if passages.is_empty() {
        return Ok(GroundedAnswer::refusal(question, &model_id, retrieval_ms));
    }

    // --- Generate ---------------------------------------------------------
    let context: Vec<String> = passages.iter().map(|p| p.content.clone()).collect();
    let generation_start = std::time::Instant::now();

    let raw = generator.generate_structured(&StructuredRequest {
        system: prompt::RAG_SYSTEM_PROMPT.to_string(),
        user: prompt::rag_user_message(&context, question),
        schema: prompt::rag_schema(),
        max_tokens: ANSWER_TOKENS,
    })?;
    let generation_ms = generation_start.elapsed().as_millis() as u64;

    // Untrusted, exactly like an analysis. A malformed answer becomes a refusal
    // rather than an error: the operator asked a question and deserves a
    // truthful "I cannot answer" over a stack trace.
    let Ok(parsed) = serde_json::from_str::<RawAnswer>(&raw) else {
        return Ok(GroundedAnswer::refusal(question, &model_id, retrieval_ms));
    };

    let answer_text = parsed.answer.trim().to_string();

    // The model says the context was not enough. Return the canonical refusal
    // rather than its own wording, so the UI has one string to recognise.
    if !parsed.sufficient || answer_text.is_empty() {
        let mut refusal = GroundedAnswer::refusal(question, &model_id, retrieval_ms);
        refusal.generation_ms = generation_ms;
        return Ok(refusal);
    }

    // Citations are validated against what was actually supplied. A model
    // naming passage 9 when it was shown three has fabricated a reference, and
    // showing it would be worse than showing none.
    // Deduplicated before counting, so a model repeating `[1, 1]` is not
    // recorded as having fabricated a source.
    let mut claimed = parsed.sources;
    claimed.sort_unstable();
    claimed.dedup();

    let mut cited: Vec<usize> = claimed
        .iter()
        .filter_map(|number| usize::try_from(*number).ok())
        .filter(|number| *number >= 1 && *number <= passages.len())
        .collect();
    let dropped_citations = claimed.len() - cited.len();

    // Some models also write markers into the prose; count those too.
    cited.extend(extract_citations(&answer_text, passages.len()));
    cited.sort_unstable();
    cited.dedup();

    Ok(GroundedAnswer {
        question: question.to_string(),
        answer: answer_text,
        grounded: !cited.is_empty(),
        refused: false,
        dropped_citations,
        sources: build_sources(&passages, &cited),
        model_id,
        retrieval_ms,
        generation_ms,
    })
}

/// **Untrusted.** The shape a model is asked to produce for a question.
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAnswer {
    answer: String,
    /// Passage numbers the model says it used. Validated before use.
    #[serde(default)]
    sources: Vec<i64>,
    /// Whether the context could answer at all.
    sufficient: bool,
}

/// Pairs retrieved passages with whether the model cited them.
fn build_sources(passages: &[RetrievedPassage], cited: &[usize]) -> Vec<AnswerSource> {
    passages
        .iter()
        .enumerate()
        .map(|(index, passage)| {
            let marker_number = index + 1;
            AnswerSource {
                marker: marker_number.to_string(),
                kind: passage.kind,
                subject_id: passage.subject_id.clone(),
                title: passage.source_title.clone(),
                score: passage.score,
                excerpt: passage.content.chars().take(240).collect(),
                cited: cited.contains(&marker_number),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Citation extraction ----------------------------------------------

    #[test]
    fn citations_are_extracted_in_order() {
        let cited = extract_citations("Per [S2] and [S1], the road is blocked.", 3);
        assert_eq!(cited, vec![1, 2]);
    }

    #[test]
    fn an_invented_citation_is_discarded() {
        // The model was shown two passages and cited a ninth. Returning it
        // would display a source that does not exist.
        let cited = extract_citations("According to [S9], everything is fine.", 2);
        assert!(cited.is_empty());
    }

    #[test]
    fn out_of_range_citations_are_dropped_but_valid_ones_kept() {
        let cited = extract_citations("[S1] is right, [S7] is invented.", 3);
        assert_eq!(cited, vec![1]);
    }

    #[test]
    fn repeated_citations_are_counted_once() {
        assert_eq!(extract_citations("[S1] and again [S1].", 2), vec![1]);
    }

    #[test]
    fn zero_is_not_a_valid_citation() {
        // Markers are 1-based; [S0] is a fabrication.
        assert!(extract_citations("see [S0]", 3).is_empty());
    }

    #[test]
    fn malformed_markers_are_ignored_without_panicking() {
        for text in [
            "[S]",
            "[",
            "[[S1",
            "[Sx]",
            "]S1[",
            "[S 1]",
            "",
            "[S99999999999999999999]",
        ] {
            let _ = extract_citations(text, 3);
        }
        assert!(extract_citations("[S]", 3).is_empty());
        assert!(extract_citations("[Sx]", 3).is_empty());
    }

    #[test]
    fn lowercase_markers_are_accepted() {
        // Small models are inconsistent about case; the citation is still real.
        assert_eq!(extract_citations("per [s2]", 3), vec![2]);
    }

    #[test]
    fn an_answer_with_no_citations_yields_none() {
        assert!(extract_citations("The bridge is closed.", 5).is_empty());
    }

    // --- Chunking ----------------------------------------------------------

    #[test]
    fn text_is_chunked_on_sentence_boundaries() {
        let text = "First sentence here. Second sentence here. Third sentence here.";
        let chunks = chunk_text(text, 40, 1);

        assert!(chunks.len() > 1);
        // Every chunk should end mid-document only at a terminator.
        for chunk in &chunks {
            assert!(chunk.ends_with('.'), "chunk ended mid-sentence: {chunk}");
        }
    }

    #[test]
    fn chunks_overlap_so_a_straddling_fact_stays_retrievable() {
        let text = "Alpha one. Bravo two. Charlie three. Delta four. Echo five.";
        let chunks = chunk_text(text, 25, 1);

        assert!(chunks.len() >= 2);
        // The last sentence of one chunk reappears at the start of the next.
        let first_tail = chunks[0].split(' ').next_back().unwrap();
        assert!(
            chunks[1].contains(first_tail),
            "expected overlap between {:?} and {:?}",
            chunks[0],
            chunks[1]
        );
    }

    #[test]
    fn a_sentence_longer_than_the_target_becomes_its_own_chunk() {
        let long = format!("{}.", "word ".repeat(200));
        let chunks = chunk_text(&long, 50, 1);

        assert_eq!(
            chunks.len(),
            1,
            "a single sentence is not split mid-thought"
        );
    }

    #[test]
    fn document_formatting_does_not_affect_chunking() {
        let messy = "First   sentence.\n\n\tSecond    sentence.";
        let tidy = "First sentence. Second sentence.";

        assert_eq!(chunk_text(messy, 1_000, 1), chunk_text(tidy, 1_000, 1));
    }

    #[test]
    fn empty_or_whitespace_text_produces_no_chunks() {
        assert!(chunk_text("", 100, 1).is_empty());
        assert!(chunk_text("   \n\t  ", 100, 1).is_empty());
    }

    #[test]
    fn text_without_terminators_still_chunks() {
        let chunks = chunk_text("no punctuation at all here", 100, 1);
        assert_eq!(chunks.len(), 1);
    }

    #[test]
    fn chunking_is_deterministic() {
        let text = "One. Two. Three. Four. Five. Six.";
        assert_eq!(chunk_text(text, 20, 1), chunk_text(text, 20, 1));
    }

    // --- Content hashing ---------------------------------------------------

    #[test]
    fn the_content_hash_ignores_formatting_differences() {
        // Re-importing the same document with different whitespace must be
        // detected as a duplicate, not indexed twice.
        assert_eq!(
            content_hash("Some   text.\n\nMore text."),
            content_hash("Some text. More text.")
        );
    }

    #[test]
    fn different_content_hashes_differently() {
        assert_ne!(content_hash("Alpha"), content_hash("Beta"));
    }

    // --- Source attribution ------------------------------------------------

    fn passage(id: &str, score: f32) -> RetrievedPassage {
        RetrievedPassage {
            kind: EmbeddingKind::KnowledgeChunk,
            subject_id: id.to_string(),
            content: format!("content of {id}"),
            source_title: "Manual".to_string(),
            score,
        }
    }

    #[test]
    fn sources_record_which_passages_were_actually_cited() {
        let passages = [passage("a", 0.9), passage("b", 0.8), passage("c", 0.7)];
        let sources = build_sources(&passages, &[1, 3]);

        assert_eq!(sources.len(), 3);
        assert!(sources[0].cited);
        assert!(!sources[1].cited, "retrieved but uncited is useful signal");
        assert!(sources[2].cited);
        assert_eq!(sources[0].marker, "1");
    }

    #[test]
    fn source_excerpts_are_bounded() {
        let long = RetrievedPassage {
            kind: EmbeddingKind::Incident,
            subject_id: "i".to_string(),
            content: "x".repeat(5_000),
            source_title: "Incident".to_string(),
            score: 0.9,
        };

        let sources = build_sources(&[long], &[1]);
        assert!(sources[0].excerpt.chars().count() <= 240);
    }

    // --- Refusal -----------------------------------------------------------

    #[test]
    fn a_refusal_carries_no_sources_and_is_not_grounded() {
        let refusal = GroundedAnswer::refusal("anything?", "m", 12);

        assert!(refusal.refused);
        assert!(!refusal.grounded);
        assert!(refusal.sources.is_empty());
        assert_eq!(refusal.answer, INSUFFICIENT_CONTEXT_REPLY);
        assert_eq!(refusal.dropped_citations, 0);
        // No model was called, so generation cost nothing.
        assert_eq!(refusal.generation_ms, 0);
    }

    #[test]
    fn the_relevance_threshold_is_a_real_filter() {
        // A threshold of 0 would admit anything and make retrieval meaningless.
        const { assert!(MIN_RELEVANCE > 0.0) };
        const { assert!(MIN_RELEVANCE < 1.0) };
    }
}
