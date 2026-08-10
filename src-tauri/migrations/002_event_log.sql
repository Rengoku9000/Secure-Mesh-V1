-- SecureMesh Phase 2: the replicated event log (migration 002).
--
-- Phase 1 stored incidents directly. From Phase 2 an incident is a *projection*
-- of the events that created it, and events are what nodes replicate. The
-- `incidents` table keeps exactly its Phase 1 shape, so nothing built on it
-- breaks; it is now written by the event apply path rather than by the command
-- layer directly.
--
-- Existing Phase 1 incidents are backfilled into this log at runtime, not here:
-- an event must be signed, and only the running node holds the key.
--
-- Note on `nodes`: this migration extends the existing table rather than
-- introducing a parallel `peers` table. One registry avoids two sources of
-- truth for "who is node X", and keeps the Phase 1 peer counts working
-- unchanged. Live connection state (including the transient CONNECTING) is
-- held in memory by the mesh service; only the last-known ONLINE/OFFLINE
-- status is persisted here.

-- The replicated log. Append-only: rows are inserted, never updated.
CREATE TABLE events (
    event_id           TEXT PRIMARY KEY NOT NULL,
    -- node_id of the author. Deliberately not a foreign key to `nodes`: an
    -- event may arrive relayed from a node we have never had a session with.
    -- It carries its own public key and is self-verifying, so it does not need
    -- prior registration in order to be trustworthy.
    origin_node        TEXT NOT NULL,
    -- Hex Ed25519 public key of the author (32 bytes = 64 hex characters).
    origin_public_key  TEXT NOT NULL CHECK (length(origin_public_key) = 64),
    -- Per-origin monotonic counter, starting at 1.
    origin_seq         INTEGER NOT NULL CHECK (origin_seq > 0),
    kind               TEXT NOT NULL
        CHECK (kind IN ('INCIDENT_CREATED', 'INCIDENT_OBSERVATION')),
    payload            TEXT NOT NULL,
    -- Author's wall clock. Display only; never used for ordering.
    created_at         TEXT NOT NULL,
    signature          TEXT NOT NULL,
    -- SHA-256 over the signed bytes, used to detect equivocation.
    content_hash       TEXT NOT NULL,
    -- When this node received it. Local only, never replicated.
    received_at        TEXT NOT NULL,

    -- A well-behaved origin issues each sequence number exactly once. This
    -- constraint is what turns equivocation into a detectable database error
    -- rather than silent divergence.
    UNIQUE (origin_node, origin_seq)
);

CREATE INDEX idx_events_origin ON events (origin_node, origin_seq);
CREATE INDEX idx_events_received ON events (received_at);

-- Detected equivocation: a second, different event claiming a sequence number
-- this node already holds. Both versions are preserved; nothing is overwritten.
CREATE TABLE event_conflicts (
    id                    TEXT PRIMARY KEY NOT NULL,
    origin_node           TEXT NOT NULL,
    origin_seq            INTEGER NOT NULL,
    -- The event already held, which is retained as authoritative.
    held_event_id         TEXT NOT NULL,
    held_content_hash     TEXT NOT NULL,
    -- The conflicting event, kept verbatim for audit rather than discarded.
    rejected_event_id     TEXT NOT NULL,
    rejected_content_hash TEXT NOT NULL,
    rejected_payload      TEXT NOT NULL,
    detected_at           TEXT NOT NULL,
    -- Which peer delivered the conflicting event, where known.
    reported_by           TEXT
);

CREATE INDEX idx_event_conflicts_origin ON event_conflicts (origin_node);

-- What this node holds, per origin.
--
-- `watermark` is the highest *contiguous* sequence number held: with events
-- 1, 2 and 4 present, the watermark is 2. Advancing only over a contiguous run
-- makes gap recovery automatic - the next sync request starts at watermark + 1
-- - and makes out-of-order delivery safe.
CREATE TABLE sync_watermarks (
    origin_node  TEXT PRIMARY KEY NOT NULL,
    watermark    INTEGER NOT NULL CHECK (watermark >= 0),
    updated_at   TEXT NOT NULL
);

-- What each peer has told us it holds, per origin.
--
-- This is the durable store-and-forward state. "Events pending for peer P" is
-- derived from it, so an undelivered event survives a restart: the queue is
-- the log itself plus this table, never an in-memory buffer.
CREATE TABLE peer_ack_watermarks (
    peer_node_id  TEXT NOT NULL,
    origin_node   TEXT NOT NULL,
    acked_through INTEGER NOT NULL CHECK (acked_through >= 0),
    updated_at    TEXT NOT NULL,

    PRIMARY KEY (peer_node_id, origin_node)
);

-- Observations appended to an incident: a projection of INCIDENT_OBSERVATION
-- events, exactly as `incidents` projects INCIDENT_CREATED.
CREATE TABLE incident_observations (
    id           TEXT PRIMARY KEY NOT NULL,
    incident_id  TEXT NOT NULL,
    author_node  TEXT NOT NULL,
    note         TEXT NOT NULL CHECK (length(trim(note)) > 0),
    created_at   TEXT NOT NULL,
    -- The event this observation was projected from.
    event_id     TEXT NOT NULL UNIQUE REFERENCES events (event_id)
);

CREATE INDEX idx_observations_incident ON incident_observations (incident_id);

-- Outbound protocol messages that could not be delivered.
--
-- Sync itself is pull-based and needs no queue - a peer asks for what it lacks
-- - so this covers non-sync messages and gives the dashboard a concrete
-- delivery state to report.
CREATE TABLE outbound_queue (
    id             TEXT PRIMARY KEY NOT NULL,
    peer_node_id   TEXT NOT NULL,
    message_kind   TEXT NOT NULL,
    payload        TEXT NOT NULL,
    delivery_state TEXT NOT NULL
        CHECK (delivery_state IN ('QUEUED', 'SENT', 'ACKNOWLEDGED', 'FAILED')),
    attempts       INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    queued_at      TEXT NOT NULL,
    updated_at     TEXT NOT NULL
);

CREATE INDEX idx_outbound_queue_peer ON outbound_queue (peer_node_id, delivery_state);

-- Extend the existing node registry with transport and trust metadata.
ALTER TABLE nodes ADD COLUMN transport_peer_id TEXT;
ALTER TABLE nodes ADD COLUMN protocol_version INTEGER;
ALTER TABLE nodes ADD COLUMN capabilities TEXT NOT NULL DEFAULT '[]';
-- Set when a node is caught signing two different events at one sequence
-- number. Records already accepted are kept; replication stops advancing.
ALTER TABLE nodes ADD COLUMN equivocating INTEGER NOT NULL DEFAULT 0;

-- Link an incident back to the event that created it, so the projection can be
-- rebuilt and so local records are distinguishable from replicated ones.
ALTER TABLE incidents ADD COLUMN origin_event_id TEXT REFERENCES events (event_id);

CREATE INDEX idx_incidents_origin_event ON incidents (origin_event_id);
