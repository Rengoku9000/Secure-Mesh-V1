//! Local embeddings and vector similarity.
//!
//! Embeddings are computed on this device by a local model. There is no
//! embedding API, no key, and no request that leaves the machine.
//!
//! # Why vectors are stored as raw bytes rather than a vector extension
//!
//! SecureMesh already carries SQLite. Adding a vector extension would mean a
//! native dependency on every platform the node targets, for a corpus that is
//! measured in thousands of chunks rather than millions. A brute-force cosine
//! scan over a few thousand 384-dimensional vectors is well under a millisecond
//! and needs no new dependency at all.
//!
//! That trade stops holding somewhere in the tens of thousands of chunks. The
//! interface here — retrieve candidates, score, take top-k — is the same shape
//! an approximate index would need, so replacing it later does not disturb
//! callers. Documented rather than left to be discovered.

use crate::ai::engine::EngineHealth;
use crate::error::{CoreError, CoreResult};

/// A single embedding vector.
///
/// Kept alongside the model that produced it: vectors from different models are
/// not comparable, and silently mixing them produces retrieval that looks like
/// it works and does not.
#[derive(Debug, Clone, PartialEq)]
pub struct Embedding {
    pub vector: Vec<f32>,
    pub model_id: String,
}

impl Embedding {
    pub fn new(vector: Vec<f32>, model_id: impl Into<String>) -> CoreResult<Self> {
        if vector.is_empty() {
            return Err(CoreError::validation("an embedding cannot be empty"));
        }
        if vector.iter().any(|v| !v.is_finite()) {
            return Err(CoreError::validation(
                "an embedding cannot contain non-finite values",
            ));
        }
        Ok(Self {
            vector,
            model_id: model_id.into(),
        })
    }

    pub fn dimensions(&self) -> usize {
        self.vector.len()
    }

    /// Little-endian `f32` bytes, for storage in a BLOB column.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(self.vector.len() * 4);
        for value in &self.vector {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        bytes
    }

    /// Reads a vector back from storage.
    ///
    /// Rejects a truncated or corrupt blob rather than producing a shorter
    /// vector, which would silently skew every similarity score computed
    /// against it.
    pub fn from_bytes(bytes: &[u8], model_id: impl Into<String>) -> CoreResult<Self> {
        if bytes.is_empty() || !bytes.len().is_multiple_of(4) {
            return Err(CoreError::storage(
                "stored embedding is truncated or malformed",
            ));
        }

        let vector: Vec<f32> = bytes
            .chunks_exact(4)
            .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
            .collect();

        Self::new(vector, model_id)
    }
}

/// Cosine similarity, in `-1.0..=1.0`.
///
/// Returns `0.0` — "unrelated" — for mismatched dimensions or a zero vector,
/// rather than erroring. A single unusable vector should degrade one candidate's
/// score, not fail an entire search.
pub fn cosine_similarity(left: &[f32], right: &[f32]) -> f32 {
    if left.len() != right.len() || left.is_empty() {
        return 0.0;
    }

    let mut dot = 0.0f32;
    let mut left_norm = 0.0f32;
    let mut right_norm = 0.0f32;

    for (a, b) in left.iter().zip(right.iter()) {
        dot += a * b;
        left_norm += a * a;
        right_norm += b * b;
    }

    let magnitude = left_norm.sqrt() * right_norm.sqrt();
    if magnitude == 0.0 || !magnitude.is_finite() {
        return 0.0;
    }

    (dot / magnitude).clamp(-1.0, 1.0)
}

/// A local text embedding model.
pub trait EmbeddingEngine: Send + Sync {
    fn health(&self) -> EngineHealth;

    /// Embeds one text.
    fn embed(&self, text: &str) -> CoreResult<Embedding>;

    /// Embeds several texts.
    ///
    /// Default implementation embeds one at a time. An engine that can batch
    /// should override it; correctness does not depend on which it does.
    fn embed_batch(&self, texts: &[String]) -> CoreResult<Vec<Embedding>> {
        texts.iter().map(|text| self.embed(text)).collect()
    }

    /// The model identifier stamped onto stored vectors.
    fn model_id(&self) -> String;
}

/// Bounds text before embedding it.
///
/// The embedding model has a small context; text beyond it is ignored by the
/// runtime anyway, so truncating here makes the behaviour explicit rather than
/// silently dependent on the runtime's own limit.
pub fn bounded_for_embedding(text: &str) -> CoreResult<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(CoreError::validation("cannot embed empty text"));
    }
    // ~512 tokens of English is roughly 2000 characters.
    Ok(trimmed.chars().take(2_000).collect())
}

impl EmbeddingEngine for crate::ai::llama::LlamaServerEngine {
    fn health(&self) -> EngineHealth {
        <Self as crate::ai::engine::LocalInferenceEngine>::health(self)
    }

    fn embed(&self, text: &str) -> CoreResult<Embedding> {
        let bounded = bounded_for_embedding(text)?;
        let vector = self.embed_one(&bounded)?;
        Embedding::new(vector, self.config().model_id.clone())
    }

    fn model_id(&self) -> String {
        self.config().model_id.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_embedding_round_trips_through_storage_bytes() {
        let original = Embedding::new(vec![0.5, -0.25, 1.0], "m").unwrap();
        let restored = Embedding::from_bytes(&original.to_bytes(), "m").unwrap();

        assert_eq!(restored, original);
        assert_eq!(restored.dimensions(), 3);
    }

    #[test]
    fn a_truncated_stored_vector_is_rejected_rather_than_shortened() {
        // Silently dropping a partial value would skew every later comparison.
        let bytes = vec![0u8; 10]; // Not a multiple of 4.
        assert!(Embedding::from_bytes(&bytes, "m").is_err());
        assert!(Embedding::from_bytes(&[], "m").is_err());
    }

    #[test]
    fn empty_or_non_finite_embeddings_are_refused() {
        assert!(Embedding::new(vec![], "m").is_err());
        assert!(Embedding::new(vec![1.0, f32::NAN], "m").is_err());
        assert!(Embedding::new(vec![f32::INFINITY], "m").is_err());
    }

    // --- Similarity --------------------------------------------------------

    #[test]
    fn identical_vectors_are_maximally_similar() {
        let v = vec![1.0, 2.0, 3.0];
        assert!((cosine_similarity(&v, &v) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn opposite_vectors_are_maximally_dissimilar() {
        let similarity = cosine_similarity(&[1.0, 0.0], &[-1.0, 0.0]);
        assert!((similarity + 1.0).abs() < 1e-6);
    }

    #[test]
    fn orthogonal_vectors_are_unrelated() {
        assert!(cosine_similarity(&[1.0, 0.0], &[0.0, 1.0]).abs() < 1e-6);
    }

    #[test]
    fn similarity_ignores_magnitude() {
        // Cosine compares direction; a longer document must not score higher
        // for being longer.
        let short = [1.0, 1.0];
        let long = [100.0, 100.0];
        assert!((cosine_similarity(&short, &long) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn mismatched_or_degenerate_vectors_score_zero_rather_than_failing() {
        // One unusable candidate must not fail an entire search.
        assert_eq!(cosine_similarity(&[1.0, 2.0], &[1.0]), 0.0);
        assert_eq!(cosine_similarity(&[], &[]), 0.0);
        assert_eq!(cosine_similarity(&[0.0, 0.0], &[1.0, 1.0]), 0.0);
    }

    #[test]
    fn similarity_stays_within_bounds_for_awkward_inputs() {
        let a = vec![1e20f32, 1e20];
        let b = vec![1e20f32, 1e20];
        let score = cosine_similarity(&a, &b);
        assert!((-1.0..=1.0).contains(&score), "got {score}");
    }

    #[test]
    fn ranking_by_similarity_puts_the_closest_first() {
        let query = [1.0, 0.0, 0.0];
        let candidates = [
            ("unrelated", vec![0.0, 1.0, 0.0]),
            ("exact", vec![1.0, 0.0, 0.0]),
            ("near", vec![0.9, 0.1, 0.0]),
        ];

        let mut scored: Vec<_> = candidates
            .iter()
            .map(|(name, v)| (*name, cosine_similarity(&query, v)))
            .collect();
        scored.sort_by(|a, b| b.1.total_cmp(&a.1));

        assert_eq!(scored[0].0, "exact");
        assert_eq!(scored[1].0, "near");
        assert_eq!(scored[2].0, "unrelated");
    }

    #[test]
    fn an_embedding_records_which_model_made_it() {
        // Vectors from different models are not comparable; the model ID is how
        // a mismatch is detected rather than silently mis-scored.
        let embedding = Embedding::new(vec![1.0], "bge-small-en-v1.5-q8_0").unwrap();
        assert_eq!(embedding.model_id, "bge-small-en-v1.5-q8_0");
    }
}
