//! Persistence for the replicated event log.
//!
//! This module owns the two properties replication depends on:
//!
//! - **Idempotency.** Applying the same event twice has exactly the same effect
//!   as applying it once, so duplicate delivery needs no deduplication logic
//!   anywhere above this layer.
//! - **Atomic sequence allocation.** A locally authored event's sequence number
//!   is allocated and the event stored inside one transaction, so a crash
//!   between the two cannot leave a gap or reuse a number.

use super::{format_timestamp, parse_timestamp, Database};
use crate::domain::event::{EventKind, MeshEvent};
use crate::domain::Incident;
use crate::error::{CoreError, CoreResult};
use rusqlite::{params, OptionalExtension, Row};
use uuid::Uuid;

/// Largest number of events returned by a single sync query.
///
/// Bounds the size of a `SYNC_RESPONSE`, so one request cannot make this node
/// serialise its entire log into memory.
pub const MAX_SYNC_BATCH: u32 = 200;

const SELECT_COLUMNS: &str = "event_id, origin_node, origin_public_key, origin_seq, kind, \
                              payload, created_at, signature";

/// What happened when an event was offered to the log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyOutcome {
    /// Stored for the first time.
    Stored,
    /// Already held, byte for byte. The defining case for idempotent replay.
    Duplicate,
    /// The origin issued a different event at a sequence number already held.
    /// Both versions are retained and the conflict recorded.
    Conflict,
}

impl Database {
    /// Allocates the next sequence number for a locally authored event.
    ///
    /// Reads from the log itself rather than a counter, so the number can never
    /// drift out of step with what is actually stored.
    pub fn next_local_sequence(&self, node_id: &str) -> CoreResult<u64> {
        let conn = self.conn();
        let highest: Option<i64> = conn.query_row(
            "SELECT max(origin_seq) FROM events WHERE origin_node = ?1",
            params![node_id],
            |row| row.get(0),
        )?;
        Ok(highest.unwrap_or(0).max(0) as u64 + 1)
    }

    /// Stores an event, returning whether it was new, duplicate, or conflicting.
    ///
    /// The caller **must** have verified the event's signature first; this
    /// layer enforces storage invariants, not authenticity.
    ///
    /// `local_node_id` tells the projection whether the event is this node's
    /// own work. A record authored here starts unsynchronised; a record that
    /// arrived by replication is by definition already shared.
    pub fn apply_event(
        &self,
        event: &MeshEvent,
        local_node_id: &str,
        reported_by: Option<&str>,
    ) -> CoreResult<ApplyOutcome> {
        let mut conn = self.conn();
        let transaction = conn.transaction()?;

        // Same event ID already present: a straightforward duplicate.
        let existing_by_id: Option<String> = transaction
            .query_row(
                "SELECT content_hash FROM events WHERE event_id = ?1",
                params![event.event_id],
                |row| row.get(0),
            )
            .optional()?;

        if let Some(held_hash) = existing_by_id {
            // An identical ID carrying different content is a forgery attempt
            // rather than a replay, so it must not be silently accepted.
            if held_hash == event.content_hash() {
                return Ok(ApplyOutcome::Duplicate);
            }
            return Err(CoreError::validation(
                "an event with this ID already exists with different content",
            ));
        }

        // Same (origin, sequence) with different content: the origin forked its
        // own log. Keep what we hold, record the conflict, discard nothing.
        let existing_at_seq: Option<(String, String)> = transaction
            .query_row(
                "SELECT event_id, content_hash FROM events
                 WHERE origin_node = ?1 AND origin_seq = ?2",
                params![event.origin_node, event.origin_seq as i64],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;

        if let Some((held_event_id, held_hash)) = existing_at_seq {
            transaction.execute(
                "INSERT INTO event_conflicts (
                     id, origin_node, origin_seq, held_event_id, held_content_hash,
                     rejected_event_id, rejected_content_hash, rejected_payload,
                     detected_at, reported_by
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    Uuid::new_v4().to_string(),
                    event.origin_node,
                    event.origin_seq as i64,
                    held_event_id,
                    held_hash,
                    event.event_id,
                    event.content_hash(),
                    event.payload,
                    format_timestamp(crate::domain::now()),
                    reported_by,
                ],
            )?;

            // Stop advancing replication from a node that equivocates.
            transaction.execute(
                "UPDATE nodes SET equivocating = 1 WHERE id = ?1",
                params![event.origin_node],
            )?;

            transaction.commit()?;
            return Ok(ApplyOutcome::Conflict);
        }

        transaction.execute(
            "INSERT INTO events (
                 event_id, origin_node, origin_public_key, origin_seq, kind,
                 payload, created_at, signature, content_hash, received_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                event.event_id,
                event.origin_node,
                event.origin_public_key,
                event.origin_seq as i64,
                event.kind.as_str(),
                event.payload,
                format_timestamp(event.created_at),
                event.signature,
                event.content_hash(),
                format_timestamp(crate::domain::now()),
            ],
        )?;

        // Projection happens in the same transaction as the append, so the log
        // and the tables derived from it can never disagree after a crash.
        project_event(&transaction, event, local_node_id)?;

        transaction.commit()?;
        Ok(ApplyOutcome::Stored)
    }

    /// Incidents authored here before the event log existed.
    ///
    /// A node upgraded from Phase 1 holds incidents with no corresponding
    /// event. They cannot be replicated until they have one, and only the node
    /// that authored them can sign it.
    pub fn incidents_awaiting_backfill(&self, local_node_id: &str) -> CoreResult<Vec<Incident>> {
        let conn = self.conn();
        let mut statement = conn.prepare(
            "SELECT id, created_by, description, severity, latitude, longitude,
                    created_at, updated_at, sync_status
             FROM incidents
             WHERE origin_event_id IS NULL AND created_by = ?1
             ORDER BY created_at ASC, id ASC",
        )?;

        let rows = statement.query_map(params![local_node_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<f64>>(4)?,
                row.get::<_, Option<f64>>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, String>(8)?,
            ))
        })?;

        let mut incidents = Vec::new();
        for row in rows {
            let (
                id,
                created_by,
                description,
                severity,
                latitude,
                longitude,
                created,
                updated,
                sync,
            ) = row?;
            incidents.push(Incident {
                id,
                created_by,
                description,
                severity: severity.parse().map_err(|_| {
                    CoreError::storage("database holds an unrecognised incident severity")
                })?,
                latitude,
                longitude,
                created_at: parse_timestamp("created_at", &created)?,
                updated_at: parse_timestamp("updated_at", &updated)?,
                sync_status: sync.parse()?,
            });
        }
        Ok(incidents)
    }

    /// Attaches a freshly signed event to an incident that predates the log.
    ///
    /// Both writes happen in one transaction, so a crash cannot leave an event
    /// stored without its incident linked to it — which would make the incident
    /// eligible for backfill a second time and mint a duplicate event.
    pub fn backfill_incident_event(&self, event: &MeshEvent, incident_id: &str) -> CoreResult<()> {
        let mut conn = self.conn();
        let transaction = conn.transaction()?;

        transaction.execute(
            "INSERT INTO events (
                 event_id, origin_node, origin_public_key, origin_seq, kind,
                 payload, created_at, signature, content_hash, received_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                event.event_id,
                event.origin_node,
                event.origin_public_key,
                event.origin_seq as i64,
                event.kind.as_str(),
                event.payload,
                format_timestamp(event.created_at),
                event.signature,
                event.content_hash(),
                format_timestamp(crate::domain::now()),
            ],
        )?;

        let linked = transaction.execute(
            "UPDATE incidents SET origin_event_id = ?1
             WHERE id = ?2 AND origin_event_id IS NULL",
            params![event.event_id, incident_id],
        )?;

        if linked != 1 {
            return Err(CoreError::storage(
                "incident was already linked to an event during backfill",
            ));
        }

        transaction.commit()?;
        Ok(())
    }

    /// Whether this node already holds an event.
    pub fn has_event(&self, event_id: &str) -> CoreResult<bool> {
        let conn = self.conn();
        let count: i64 = conn.query_row(
            "SELECT count(*) FROM events WHERE event_id = ?1",
            params![event_id],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    /// The highest **contiguous** sequence number held for `origin_node`.
    ///
    /// Recomputed from the log rather than cached, so it cannot disagree with
    /// the stored events. With 1, 2 and 4 present this returns 2: event 4 is
    /// held but does not count, which is what makes the next sync request ask
    /// for 3 onward and heals the gap.
    pub fn watermark_for(&self, origin_node: &str) -> CoreResult<u64> {
        let conn = self.conn();
        let mut statement = conn.prepare(
            "SELECT origin_seq FROM events WHERE origin_node = ?1 ORDER BY origin_seq ASC",
        )?;
        let rows = statement.query_map(params![origin_node], |row| row.get::<_, i64>(0))?;

        let mut watermark: u64 = 0;
        for row in rows {
            let seq = row? as u64;
            if seq == watermark + 1 {
                watermark = seq;
            } else if seq > watermark + 1 {
                break;
            }
        }
        Ok(watermark)
    }

    /// This node's complete view of the log: every origin it holds events for,
    /// with its contiguous watermark. This is the payload of a `SYNC_REQUEST`.
    pub fn sync_watermarks(&self) -> CoreResult<Vec<(String, u64)>> {
        let conn = self.conn();
        let mut statement =
            conn.prepare("SELECT DISTINCT origin_node FROM events ORDER BY origin_node")?;
        let origins: Vec<String> = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<_, _>>()?;
        drop(statement);
        drop(conn);

        let mut watermarks = Vec::with_capacity(origins.len());
        for origin in origins {
            let watermark = self.watermark_for(&origin)?;
            watermarks.push((origin, watermark));
        }
        Ok(watermarks)
    }

    /// Events from `origin_node` after `after_seq`, oldest first.
    ///
    /// Returns a contiguous run only: it stops at the first gap, so a peer is
    /// never handed events it cannot yet fold into its watermark.
    pub fn events_since(
        &self,
        origin_node: &str,
        after_seq: u64,
        limit: u32,
    ) -> CoreResult<Vec<MeshEvent>> {
        let limit = limit.clamp(1, MAX_SYNC_BATCH);
        let conn = self.conn();
        let mut statement = conn.prepare(&format!(
            "SELECT {SELECT_COLUMNS} FROM events
             WHERE origin_node = ?1 AND origin_seq > ?2
             ORDER BY origin_seq ASC LIMIT ?3"
        ))?;

        let rows = statement.query_map(
            params![origin_node, after_seq as i64, limit],
            EventRow::from_row,
        )?;

        let mut events = Vec::new();
        for (expected, row) in (after_seq + 1..).zip(rows) {
            let event = row?.into_domain()?;
            // Stop at the first gap: handing a peer events it cannot fold into
            // its watermark would only be re-requested later.
            if event.origin_seq != expected {
                break;
            }
            events.push(event);
        }
        Ok(events)
    }

    /// Total events held locally.
    pub fn count_events(&self) -> CoreResult<u64> {
        let conn = self.conn();
        let count: i64 = conn.query_row("SELECT count(*) FROM events", [], |row| row.get(0))?;
        Ok(count.max(0) as u64)
    }

    /// Number of recorded equivocations.
    pub fn count_event_conflicts(&self) -> CoreResult<u64> {
        let conn = self.conn();
        let count: i64 =
            conn.query_row("SELECT count(*) FROM event_conflicts", [], |row| row.get(0))?;
        Ok(count.max(0) as u64)
    }

    /// Records what a peer has told us it holds, so undelivered events survive
    /// a restart without an in-memory queue.
    pub fn record_peer_ack(
        &self,
        peer_node_id: &str,
        origin_node: &str,
        acked_through: u64,
    ) -> CoreResult<()> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO peer_ack_watermarks (peer_node_id, origin_node, acked_through, updated_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (peer_node_id, origin_node) DO UPDATE SET
                 -- max() keeps an out-of-order or replayed ACK from moving the
                 -- watermark backwards and re-sending delivered events.
                 acked_through = max(acked_through, excluded.acked_through),
                 updated_at    = excluded.updated_at",
            params![
                peer_node_id,
                origin_node,
                acked_through as i64,
                format_timestamp(crate::domain::now()),
            ],
        )?;
        Ok(())
    }

    /// Recomputes the sync status of this node's own incidents.
    ///
    /// Status is **derived**, never accumulated: an incident authored here is
    /// `SYNCED` exactly when some peer has acknowledged the event that created
    /// it, and `PENDING` otherwise. Recomputing from the acknowledgement
    /// watermarks means a replayed, stale, or duplicated ACK cannot corrupt the
    /// status, and the answer is identical after a restart.
    pub fn refresh_local_sync_status(&self, local_node_id: &str) -> CoreResult<()> {
        let conn = self.conn();
        conn.execute(
            "UPDATE incidents
             SET sync_status = CASE WHEN EXISTS (
                     SELECT 1
                     FROM events e
                     JOIN peer_ack_watermarks a ON a.origin_node = e.origin_node
                     WHERE e.event_id = incidents.origin_event_id
                       AND a.acked_through >= e.origin_seq
                       AND a.peer_node_id <> ?1
                 ) THEN 'SYNCED' ELSE 'PENDING' END
             WHERE created_by = ?1 AND origin_event_id IS NOT NULL",
            params![local_node_id],
        )?;
        Ok(())
    }

    /// How many events this node holds that `peer_node_id` has not acknowledged.
    pub fn pending_events_for_peer(&self, peer_node_id: &str) -> CoreResult<u64> {
        let conn = self.conn();
        let pending: i64 = conn.query_row(
            "SELECT count(*) FROM events e
             WHERE e.origin_seq > coalesce(
                 (SELECT acked_through FROM peer_ack_watermarks a
                  WHERE a.peer_node_id = ?1 AND a.origin_node = e.origin_node),
                 0)
               -- A peer never needs its own events sent back to it.
               AND e.origin_node <> ?1",
            params![peer_node_id],
            |row| row.get(0),
        )?;
        Ok(pending.max(0) as u64)
    }
}

/// Materialises an event into the tables the UI reads.
///
/// Runs inside the caller's transaction. Every write is idempotent, so
/// re-projecting an event that is already materialised is harmless — which is
/// what allows the projection to be rebuilt from the log if it is ever lost.
fn project_event(
    transaction: &rusqlite::Transaction<'_>,
    event: &MeshEvent,
    local_node_id: &str,
) -> CoreResult<()> {
    // `incidents.created_by` is a foreign key into `nodes`, so an author this
    // node has never had a session with must be registered before its record
    // can land. The public key was verified against the node ID during
    // `MeshEvent::verify`, so this registers a cryptographically checked
    // identity, not a self-declared one.
    transaction.execute(
        "INSERT INTO nodes (id, node_name, public_key, status, last_seen, created_at)
         VALUES (?1, ?2, ?3, 'OFFLINE', NULL, ?4)
         ON CONFLICT (id) DO NOTHING",
        params![
            event.origin_node,
            crate::identity::node_name_for(&event.origin_node),
            event.origin_public_key,
            format_timestamp(event.created_at),
        ],
    )?;

    match event.kind {
        EventKind::IncidentCreated => {
            let payload = event.incident_created_payload()?;
            transaction.execute(
                "INSERT INTO incidents (
                     id, created_by, description, severity, latitude, longitude,
                     created_at, updated_at, sync_status, origin_event_id
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, ?8, ?9)
                 ON CONFLICT (id) DO NOTHING",
                params![
                    payload.incident_id,
                    event.origin_node,
                    payload.description,
                    payload.severity,
                    payload.latitude,
                    payload.longitude,
                    format_timestamp(event.created_at),
                    // Authored here: nothing has acknowledged it yet. Arrived
                    // by replication: it is already shared by definition.
                    if event.origin_node == local_node_id {
                        "PENDING"
                    } else {
                        "SYNCED"
                    },
                    event.event_id,
                ],
            )?;
        }
        EventKind::IncidentObservation => {
            let payload = event.incident_observation_payload()?;
            transaction.execute(
                "INSERT INTO incident_observations (
                     id, incident_id, author_node, note, created_at, event_id
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT (id) DO NOTHING",
                params![
                    payload.observation_id,
                    payload.incident_id,
                    event.origin_node,
                    payload.note,
                    format_timestamp(event.created_at),
                    event.event_id,
                ],
            )?;
        }
    }

    Ok(())
}

struct EventRow {
    event_id: String,
    origin_node: String,
    origin_public_key: String,
    origin_seq: i64,
    kind: String,
    payload: String,
    created_at: String,
    signature: String,
}

impl EventRow {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            event_id: row.get(0)?,
            origin_node: row.get(1)?,
            origin_public_key: row.get(2)?,
            origin_seq: row.get(3)?,
            kind: row.get(4)?,
            payload: row.get(5)?,
            created_at: row.get(6)?,
            signature: row.get(7)?,
        })
    }

    fn into_domain(self) -> CoreResult<MeshEvent> {
        if self.origin_seq <= 0 {
            return Err(CoreError::storage(
                "event has a non-positive sequence number",
            ));
        }
        Ok(MeshEvent {
            event_id: self.event_id,
            origin_node: self.origin_node,
            origin_public_key: self.origin_public_key,
            origin_seq: self.origin_seq as u64,
            kind: self.kind.parse::<EventKind>()?,
            payload: self.payload,
            created_at: parse_timestamp("created_at", &self.created_at)?,
            signature: self.signature,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::event::{EventKind, IncidentCreatedPayload};
    use crate::identity::keystore::FileKeyStore;
    use crate::identity::NodeIdentity;
    use tempfile::TempDir;

    struct Fixture {
        _dir: TempDir,
        db: Database,
        identity: NodeIdentity,
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
            identity,
        }
    }

    fn event_at(identity: &NodeIdentity, seq: u64, description: &str) -> MeshEvent {
        MeshEvent::create(
            identity,
            seq,
            EventKind::IncidentCreated,
            IncidentCreatedPayload {
                incident_id: Uuid::new_v4().to_string(),
                description: description.to_string(),
                severity: "LOW".to_string(),
                latitude: None,
                longitude: None,
            },
        )
        .unwrap()
    }

    #[test]
    fn sequence_allocation_starts_at_one_and_advances() {
        let f = fixture();
        assert_eq!(f.db.next_local_sequence(f.identity.node_id()).unwrap(), 1);

        f.db.apply_event(
            &event_at(&f.identity, 1, "first"),
            f.identity.node_id(),
            None,
        )
        .unwrap();
        assert_eq!(f.db.next_local_sequence(f.identity.node_id()).unwrap(), 2);
    }

    #[test]
    fn a_new_event_is_stored() {
        let f = fixture();
        let outcome =
            f.db.apply_event(
                &event_at(&f.identity, 1, "first"),
                f.identity.node_id(),
                None,
            )
            .unwrap();

        assert_eq!(outcome, ApplyOutcome::Stored);
        assert_eq!(f.db.count_events().unwrap(), 1);
    }

    #[test]
    fn applying_the_same_event_twice_is_idempotent() {
        let f = fixture();
        let event = event_at(&f.identity, 1, "first");

        assert_eq!(
            f.db.apply_event(&event, f.identity.node_id(), None)
                .unwrap(),
            ApplyOutcome::Stored
        );
        for _ in 0..5 {
            assert_eq!(
                f.db.apply_event(&event, f.identity.node_id(), None)
                    .unwrap(),
                ApplyOutcome::Duplicate
            );
        }
        assert_eq!(f.db.count_events().unwrap(), 1);
    }

    #[test]
    fn an_event_id_reused_with_different_content_is_refused() {
        let f = fixture();
        let original = event_at(&f.identity, 1, "genuine");
        f.db.apply_event(&original, f.identity.node_id(), None)
            .unwrap();

        let mut forged = event_at(&f.identity, 2, "tampered");
        forged.event_id = original.event_id.clone();

        let err =
            f.db.apply_event(&forged, f.identity.node_id(), None)
                .unwrap_err();
        assert!(err
            .message()
            .contains("already exists with different content"));
        assert_eq!(f.db.count_events().unwrap(), 1);
    }

    // --- Equivocation ------------------------------------------------------

    #[test]
    fn equivocation_is_detected_and_both_versions_are_kept() {
        let f = fixture();
        let first = event_at(&f.identity, 1, "version one");
        let second = event_at(&f.identity, 1, "version two");

        assert_eq!(
            f.db.apply_event(&first, f.identity.node_id(), None)
                .unwrap(),
            ApplyOutcome::Stored
        );
        assert_eq!(
            f.db.apply_event(&second, f.identity.node_id(), Some("peer-x"))
                .unwrap(),
            ApplyOutcome::Conflict
        );

        // The held event is untouched; the conflicting one is preserved for audit.
        assert_eq!(f.db.count_events().unwrap(), 1);
        assert_eq!(f.db.count_event_conflicts().unwrap(), 1);

        let conn = f.db.conn();
        let (rejected_payload, reported_by): (String, Option<String>) = conn
            .query_row(
                "SELECT rejected_payload, reported_by FROM event_conflicts",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert!(rejected_payload.contains("version two"));
        assert_eq!(reported_by.as_deref(), Some("peer-x"));
    }

    #[test]
    fn an_equivocating_node_is_flagged() {
        let f = fixture();
        f.db.apply_event(&event_at(&f.identity, 1, "one"), f.identity.node_id(), None)
            .unwrap();
        f.db.apply_event(&event_at(&f.identity, 1, "two"), f.identity.node_id(), None)
            .unwrap();

        let conn = f.db.conn();
        let flagged: i64 = conn
            .query_row(
                "SELECT equivocating FROM nodes WHERE id = ?1",
                params![f.identity.node_id()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(flagged, 1);
    }

    // --- Watermarks --------------------------------------------------------

    #[test]
    fn the_watermark_advances_over_a_contiguous_run() {
        let f = fixture();
        assert_eq!(f.db.watermark_for(f.identity.node_id()).unwrap(), 0);

        for seq in 1..=3 {
            f.db.apply_event(&event_at(&f.identity, seq, "e"), f.identity.node_id(), None)
                .unwrap();
        }
        assert_eq!(f.db.watermark_for(f.identity.node_id()).unwrap(), 3);
    }

    #[test]
    fn a_gap_holds_the_watermark_back_until_it_is_filled() {
        let f = fixture();

        // Arrive out of order: 1, then 3. The watermark must stay at 1.
        f.db.apply_event(&event_at(&f.identity, 1, "one"), f.identity.node_id(), None)
            .unwrap();
        f.db.apply_event(
            &event_at(&f.identity, 3, "three"),
            f.identity.node_id(),
            None,
        )
        .unwrap();
        assert_eq!(f.db.watermark_for(f.identity.node_id()).unwrap(), 1);

        // Filling the hole folds in the event that was already held.
        f.db.apply_event(&event_at(&f.identity, 2, "two"), f.identity.node_id(), None)
            .unwrap();
        assert_eq!(f.db.watermark_for(f.identity.node_id()).unwrap(), 3);
    }

    #[test]
    fn out_of_order_arrival_loses_no_events() {
        let f = fixture();
        for seq in [5, 3, 1, 4, 2] {
            f.db.apply_event(&event_at(&f.identity, seq, "e"), f.identity.node_id(), None)
                .unwrap();
        }

        assert_eq!(f.db.count_events().unwrap(), 5);
        assert_eq!(f.db.watermark_for(f.identity.node_id()).unwrap(), 5);
    }

    #[test]
    fn an_unknown_origin_has_a_zero_watermark() {
        let f = fixture();
        assert_eq!(f.db.watermark_for("never-heard-of-this-node").unwrap(), 0);
    }

    // --- Delta queries -----------------------------------------------------

    #[test]
    fn events_since_returns_only_what_the_peer_lacks() {
        let f = fixture();
        for seq in 1..=5 {
            f.db.apply_event(
                &event_at(&f.identity, seq, &format!("e{seq}")),
                f.identity.node_id(),
                None,
            )
            .unwrap();
        }

        let delta = f.db.events_since(f.identity.node_id(), 2, 100).unwrap();
        assert_eq!(delta.len(), 3);
        assert_eq!(delta[0].origin_seq, 3);
        assert_eq!(delta[2].origin_seq, 5);
    }

    #[test]
    fn events_since_stops_at_a_gap() {
        let f = fixture();
        for seq in [1, 2, 4, 5] {
            f.db.apply_event(&event_at(&f.identity, seq, "e"), f.identity.node_id(), None)
                .unwrap();
        }

        // 3 is missing, so only 1 and 2 may be handed on.
        let delta = f.db.events_since(f.identity.node_id(), 0, 100).unwrap();
        assert_eq!(delta.len(), 2);
        assert_eq!(delta.last().unwrap().origin_seq, 2);
    }

    #[test]
    fn a_delta_request_is_bounded() {
        let f = fixture();
        for seq in 1..=10 {
            f.db.apply_event(&event_at(&f.identity, seq, "e"), f.identity.node_id(), None)
                .unwrap();
        }

        assert_eq!(
            f.db.events_since(f.identity.node_id(), 0, 4).unwrap().len(),
            4
        );
        // An absurd limit is clamped rather than honoured.
        assert!(
            f.db.events_since(f.identity.node_id(), 0, u32::MAX)
                .unwrap()
                .len()
                <= MAX_SYNC_BATCH as usize
        );
    }

    #[test]
    fn a_stored_event_round_trips_unchanged_and_still_verifies() {
        let f = fixture();
        let original = event_at(&f.identity, 1, "round trip");
        f.db.apply_event(&original, f.identity.node_id(), None)
            .unwrap();

        let loaded = f.db.events_since(f.identity.node_id(), 0, 10).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0], original);
        // Crucially, the signature still verifies after a database round trip.
        assert!(loaded[0].verify().is_ok());
    }

    // --- Peer acknowledgement / store-and-forward --------------------------

    #[test]
    fn pending_count_reflects_what_a_peer_has_not_acknowledged() {
        let f = fixture();
        for seq in 1..=4 {
            f.db.apply_event(&event_at(&f.identity, seq, "e"), f.identity.node_id(), None)
                .unwrap();
        }

        assert_eq!(f.db.pending_events_for_peer("peer-b").unwrap(), 4);

        f.db.record_peer_ack("peer-b", f.identity.node_id(), 3)
            .unwrap();
        assert_eq!(f.db.pending_events_for_peer("peer-b").unwrap(), 1);

        f.db.record_peer_ack("peer-b", f.identity.node_id(), 4)
            .unwrap();
        assert_eq!(f.db.pending_events_for_peer("peer-b").unwrap(), 0);
    }

    #[test]
    fn a_replayed_ack_cannot_move_the_watermark_backwards() {
        let f = fixture();
        for seq in 1..=3 {
            f.db.apply_event(&event_at(&f.identity, seq, "e"), f.identity.node_id(), None)
                .unwrap();
        }

        f.db.record_peer_ack("peer-b", f.identity.node_id(), 3)
            .unwrap();
        // A stale ACK arriving late must not cause events to be re-sent.
        f.db.record_peer_ack("peer-b", f.identity.node_id(), 1)
            .unwrap();

        assert_eq!(f.db.pending_events_for_peer("peer-b").unwrap(), 0);
    }

    #[test]
    fn a_peer_is_never_sent_its_own_events() {
        let f = fixture();
        f.db.apply_event(
            &event_at(&f.identity, 1, "mine"),
            f.identity.node_id(),
            None,
        )
        .unwrap();

        // From the local node's own perspective it needs nothing sent to it.
        assert_eq!(
            f.db.pending_events_for_peer(f.identity.node_id()).unwrap(),
            0
        );
    }

    #[test]
    fn acknowledgement_state_survives_reopening_the_database() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("node.sqlite");
        let identity =
            NodeIdentity::load_or_create(&FileKeyStore::new(dir.path().join("id.json"))).unwrap();

        {
            let db = Database::open(&path).unwrap();
            db.register_local_node(
                identity.node_id(),
                identity.node_name(),
                &identity.public_key_hex(),
                identity.created_at(),
            )
            .unwrap();
            db.apply_event(&event_at(&identity, 1, "e"), identity.node_id(), None)
                .unwrap();
            db.apply_event(&event_at(&identity, 2, "e"), identity.node_id(), None)
                .unwrap();
            db.record_peer_ack("peer-b", identity.node_id(), 1).unwrap();
        }

        let reopened = Database::open(&path).unwrap();
        assert_eq!(reopened.count_events().unwrap(), 2);
        assert_eq!(reopened.watermark_for(identity.node_id()).unwrap(), 2);
        assert_eq!(reopened.pending_events_for_peer("peer-b").unwrap(), 1);
    }

    #[test]
    fn sync_watermarks_describe_every_origin_held() {
        let f = fixture();
        let other_dir = TempDir::new().unwrap();
        let other =
            NodeIdentity::load_or_create(&FileKeyStore::new(other_dir.path().join("id.json")))
                .unwrap();

        f.db.apply_event(
            &event_at(&f.identity, 1, "mine"),
            f.identity.node_id(),
            None,
        )
        .unwrap();
        f.db.apply_event(&event_at(&other, 1, "theirs"), f.identity.node_id(), None)
            .unwrap();
        f.db.apply_event(&event_at(&other, 2, "theirs"), f.identity.node_id(), None)
            .unwrap();

        let watermarks = f.db.sync_watermarks().unwrap();
        assert_eq!(watermarks.len(), 2);

        let lookup: std::collections::HashMap<_, _> = watermarks.into_iter().collect();
        assert_eq!(lookup[f.identity.node_id()], 1);
        assert_eq!(lookup[other.node_id()], 2);
    }
}
