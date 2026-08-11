-- SecureMesh Phase 2.5: peer authorization (migration 003).
--
-- Phase 2 authenticated peers: a QUIC handshake proves a peer holds the private
-- key behind its node ID. It did not *authorize* them — any node on the network
-- that spoke the protocol was replicated with. This migration adds the
-- authorization decision, keyed by the cryptographic identity.
--
-- The trust columns extend `nodes` rather than forming a parallel table, for
-- the same reason Phase 2 extended it: one registry means one answer to "who is
-- node X", and no chance of two tables disagreeing about it.
--
-- FAIL CLOSED ON UPGRADE
-- ----------------------
-- Every existing peer becomes UNKNOWN, so a deployment upgraded from Phase 2
-- stops synchronising until its peers are explicitly approved. That is
-- disruptive on purpose: silently carrying forward implicit trust would defeat
-- the entire point of this phase. The local node is the one exception — it is
-- the operator's own device, and is bootstrapped as TRUSTED/ADMIN below.

-- Whether a peer is authorized. Distinct from `status`, which is reachability:
-- a peer can be ONLINE and REVOKED at once — the session exists, but nothing is
-- authorized to flow through it.
ALTER TABLE nodes ADD COLUMN trust_state TEXT NOT NULL DEFAULT 'UNKNOWN'
    CHECK (trust_state IN ('UNKNOWN', 'PENDING', 'TRUSTED', 'REVOKED'));

-- Capabilities are derived from the role in code, never stored per node, so a
-- stored list cannot drift out of step with the role it should reflect.
ALTER TABLE nodes ADD COLUMN peer_role TEXT NOT NULL DEFAULT 'NODE'
    CHECK (peer_role IN ('NODE', 'ADMIN'));

-- Provenance of the current decision. Retained through later transitions so a
-- revoked peer still shows who once approved it.
ALTER TABLE nodes ADD COLUMN enrolled_at  TEXT;
ALTER TABLE nodes ADD COLUMN enrolled_by  TEXT;
ALTER TABLE nodes ADD COLUMN revoked_at   TEXT;
ALTER TABLE nodes ADD COLUMN revoked_by   TEXT;
-- Free-text operator note, e.g. why a peer was refused.
ALTER TABLE nodes ADD COLUMN trust_notes  TEXT;

CREATE INDEX idx_nodes_trust_state ON nodes (trust_state);

-- The append-only trust audit log.
--
-- Deliberately NOT part of the replicated `events` log. Replicating trust
-- decisions would make one node's policy bind another's, which is a public key
-- infrastructure — a much larger problem than this phase solves, and one whose
-- failure modes are far worse than the limitation of keeping decisions local.
--
-- Rows are inserted, never updated or deleted. Revoking a peer adds an entry;
-- it never rewrites the entry that approved it.
CREATE TABLE peer_trust_events (
    id           TEXT PRIMARY KEY NOT NULL,
    -- Local monotonic ordering. Wall-clock time is recorded for display but
    -- does not order the log, for the same reason it does not order the
    -- replicated event log: device clocks drift and can step backwards.
    sequence     INTEGER NOT NULL UNIQUE CHECK (sequence > 0),
    -- The peer the decision concerns. A foreign key, so an authorization
    -- decision cannot reference a node this device holds no public key for.
    node_id      TEXT NOT NULL REFERENCES nodes (id) ON DELETE RESTRICT,
    kind         TEXT NOT NULL CHECK (kind IN (
                     'peer.enrollment.requested',
                     'peer.enrollment.approved',
                     'peer.enrollment.rejected',
                     'peer.revoked',
                     'peer.reinstated'
                 )),
    from_state   TEXT CHECK (from_state IN ('UNKNOWN', 'PENDING', 'TRUSTED', 'REVOKED')),
    to_state     TEXT NOT NULL CHECK (to_state IN ('UNKNOWN', 'PENDING', 'TRUSTED', 'REVOKED')),
    -- Which node's operator made the decision.
    actor_node   TEXT NOT NULL,
    occurred_at  TEXT NOT NULL,
    detail       TEXT,
    -- Ed25519 signature by the deciding node over the canonical encoding of
    -- this record, making the audit log tamper-evident rather than merely
    -- append-only by convention.
    signature    TEXT NOT NULL
);

CREATE INDEX idx_peer_trust_events_node ON peer_trust_events (node_id, sequence);

-- Bootstrap the root of authority.
--
-- The local node administers its own trust store. This is not a claim of
-- cryptographic authority over anything else: it records that the operator of
-- this device decides what this device accepts, which is true whether or not it
-- is written down. Documented in docs/security/SECURITY.md.
UPDATE nodes SET trust_state = 'TRUSTED', peer_role = 'ADMIN' WHERE status = 'LOCAL';
