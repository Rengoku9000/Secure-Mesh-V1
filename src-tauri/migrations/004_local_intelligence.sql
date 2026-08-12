-- SecureMesh Phase 3: local intelligence (migration 004).
--
-- Everything here is DERIVED. The authoritative record remains the signed event
-- log and the incidents projected from it; these tables hold what a local model
-- inferred, and dropping every row of them must leave the operational record
-- intact.
--
-- None of it is replicated. An inference is a local opinion produced by a
-- particular model at a particular time, and two nodes running different models
-- will legitimately disagree. Replicating opinions as though they were facts
-- would corrupt the one thing the mesh does guarantee.

-- What a local model concluded about an incident.
CREATE TABLE incident_analyses (
    -- One current analysis per incident. Re-analysing replaces it rather than
    -- accumulating, since an older opinion from the same or an earlier model is
    -- not evidence of anything.
    incident_id         TEXT PRIMARY KEY NOT NULL
                            REFERENCES incidents (id) ON DELETE CASCADE,
    category            TEXT NOT NULL CHECK (category IN (
                            'INFRASTRUCTURE', 'FLOODING', 'FIRE', 'MEDICAL',
                            'EVACUATION', 'POWER', 'COMMUNICATIONS',
                            'RESOURCE_SHORTAGE', 'EARTHQUAKE', 'SEVERE_WEATHER',
                            'ROAD_BLOCKAGE', 'OTHER')),
    -- The severity the MODEL assigned. Deliberately separate from
    -- incidents.severity, which an operator set: the two disagreeing is useful
    -- signal, so the model never overwrites the human judgement.
    severity            TEXT NOT NULL
                            CHECK (severity IN ('LOW', 'MEDIUM', 'HIGH', 'CRITICAL')),
    summary             TEXT NOT NULL CHECK (length(trim(summary)) > 0),
    asset               TEXT,
    cause               TEXT,
    access_status       TEXT NOT NULL
                            CHECK (access_status IN ('OPEN', 'RESTRICTED', 'BLOCKED', 'UNKNOWN')),
    -- JSON arrays. Small, read whole, and never queried by element, so a
    -- separate table would add joins for nothing.
    entities            TEXT NOT NULL DEFAULT '[]',
    affected_resources  TEXT NOT NULL DEFAULT '[]',
    location_hint       TEXT,
    -- Model-stated confidence, already clamped to 0..1 before it reaches here.
    confidence          REAL CHECK (confidence IS NULL OR (confidence BETWEEN 0.0 AND 1.0)),
    -- Which model produced this, so analyses can be invalidated when the model
    -- changes rather than silently mixing outputs from different models.
    model_id            TEXT NOT NULL,
    latency_ms          INTEGER NOT NULL CHECK (latency_ms >= 0),
    generated_at        TEXT NOT NULL
);

CREATE INDEX idx_analyses_category ON incident_analyses (category);
CREATE INDEX idx_analyses_model ON incident_analyses (model_id);

-- Locally provisioned reference material: procedures, guidelines, manuals.
--
-- Documents are operator-supplied files, never fetched by the application.
CREATE TABLE knowledge_documents (
    id           TEXT PRIMARY KEY NOT NULL,
    title        TEXT NOT NULL CHECK (length(trim(title)) > 0),
    -- Where it came from: a filename, a publication, an operator's note.
    source       TEXT NOT NULL,
    -- Licence or provenance, so a corpus can be audited for anything that
    -- should not have been ingested.
    source_type  TEXT NOT NULL,
    -- SHA-256 of the normalised text, so re-importing the same document is
    -- detectable rather than silently duplicating every chunk.
    content_hash TEXT NOT NULL UNIQUE,
    imported_at  TEXT NOT NULL
);

-- The retrievable unit. Chunks, not whole documents: a passage answers a
-- question, and embedding a whole manual as one vector retrieves nothing well.
CREATE TABLE knowledge_chunks (
    id           TEXT PRIMARY KEY NOT NULL,
    document_id  TEXT NOT NULL REFERENCES knowledge_documents (id) ON DELETE CASCADE,
    -- Position within the document, for stable ordering and citation.
    ordinal      INTEGER NOT NULL CHECK (ordinal >= 0),
    content      TEXT NOT NULL CHECK (length(trim(content)) > 0),

    UNIQUE (document_id, ordinal)
);

CREATE INDEX idx_chunks_document ON knowledge_chunks (document_id);

-- Vectors for every retrievable item.
--
-- One table for both knowledge chunks and incidents: retrieval must be able to
-- rank a procedure and a field report against the same question, and two tables
-- would mean two scans and two rankings to merge.
--
-- Stored as a BLOB of little-endian f32. A vector extension would mean a native
-- dependency on every target platform for a corpus this size; see
-- src/ai/embedding.rs for when that trade stops holding.
CREATE TABLE embeddings (
    id          TEXT PRIMARY KEY NOT NULL,
    -- What this vector represents.
    kind        TEXT NOT NULL CHECK (kind IN ('KNOWLEDGE_CHUNK', 'INCIDENT')),
    -- The chunk or incident it belongs to. Not a foreign key: it points into
    -- one of two tables depending on `kind`, and the alternative — two nullable
    -- columns with a CHECK enforcing exactly one — buys integrity at the cost
    -- of every query. Orphans are cleaned up on delete by the storage layer.
    subject_id  TEXT NOT NULL,
    vector      BLOB NOT NULL,
    -- Vectors from different models are not comparable. Recording the model
    -- lets a mismatch be detected instead of silently mis-scored.
    model_id    TEXT NOT NULL,
    dimensions  INTEGER NOT NULL CHECK (dimensions > 0),
    created_at  TEXT NOT NULL,

    UNIQUE (kind, subject_id, model_id)
);

CREATE INDEX idx_embeddings_lookup ON embeddings (kind, model_id);
