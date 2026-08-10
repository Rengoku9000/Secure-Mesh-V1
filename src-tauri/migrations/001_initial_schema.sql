-- SecureMesh initial schema (migration 001).
--
-- Conventions:
--   * Primary keys are UUIDv4 strings, so records created on different nodes
--     while offline never collide and can be merged by the Phase 2 sync engine
--     without a coordinating server.
--   * Timestamps are RFC 3339 strings in UTC. Text keeps the database portable
--     and human-inspectable in the field; UTC avoids ambiguity across the
--     timezones a deployed mesh may span.
--   * Enumerated columns carry CHECK constraints so the database rejects
--     invalid states even if a future code path forgets to validate.

CREATE TABLE nodes (
    id          TEXT PRIMARY KEY NOT NULL,
    node_name   TEXT NOT NULL,
    -- Hex-encoded Ed25519 verifying key. Public by design; no private key
    -- material is ever stored in the database.
    public_key  TEXT NOT NULL UNIQUE,
    status      TEXT NOT NULL CHECK (status IN ('LOCAL', 'ONLINE', 'OFFLINE')),
    last_seen   TEXT,
    created_at  TEXT NOT NULL
);

CREATE TABLE incidents (
    id           TEXT PRIMARY KEY NOT NULL,
    -- Authorship is a foreign key, so an incident can never be attributed to a
    -- node this device has no public key for. Phase 2 must therefore register
    -- a peer before accepting records signed by it.
    created_by   TEXT NOT NULL REFERENCES nodes (id) ON DELETE RESTRICT,
    description  TEXT NOT NULL CHECK (length(trim(description)) > 0),
    severity     TEXT NOT NULL CHECK (severity IN ('LOW', 'MEDIUM', 'HIGH', 'CRITICAL')),
    latitude     REAL,
    longitude    REAL,
    created_at   TEXT NOT NULL,
    updated_at   TEXT NOT NULL,
    sync_status  TEXT NOT NULL CHECK (sync_status IN ('PENDING', 'SYNCING', 'SYNCED', 'FAILED')),

    -- Coordinates are stored as a pair or not at all.
    CHECK ((latitude IS NULL) = (longitude IS NULL)),
    CHECK (latitude IS NULL OR (latitude BETWEEN -90.0 AND 90.0)),
    CHECK (longitude IS NULL OR (longitude BETWEEN -180.0 AND 180.0))
);

-- The dashboard reads the newest incidents first.
CREATE INDEX idx_incidents_created_at ON incidents (created_at DESC);
-- The Phase 2 sync engine scans for records still awaiting propagation.
CREATE INDEX idx_incidents_sync_status ON incidents (sync_status);

CREATE TABLE messages (
    id               TEXT PRIMARY KEY NOT NULL,
    sender_id        TEXT NOT NULL,
    -- NULL addresses a broadcast to the whole mesh.
    receiver_id      TEXT,
    -- BLOB because Phase 2 payloads are ciphertext, not text.
    payload          BLOB NOT NULL,
    created_at       TEXT NOT NULL,
    delivery_status  TEXT NOT NULL
        CHECK (delivery_status IN ('QUEUED', 'SENT', 'DELIVERED', 'FAILED'))
);

CREATE INDEX idx_messages_delivery_status ON messages (delivery_status);

CREATE TABLE sync_events (
    id           TEXT PRIMARY KEY NOT NULL,
    event_type   TEXT NOT NULL,
    object_id    TEXT NOT NULL,
    source_node  TEXT NOT NULL,
    created_at   TEXT NOT NULL,
    sync_status  TEXT NOT NULL CHECK (sync_status IN ('PENDING', 'SYNCING', 'SYNCED', 'FAILED'))
);

CREATE INDEX idx_sync_events_object ON sync_events (object_id);
