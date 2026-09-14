//! Persistence for derived intelligence, knowledge, and vectors.
//!
//! Everything here is rebuildable. If the model changes, or the analyses turn
//! out to be poor, every row can be deleted and the operational record is
//! untouched — that separation is the point, and it is why none of this is
//! replicated.

use super::{format_timestamp, parse_timestamp, Database};
use crate::ai::embedding::{cosine_similarity, Embedding};
use crate::domain::{AccessStatus, IncidentAnalysis, IncidentCategory};
use crate::error::{CoreError, CoreResult};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Largest number of candidates scored in one retrieval.
///
/// A brute-force scan is fine at this scale; the bound stops a pathological
/// corpus turning one question into an unbounded amount of work.
pub const MAX_RETRIEVAL_CANDIDATES: usize = 20_000;

/// What an embedding vector represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum EmbeddingKind {
    KnowledgeChunk,
    Incident,
}

impl EmbeddingKind {
    pub fn as_str(self) -> &'static str {
        match self {
            EmbeddingKind::KnowledgeChunk => "KNOWLEDGE_CHUNK",
            EmbeddingKind::Incident => "INCIDENT",
        }
    }
}

impl std::str::FromStr for EmbeddingKind {
    type Err = CoreError;

    fn from_str(value: &str) -> CoreResult<Self> {
        match value {
            "KNOWLEDGE_CHUNK" => Ok(EmbeddingKind::KnowledgeChunk),
            "INCIDENT" => Ok(EmbeddingKind::Incident),
            other => Err(CoreError::storage(format!(
                "database holds an unrecognised embedding kind: {other}"
            ))),
        }
    }
}

/// What kind of local knowledge a passage came from.
///
/// Distinct from [`EmbeddingKind`], which records which table a vector points
/// into. This records what the passage *is* to an operator reading a citation:
/// standing guidance, a live report, or a document someone loaded. A single
/// answer routinely cites more than one, and conflating them would let field
/// doctrine and an unverified field report appear identically sourced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PassageSource {
    /// From the provisioned operational knowledge pack: stable field guidance.
    OperationalKnowledge,
    /// From an incident recorded on this node or replicated from a peer:
    /// dynamic, unverified, and specific to one event.
    LiveIncident,
    /// From any other document an operator imported, including the synthetic
    /// evaluation corpus. Deliberately not folded into operational knowledge —
    /// this project does not get to promote arbitrary imports to doctrine.
    ImportedDocument,
}

impl PassageSource {
    pub fn as_str(self) -> &'static str {
        match self {
            PassageSource::OperationalKnowledge => "OPERATIONAL_KNOWLEDGE",
            PassageSource::LiveIncident => "LIVE_INCIDENT",
            PassageSource::ImportedDocument => "IMPORTED_DOCUMENT",
        }
    }

    /// Classifies a retrieved row from its embedding kind and, for a chunk, the
    /// `source_type` its document was imported under.
    fn classify(kind: EmbeddingKind, document_source_type: Option<&str>) -> Self {
        match kind {
            EmbeddingKind::Incident => PassageSource::LiveIncident,
            EmbeddingKind::KnowledgeChunk => {
                if document_source_type == Some(crate::ai::knowledge_pack::SOURCE_TYPE) {
                    PassageSource::OperationalKnowledge
                } else {
                    PassageSource::ImportedDocument
                }
            }
        }
    }
}

/// A locally provisioned reference document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeDocument {
    pub id: String,
    pub title: String,
    pub source: String,
    /// Licence or provenance, so a corpus can be audited.
    pub source_type: String,
    pub content_hash: String,
    pub imported_at: chrono::DateTime<chrono::Utc>,
    pub chunk_count: u64,
}

/// One retrievable passage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnowledgeChunk {
    pub id: String,
    pub document_id: String,
    pub ordinal: u32,
    pub content: String,
}

/// A retrieval hit, with enough provenance to cite it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RetrievedPassage {
    pub kind: EmbeddingKind,
    /// What this passage is to a reader: guidance, a live report, or an import.
    pub source: PassageSource,
    /// Chunk ID or incident ID.
    pub subject_id: String,
    pub content: String,
    /// Document title, or the incident's identifier.
    pub source_title: String,
    pub score: f32,
}

impl Database {
    // --- Derived intelligence ---------------------------------------------

    /// Stores an analysis, replacing any previous one for that incident.
    pub fn store_analysis(&self, analysis: &IncidentAnalysis) -> CoreResult<()> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO incident_analyses (
                 incident_id, category, severity, summary, asset, cause, access_status,
                 entities, affected_resources, location_hint, confidence,
                 model_id, latency_ms, generated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
             ON CONFLICT (incident_id) DO UPDATE SET
                 category           = excluded.category,
                 severity           = excluded.severity,
                 summary            = excluded.summary,
                 asset              = excluded.asset,
                 cause              = excluded.cause,
                 access_status      = excluded.access_status,
                 entities           = excluded.entities,
                 affected_resources = excluded.affected_resources,
                 location_hint      = excluded.location_hint,
                 confidence         = excluded.confidence,
                 model_id           = excluded.model_id,
                 latency_ms         = excluded.latency_ms,
                 generated_at       = excluded.generated_at",
            params![
                analysis.incident_id,
                analysis.category.as_str(),
                analysis.severity.as_str(),
                analysis.summary,
                analysis.asset,
                analysis.cause,
                analysis.access_status.as_str(),
                serde_json::to_string(&analysis.entities)?,
                serde_json::to_string(&analysis.affected_resources)?,
                analysis.location_hint,
                analysis.confidence,
                analysis.model_id,
                analysis.latency_ms as i64,
                format_timestamp(analysis.generated_at),
            ],
        )
        .map_err(|e| {
            if e.to_string().contains("FOREIGN KEY") {
                CoreError::not_found("cannot analyse an incident this node does not hold")
            } else {
                CoreError::from(e)
            }
        })?;
        Ok(())
    }

    /// The current analysis for an incident, if one exists.
    pub fn get_analysis(&self, incident_id: &str) -> CoreResult<Option<IncidentAnalysis>> {
        let conn = self.conn();
        let row = conn
            .query_row(
                "SELECT category, severity, summary, asset, cause, access_status,
                        entities, affected_resources, location_hint, confidence,
                        model_id, latency_ms, generated_at
                 FROM incident_analyses WHERE incident_id = ?1",
                params![incident_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, String>(7)?,
                        row.get::<_, Option<String>>(8)?,
                        row.get::<_, Option<f64>>(9)?,
                        row.get::<_, String>(10)?,
                        row.get::<_, i64>(11)?,
                        row.get::<_, String>(12)?,
                    ))
                },
            )
            .optional()?;

        let Some((
            category,
            severity,
            summary,
            asset,
            cause,
            access,
            entities,
            resources,
            hint,
            confidence,
            model_id,
            latency,
            generated,
        )) = row
        else {
            return Ok(None);
        };

        Ok(Some(IncidentAnalysis {
            incident_id: incident_id.to_string(),
            category: category.parse::<IncidentCategory>()?,
            severity: severity.parse()?,
            summary,
            asset,
            cause,
            access_status: access.parse::<AccessStatus>()?,
            // A malformed JSON column degrades to empty rather than failing the
            // read: a corrupt entity list must not make an incident unviewable.
            entities: serde_json::from_str(&entities).unwrap_or_default(),
            affected_resources: serde_json::from_str(&resources).unwrap_or_default(),
            location_hint: hint,
            confidence,
            model_id,
            latency_ms: latency.max(0) as u64,
            generated_at: parse_timestamp("generated_at", &generated)?,
        }))
    }

    pub fn count_analyses(&self) -> CoreResult<u64> {
        let conn = self.conn();
        let count: i64 =
            conn.query_row("SELECT count(*) FROM incident_analyses", [], |r| r.get(0))?;
        Ok(count.max(0) as u64)
    }

    /// Removes every analysis, for when the model changes.
    pub fn clear_analyses(&self) -> CoreResult<u64> {
        let conn = self.conn();
        let removed = conn.execute("DELETE FROM incident_analyses", [])?;
        Ok(removed as u64)
    }

    // --- Knowledge --------------------------------------------------------

    /// Imports a document and its chunks in one transaction.
    ///
    /// Returns `None` when a document with the same content hash is already
    /// present: re-importing the same file must not silently duplicate every
    /// chunk and skew retrieval towards it.
    pub fn import_document(
        &self,
        title: &str,
        source: &str,
        source_type: &str,
        content_hash: &str,
        chunks: &[String],
    ) -> CoreResult<Option<String>> {
        if chunks.is_empty() {
            return Err(CoreError::validation("a document must have content"));
        }

        let mut conn = self.conn();
        let transaction = conn.transaction()?;

        let existing: Option<String> = transaction
            .query_row(
                "SELECT id FROM knowledge_documents WHERE content_hash = ?1",
                params![content_hash],
                |row| row.get(0),
            )
            .optional()?;
        if existing.is_some() {
            return Ok(None);
        }

        let document_id = Uuid::new_v4().to_string();
        transaction.execute(
            "INSERT INTO knowledge_documents (id, title, source, source_type, content_hash, imported_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                document_id,
                title.trim(),
                source,
                source_type,
                content_hash,
                format_timestamp(crate::domain::now()),
            ],
        )?;

        for (ordinal, content) in chunks.iter().enumerate() {
            if content.trim().is_empty() {
                continue;
            }
            transaction.execute(
                "INSERT INTO knowledge_chunks (id, document_id, ordinal, content)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    Uuid::new_v4().to_string(),
                    document_id,
                    ordinal as i64,
                    content.trim(),
                ],
            )?;
        }

        transaction.commit()?;
        Ok(Some(document_id))
    }

    /// Chunks that have no embedding for the given model yet.
    ///
    /// Drives incremental indexing: embedding is slow, so it resumes rather
    /// than restarting after an interruption or a model change.
    pub fn chunks_awaiting_embedding(
        &self,
        model_id: &str,
        limit: u32,
    ) -> CoreResult<Vec<KnowledgeChunk>> {
        let conn = self.conn();
        let mut statement = conn.prepare(
            "SELECT c.id, c.document_id, c.ordinal, c.content
             FROM knowledge_chunks c
             WHERE NOT EXISTS (
                 SELECT 1 FROM embeddings e
                 WHERE e.kind = 'KNOWLEDGE_CHUNK'
                   AND e.subject_id = c.id
                   AND e.model_id = ?1
             )
             ORDER BY c.document_id, c.ordinal
             LIMIT ?2",
        )?;

        let rows = statement.query_map(params![model_id, limit.clamp(1, 10_000)], |row| {
            Ok(KnowledgeChunk {
                id: row.get(0)?,
                document_id: row.get(1)?,
                ordinal: row.get::<_, i64>(2)?.max(0) as u32,
                content: row.get(3)?,
            })
        })?;

        rows.collect::<Result<_, _>>().map_err(CoreError::from)
    }

    /// Incidents with no embedding for the given model yet.
    pub fn incidents_awaiting_embedding(
        &self,
        model_id: &str,
        limit: u32,
    ) -> CoreResult<Vec<(String, String)>> {
        let conn = self.conn();
        let mut statement = conn.prepare(
            "SELECT i.id, i.description
             FROM incidents i
             WHERE NOT EXISTS (
                 SELECT 1 FROM embeddings e
                 WHERE e.kind = 'INCIDENT'
                   AND e.subject_id = i.id
                   AND e.model_id = ?1
             )
             ORDER BY i.created_at DESC
             LIMIT ?2",
        )?;

        let rows = statement.query_map(params![model_id, limit.clamp(1, 10_000)], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;

        rows.collect::<Result<_, _>>().map_err(CoreError::from)
    }

    pub fn list_documents(&self) -> CoreResult<Vec<KnowledgeDocument>> {
        let conn = self.conn();
        let mut statement = conn.prepare(
            "SELECT d.id, d.title, d.source, d.source_type, d.content_hash, d.imported_at,
                    (SELECT count(*) FROM knowledge_chunks c WHERE c.document_id = d.id)
             FROM knowledge_documents d
             ORDER BY d.imported_at DESC",
        )?;

        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, i64>(6)?,
            ))
        })?;

        let collected: Vec<_> = rows.collect::<Result<_, _>>()?;
        drop(statement);
        drop(conn);

        collected
            .into_iter()
            .map(|(id, title, source, source_type, hash, imported, chunks)| {
                Ok(KnowledgeDocument {
                    id,
                    title,
                    source,
                    source_type,
                    content_hash: hash,
                    imported_at: parse_timestamp("imported_at", &imported)?,
                    chunk_count: chunks.max(0) as u64,
                })
            })
            .collect()
    }

    // --- Vectors ----------------------------------------------------------

    /// Stores a vector, replacing any previous one for the same subject and
    /// model. Re-embedding is therefore idempotent.
    pub fn store_embedding(
        &self,
        kind: EmbeddingKind,
        subject_id: &str,
        embedding: &Embedding,
    ) -> CoreResult<()> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO embeddings (id, kind, subject_id, vector, model_id, dimensions, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT (kind, subject_id, model_id) DO UPDATE SET
                 vector     = excluded.vector,
                 dimensions = excluded.dimensions,
                 created_at = excluded.created_at",
            params![
                Uuid::new_v4().to_string(),
                kind.as_str(),
                subject_id,
                embedding.to_bytes(),
                embedding.model_id,
                embedding.dimensions() as i64,
                format_timestamp(crate::domain::now()),
            ],
        )?;
        Ok(())
    }

    /// Ranks everything embedded with `model_id` against a query vector.
    ///
    /// Brute force by design; see `src/ai/embedding.rs` for why, and for when
    /// that stops being the right answer. Only vectors from the *same* model
    /// are considered — comparing across models produces scores that look
    /// plausible and mean nothing.
    pub fn search_embeddings(
        &self,
        query: &Embedding,
        top_k: usize,
        min_score: f32,
    ) -> CoreResult<Vec<RetrievedPassage>> {
        let top_k = top_k.clamp(1, 100);
        let conn = self.conn();

        // Joined here rather than looked up per hit: a second query per
        // candidate would dominate the cost of the scan itself.
        let mut statement = conn.prepare(
            "SELECT e.kind, e.subject_id, e.vector,
                    COALESCE(c.content, i.description, ''),
                    COALESCE(d.title, 'Incident ' || substr(i.id, 1, 8), ''),
                    d.source_type
             FROM embeddings e
             LEFT JOIN knowledge_chunks c
                    ON e.kind = 'KNOWLEDGE_CHUNK' AND c.id = e.subject_id
             LEFT JOIN knowledge_documents d ON d.id = c.document_id
             LEFT JOIN incidents i
                    ON e.kind = 'INCIDENT' AND i.id = e.subject_id
             WHERE e.model_id = ?1
             LIMIT ?2",
        )?;

        let rows = statement.query_map(
            params![query.model_id, MAX_RETRIEVAL_CANDIDATES as i64],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<String>>(5)?,
                ))
            },
        )?;

        let mut scored: Vec<RetrievedPassage> = Vec::new();
        for row in rows {
            let (kind, subject_id, bytes, content, title, document_source_type) = row?;

            // A corrupt stored vector skips that candidate rather than failing
            // the whole search.
            let Ok(candidate) = Embedding::from_bytes(&bytes, query.model_id.clone()) else {
                continue;
            };
            // Content missing means the subject was deleted and the vector is
            // orphaned; nothing useful can be cited from it.
            if content.trim().is_empty() {
                continue;
            }

            let score = cosine_similarity(&query.vector, &candidate.vector);
            if score < min_score {
                continue;
            }

            let kind = kind.parse::<EmbeddingKind>()?;
            scored.push(RetrievedPassage {
                kind,
                source: PassageSource::classify(kind, document_source_type.as_deref()),
                subject_id,
                content,
                source_title: title,
                score,
            });
        }

        // Descending by score; `total_cmp` orders NaN deterministically rather
        // than panicking as `partial_cmp().unwrap()` would.
        scored.sort_by(|a, b| b.score.total_cmp(&a.score));
        scored.truncate(top_k);
        Ok(scored)
    }

    /// The stored vector for one incident under one embedding model.
    ///
    /// `None` when the incident has not been indexed yet, or was indexed by a
    /// different model — vectors from another model are not comparable, so
    /// they are not returned as though they were.
    pub fn incident_embedding(
        &self,
        incident_id: &str,
        model_id: &str,
    ) -> CoreResult<Option<Embedding>> {
        let conn = self.conn();
        let bytes: Option<Vec<u8>> = conn
            .query_row(
                "SELECT vector FROM embeddings
                 WHERE kind = 'INCIDENT' AND subject_id = ?1 AND model_id = ?2",
                params![incident_id, model_id],
                |row| row.get(0),
            )
            .optional()?;

        // A corrupt blob reads as "not indexed" rather than failing the caller.
        Ok(bytes.and_then(|b| Embedding::from_bytes(&b, model_id).ok()))
    }

    /// Every incident vector under one model, for similarity between
    /// incidents.
    ///
    /// Read-only and bounded by [`MAX_RETRIEVAL_CANDIDATES`]. Vectors whose
    /// incident no longer exists are skipped by the join.
    pub fn incident_embeddings(&self, model_id: &str) -> CoreResult<Vec<(String, Embedding)>> {
        let conn = self.conn();
        let mut statement = conn.prepare(
            "SELECT e.subject_id, e.vector
             FROM embeddings e
             JOIN incidents i ON i.id = e.subject_id
             WHERE e.kind = 'INCIDENT' AND e.model_id = ?1
             LIMIT ?2",
        )?;

        let rows = statement.query_map(
            params![model_id, MAX_RETRIEVAL_CANDIDATES as i64],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?)),
        )?;

        let mut vectors = Vec::new();
        for row in rows {
            let (id, bytes) = row?;
            if let Ok(embedding) = Embedding::from_bytes(&bytes, model_id) {
                vectors.push((id, embedding));
            }
        }
        Ok(vectors)
    }

    pub fn count_embeddings(&self) -> CoreResult<u64> {
        let conn = self.conn();
        let count: i64 = conn.query_row("SELECT count(*) FROM embeddings", [], |r| r.get(0))?;
        Ok(count.max(0) as u64)
    }

    /// Vectors of one kind.
    ///
    /// "How many incidents are searchable" is derived from the vectors that
    /// exist, not from a status column, for the same reason
    /// `incidents_awaiting_embedding` is: a count that can disagree with what
    /// retrieval can find is worse than no count.
    pub fn count_embeddings_of_kind(&self, kind: EmbeddingKind) -> CoreResult<u64> {
        let conn = self.conn();
        let count: i64 = conn.query_row(
            "SELECT count(*) FROM embeddings WHERE kind = ?1",
            params![kind.as_str()],
            |row| row.get(0),
        )?;
        Ok(count.max(0) as u64)
    }

    pub fn count_knowledge_chunks(&self) -> CoreResult<u64> {
        let conn = self.conn();
        let count: i64 =
            conn.query_row("SELECT count(*) FROM knowledge_chunks", [], |r| r.get(0))?;
        Ok(count.max(0) as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{NewIncident, Severity};
    use crate::identity::keystore::FileKeyStore;
    use crate::identity::NodeIdentity;
    use tempfile::TempDir;

    struct Fixture {
        _dir: TempDir,
        db: Database,
        node_id: String,
    }

    fn fixture() -> Fixture {
        let dir = TempDir::new().unwrap();
        let identity =
            NodeIdentity::load_or_create(&FileKeyStore::new(dir.path().join("id.json"))).unwrap();
        let db = Database::open(dir.path().join("node.sqlite")).unwrap();
        db.register_local_node(
            identity.node_id(),
            identity.node_name(),
            &identity.public_key_hex(),
            identity.created_at(),
        )
        .unwrap();
        Fixture {
            _dir: dir,
            db,
            node_id: identity.node_id().to_string(),
        }
    }

    fn incident(f: &Fixture, description: &str) -> String {
        let validated = NewIncident {
            description: description.to_string(),
            severity: "HIGH".to_string(),
            latitude: None,
            longitude: None,
            accuracy_meters: None,
            location_source: None,
            location_captured_at: None,
        }
        .validate(&f.node_id)
        .unwrap();
        f.db.insert_incident(&validated).unwrap();
        validated.id
    }

    fn analysis(incident_id: &str) -> IncidentAnalysis {
        IncidentAnalysis {
            incident_id: incident_id.to_string(),
            category: IncidentCategory::Flooding,
            severity: Severity::High,
            summary: "Flooding in the northern zone.".to_string(),
            asset: Some("bridge".to_string()),
            cause: Some("rainfall".to_string()),
            access_status: AccessStatus::Blocked,
            entities: vec!["2 vehicles".to_string()],
            affected_resources: vec!["eastern road".to_string()],
            location_hint: Some("north".to_string()),
            confidence: Some(0.75),
            model_id: "test-model".to_string(),
            latency_ms: 1234,
            generated_at: crate::domain::now(),
        }
    }

    // --- Analyses ----------------------------------------------------------

    #[test]
    fn an_analysis_round_trips() {
        let f = fixture();
        let id = incident(&f, "Bridge flooded");
        let original = analysis(&id);

        f.db.store_analysis(&original).unwrap();
        let loaded = f.db.get_analysis(&id).unwrap().unwrap();

        assert_eq!(loaded, original);
    }

    #[test]
    fn an_unanalysed_incident_returns_nothing_rather_than_failing() {
        let f = fixture();
        let id = incident(&f, "Not analysed");
        assert!(f.db.get_analysis(&id).unwrap().is_none());
    }

    #[test]
    fn re_analysing_replaces_rather_than_accumulating() {
        let f = fixture();
        let id = incident(&f, "Bridge flooded");

        f.db.store_analysis(&analysis(&id)).unwrap();
        let mut revised = analysis(&id);
        revised.summary = "Revised by a newer model.".to_string();
        revised.model_id = "newer-model".to_string();
        f.db.store_analysis(&revised).unwrap();

        assert_eq!(f.db.count_analyses().unwrap(), 1);
        let loaded = f.db.get_analysis(&id).unwrap().unwrap();
        assert_eq!(loaded.model_id, "newer-model");
    }

    #[test]
    fn an_analysis_cannot_reference_an_incident_this_node_does_not_hold() {
        let f = fixture();
        let err =
            f.db.store_analysis(&analysis("no-such-incident"))
                .unwrap_err();
        assert_eq!(err.code(), "NOT_FOUND");
    }

    #[test]
    fn deleting_an_incident_removes_its_analysis() {
        let f = fixture();
        let id = incident(&f, "Temporary");
        f.db.store_analysis(&analysis(&id)).unwrap();

        f.db.conn()
            .execute("DELETE FROM incidents WHERE id = ?1", params![id])
            .unwrap();

        assert_eq!(f.db.count_analyses().unwrap(), 0);
    }

    #[test]
    fn analyses_can_be_cleared_without_touching_incidents() {
        let f = fixture();
        let id = incident(&f, "Kept");
        f.db.store_analysis(&analysis(&id)).unwrap();

        assert_eq!(f.db.clear_analyses().unwrap(), 1);
        assert_eq!(f.db.count_analyses().unwrap(), 0);
        // The operational record survives — the whole point of "derived".
        assert!(f.db.get_incident(&id).is_ok());
    }

    // --- Documents ---------------------------------------------------------

    #[test]
    fn a_document_and_its_chunks_are_imported_together() {
        let f = fixture();
        let id =
            f.db.import_document(
                "Flood Response",
                "manual.txt",
                "synthetic",
                "hash-1",
                &["first chunk".to_string(), "second chunk".to_string()],
            )
            .unwrap();

        assert!(id.is_some());
        assert_eq!(f.db.count_knowledge_chunks().unwrap(), 2);
        assert_eq!(f.db.list_documents().unwrap()[0].chunk_count, 2);
    }

    #[test]
    fn re_importing_identical_content_is_refused_rather_than_duplicated() {
        let f = fixture();
        let chunks = vec!["content".to_string()];

        assert!(f
            .db
            .import_document("Doc", "s", "synthetic", "same-hash", &chunks)
            .unwrap()
            .is_some());
        // Duplicated chunks would skew retrieval towards the repeated document.
        assert!(f
            .db
            .import_document("Doc again", "s", "synthetic", "same-hash", &chunks)
            .unwrap()
            .is_none());

        assert_eq!(f.db.count_knowledge_chunks().unwrap(), 1);
    }

    #[test]
    fn a_document_with_no_content_is_refused() {
        let f = fixture();
        assert!(f.db.import_document("Empty", "s", "t", "h", &[]).is_err());
    }

    #[test]
    fn blank_chunks_are_skipped_rather_than_stored() {
        let f = fixture();
        f.db.import_document(
            "Doc",
            "s",
            "t",
            "h",
            &["real".to_string(), "   ".to_string(), String::new()],
        )
        .unwrap();

        assert_eq!(f.db.count_knowledge_chunks().unwrap(), 1);
    }

    #[test]
    fn deleting_a_document_removes_its_chunks() {
        let f = fixture();
        let id =
            f.db.import_document("Doc", "s", "t", "h", &["a".to_string(), "b".to_string()])
                .unwrap()
                .unwrap();

        f.db.conn()
            .execute("DELETE FROM knowledge_documents WHERE id = ?1", params![id])
            .unwrap();

        assert_eq!(f.db.count_knowledge_chunks().unwrap(), 0);
    }

    // --- Embeddings and retrieval ------------------------------------------

    fn embed(values: Vec<f32>) -> Embedding {
        Embedding::new(values, "test-embed").unwrap()
    }

    #[test]
    fn indexing_resumes_rather_than_restarting() {
        let f = fixture();
        f.db.import_document("Doc", "s", "t", "h", &["a".to_string(), "b".to_string()])
            .unwrap();

        let pending = f.db.chunks_awaiting_embedding("test-embed", 100).unwrap();
        assert_eq!(pending.len(), 2);

        f.db.store_embedding(
            EmbeddingKind::KnowledgeChunk,
            &pending[0].id,
            &embed(vec![1.0, 0.0]),
        )
        .unwrap();

        // Only the un-embedded chunk remains outstanding.
        let remaining = f.db.chunks_awaiting_embedding("test-embed", 100).unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id, pending[1].id);
    }

    #[test]
    fn a_different_model_requires_re_embedding_everything() {
        let f = fixture();
        f.db.import_document("Doc", "s", "t", "h", &["a".to_string()])
            .unwrap();
        let chunk = f.db.chunks_awaiting_embedding("model-a", 10).unwrap()[0].clone();

        f.db.store_embedding(
            EmbeddingKind::KnowledgeChunk,
            &chunk.id,
            &Embedding::new(vec![1.0], "model-a").unwrap(),
        )
        .unwrap();

        assert!(f
            .db
            .chunks_awaiting_embedding("model-a", 10)
            .unwrap()
            .is_empty());
        // Vectors are not comparable across models, so a new model starts over.
        assert_eq!(
            f.db.chunks_awaiting_embedding("model-b", 10).unwrap().len(),
            1
        );
    }

    #[test]
    fn re_embedding_the_same_subject_replaces_the_vector() {
        let f = fixture();
        f.db.import_document("Doc", "s", "t", "h", &["a".to_string()])
            .unwrap();
        let chunk = f.db.chunks_awaiting_embedding("test-embed", 10).unwrap()[0].clone();

        f.db.store_embedding(
            EmbeddingKind::KnowledgeChunk,
            &chunk.id,
            &embed(vec![1.0, 0.0]),
        )
        .unwrap();
        f.db.store_embedding(
            EmbeddingKind::KnowledgeChunk,
            &chunk.id,
            &embed(vec![0.0, 1.0]),
        )
        .unwrap();

        assert_eq!(f.db.count_embeddings().unwrap(), 1);
    }

    #[test]
    fn retrieval_ranks_by_similarity_and_cites_the_document() {
        let f = fixture();
        f.db.import_document(
            "Flood Manual",
            "s",
            "synthetic",
            "h",
            &[
                "evacuate low ground".to_string(),
                "unrelated text".to_string(),
            ],
        )
        .unwrap();

        let chunks = f.db.chunks_awaiting_embedding("test-embed", 10).unwrap();
        f.db.store_embedding(
            EmbeddingKind::KnowledgeChunk,
            &chunks[0].id,
            &embed(vec![1.0, 0.0]),
        )
        .unwrap();
        f.db.store_embedding(
            EmbeddingKind::KnowledgeChunk,
            &chunks[1].id,
            &embed(vec![0.0, 1.0]),
        )
        .unwrap();

        let hits =
            f.db.search_embeddings(&embed(vec![1.0, 0.0]), 5, -1.0)
                .unwrap();

        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].content, "evacuate low ground");
        assert!(hits[0].score > hits[1].score);
        assert_eq!(hits[0].source_title, "Flood Manual");
        assert_eq!(hits[0].kind, EmbeddingKind::KnowledgeChunk);
    }

    #[test]
    fn retrieval_ignores_vectors_from_a_different_model() {
        // Scores across models look plausible and mean nothing, so such
        // vectors must not be returned at all.
        let f = fixture();
        f.db.import_document("Doc", "s", "t", "h", &["content".to_string()])
            .unwrap();
        let chunk = f.db.chunks_awaiting_embedding("model-a", 10).unwrap()[0].clone();

        f.db.store_embedding(
            EmbeddingKind::KnowledgeChunk,
            &chunk.id,
            &Embedding::new(vec![1.0, 0.0], "model-a").unwrap(),
        )
        .unwrap();

        let hits =
            f.db.search_embeddings(&Embedding::new(vec![1.0, 0.0], "model-b").unwrap(), 5, -1.0)
                .unwrap();
        assert!(hits.is_empty());
    }

    #[test]
    fn retrieval_can_rank_incidents_and_knowledge_together() {
        let f = fixture();
        let incident_id = incident(&f, "Bridge collapsed in the north");
        f.db.import_document("Manual", "s", "t", "h", &["bridge repair".to_string()])
            .unwrap();

        let chunk = f.db.chunks_awaiting_embedding("test-embed", 10).unwrap()[0].clone();
        f.db.store_embedding(
            EmbeddingKind::KnowledgeChunk,
            &chunk.id,
            &embed(vec![0.9, 0.1]),
        )
        .unwrap();
        f.db.store_embedding(
            EmbeddingKind::Incident,
            &incident_id,
            &embed(vec![1.0, 0.0]),
        )
        .unwrap();

        let hits =
            f.db.search_embeddings(&embed(vec![1.0, 0.0]), 5, -1.0)
                .unwrap();

        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].kind, EmbeddingKind::Incident);
        assert!(hits[0].source_title.starts_with("Incident "));
    }

    #[test]
    fn a_minimum_score_filters_weak_matches() {
        let f = fixture();
        f.db.import_document("Doc", "s", "t", "h", &["a".to_string(), "b".to_string()])
            .unwrap();
        let chunks = f.db.chunks_awaiting_embedding("test-embed", 10).unwrap();

        f.db.store_embedding(
            EmbeddingKind::KnowledgeChunk,
            &chunks[0].id,
            &embed(vec![1.0, 0.0]),
        )
        .unwrap();
        f.db.store_embedding(
            EmbeddingKind::KnowledgeChunk,
            &chunks[1].id,
            &embed(vec![-1.0, 0.0]),
        )
        .unwrap();

        let hits =
            f.db.search_embeddings(&embed(vec![1.0, 0.0]), 5, 0.5)
                .unwrap();
        assert_eq!(hits.len(), 1, "the opposite vector must be filtered out");
    }

    #[test]
    fn retrieval_is_bounded_by_top_k() {
        let f = fixture();
        let chunks: Vec<String> = (0..20).map(|n| format!("chunk {n}")).collect();
        f.db.import_document("Doc", "s", "t", "h", &chunks).unwrap();

        for chunk in f.db.chunks_awaiting_embedding("test-embed", 100).unwrap() {
            f.db.store_embedding(
                EmbeddingKind::KnowledgeChunk,
                &chunk.id,
                &embed(vec![1.0, 0.0]),
            )
            .unwrap();
        }

        assert_eq!(
            f.db.search_embeddings(&embed(vec![1.0, 0.0]), 3, -1.0)
                .unwrap()
                .len(),
            3
        );
    }

    #[test]
    fn an_empty_index_returns_no_hits_rather_than_failing() {
        let f = fixture();
        assert!(f
            .db
            .search_embeddings(&embed(vec![1.0]), 5, 0.0)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn an_orphaned_vector_is_skipped_rather_than_cited() {
        // Nothing useful can be quoted from a vector whose subject is gone.
        let f = fixture();
        f.db.store_embedding(
            EmbeddingKind::KnowledgeChunk,
            "ghost",
            &embed(vec![1.0, 0.0]),
        )
        .unwrap();

        assert!(f
            .db
            .search_embeddings(&embed(vec![1.0, 0.0]), 5, -1.0)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn a_corrupt_stored_vector_does_not_fail_the_search() {
        let f = fixture();
        f.db.import_document(
            "Doc",
            "s",
            "t",
            "h",
            &["good".to_string(), "also indexed".to_string()],
        )
        .unwrap();
        let chunks = f.db.chunks_awaiting_embedding("test-embed", 10).unwrap();

        f.db.store_embedding(
            EmbeddingKind::KnowledgeChunk,
            &chunks[0].id,
            &embed(vec![1.0, 0.0]),
        )
        .unwrap();

        // A truncated blob on the second chunk, as a partial write might leave.
        f.db.conn()
            .execute(
                "INSERT INTO embeddings (id, kind, subject_id, vector, model_id, dimensions, created_at)
                 VALUES ('bad', 'KNOWLEDGE_CHUNK', ?1, X'0102', 'test-embed', 2, '2026-01-01T00:00:00.000Z')",
                params![chunks[1].id],
            )
            .unwrap();

        let hits =
            f.db.search_embeddings(&embed(vec![1.0, 0.0]), 5, -1.0)
                .unwrap();
        assert_eq!(hits.len(), 1, "the good vector is still returned");
        assert_eq!(hits[0].content, "good");
    }

    #[test]
    fn incidents_awaiting_embedding_are_reported() {
        let f = fixture();
        incident(&f, "Needs embedding");

        let pending = f.db.incidents_awaiting_embedding("test-embed", 10).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].1, "Needs embedding");
    }
}
