//! Persistence for node records — this node, and (from Phase 2) its peers.

use super::{format_timestamp, parse_timestamp, Database};
use crate::domain::{ConnectionState, NodeRecord, NodeStatus, Peer};
use crate::error::{CoreError, CoreResult};
use chrono::{DateTime, Utc};
use rusqlite::{params, Row};

const SELECT_COLUMNS: &str = "id, node_name, public_key, status, last_seen, created_at";

impl Database {
    /// Records this node in its own `nodes` table.
    ///
    /// Called once per launch. Re-running it refreshes the display name and
    /// `last_seen` but never rewrites the public key: a node's key is its
    /// identity, and a changed key means a different node.
    pub fn register_local_node(
        &self,
        node_id: &str,
        node_name: &str,
        public_key_hex: &str,
        created_at: DateTime<Utc>,
    ) -> CoreResult<()> {
        let now = Utc::now();
        let conn = self.conn();
        conn.execute(
            "INSERT INTO nodes (id, node_name, public_key, status, last_seen, created_at)
             VALUES (?1, ?2, ?3, 'LOCAL', ?4, ?5)
             ON CONFLICT (id) DO UPDATE SET
                 node_name = excluded.node_name,
                 status    = 'LOCAL',
                 last_seen = excluded.last_seen",
            params![
                node_id,
                node_name,
                public_key_hex,
                format_timestamp(now),
                format_timestamp(created_at),
            ],
        )?;
        Ok(())
    }

    /// Records a peer this node has an authenticated session with.
    ///
    /// The caller must already have verified that `node_id` is the fingerprint
    /// of `public_key_hex`; this layer stores an identity, it does not vouch
    /// for one. The public key is deliberately **not** updated on conflict: a
    /// node's key *is* its identity, so a changed key means a different node,
    /// not an update to this one.
    pub fn register_peer(
        &self,
        node_id: &str,
        public_key_hex: &str,
        transport_peer_id: Option<&str>,
    ) -> CoreResult<()> {
        let now = format_timestamp(crate::domain::now());
        let conn = self.conn();
        conn.execute(
            "INSERT INTO nodes (id, node_name, public_key, status, last_seen, created_at,
                                transport_peer_id)
             VALUES (?1, ?2, ?3, 'ONLINE', ?4, ?4, ?5)
             ON CONFLICT (id) DO UPDATE SET
                 status            = 'ONLINE',
                 last_seen         = excluded.last_seen,
                 transport_peer_id = excluded.transport_peer_id",
            params![
                node_id,
                crate::identity::node_name_for(node_id),
                public_key_hex,
                now,
                transport_peer_id,
            ],
        )?;
        Ok(())
    }

    /// Notes that a peer's session has ended.
    ///
    /// Never applied to the local node, whose `LOCAL` status is not a
    /// reachability claim and must survive a peer disconnecting.
    pub fn mark_node_offline(&self, node_id: &str) -> CoreResult<()> {
        let conn = self.conn();
        conn.execute(
            "UPDATE nodes SET status = 'OFFLINE' WHERE id = ?1 AND status <> 'LOCAL'",
            params![node_id],
        )?;
        Ok(())
    }

    /// Refreshes the last-seen timestamp for an active peer.
    pub fn touch_peer_seen(&self, node_id: &str) -> CoreResult<()> {
        let conn = self.conn();
        conn.execute(
            "UPDATE nodes SET last_seen = ?2 WHERE id = ?1 AND status <> 'LOCAL'",
            params![node_id, format_timestamp(crate::domain::now())],
        )?;
        Ok(())
    }

    /// Records the protocol version a peer announced at its last handshake.
    pub fn record_peer_protocol(&self, node_id: &str, protocol_version: u16) -> CoreResult<()> {
        let conn = self.conn();
        conn.execute(
            "UPDATE nodes SET protocol_version = ?2 WHERE id = ?1",
            params![node_id, protocol_version as i64],
        )?;
        Ok(())
    }

    /// Marks every peer offline.
    ///
    /// Run at startup: a `nodes` row saying ONLINE is a claim about a live
    /// session, and no session survives a process exit. Without this a
    /// restarted node would report peers it cannot reach.
    pub fn mark_all_peers_offline(&self) -> CoreResult<()> {
        let conn = self.conn();
        conn.execute("UPDATE nodes SET status = 'OFFLINE' WHERE status <> 'LOCAL'", [])?;
        Ok(())
    }

    /// Every known peer, assembled for the dashboard.
    ///
    /// Connection state comes from the caller's live view, not from the
    /// database: the stored status is only the last thing observed.
    pub fn list_peers(&self, connected: &[String]) -> CoreResult<Vec<Peer>> {
        let conn = self.conn();
        let mut statement = conn.prepare(
            "SELECT id, node_name, public_key, transport_peer_id, last_seen, protocol_version,
                    capabilities, equivocating, created_at
             FROM nodes
             WHERE status <> 'LOCAL'
             ORDER BY node_name ASC",
        )?;

        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<i64>>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, i64>(7)?,
                row.get::<_, String>(8)?,
            ))
        })?;

        let mut collected = Vec::new();
        for row in rows {
            let (id, name, key, transport, last_seen, version, capabilities, equivocating, first) =
                row?;
            collected.push((
                id,
                name,
                key,
                transport,
                last_seen,
                version,
                capabilities,
                equivocating,
                first,
            ));
        }
        drop(statement);
        drop(conn);

        let mut peers = Vec::with_capacity(collected.len());
        for (id, node_name, public_key, transport, last_seen, version, capabilities, equiv, first) in
            collected
        {
            let pending = self.pending_events_for_peer(&id)?;
            peers.push(Peer {
                connection_state: if connected.iter().any(|c| c == &id) {
                    ConnectionState::Connected
                } else {
                    ConnectionState::Disconnected
                },
                node_id: id,
                node_name,
                public_key,
                transport_peer_id: transport,
                last_seen: match last_seen {
                    Some(ref value) => Some(parse_timestamp("last_seen", value)?),
                    None => None,
                },
                protocol_version: version.and_then(|v| u16::try_from(v).ok()),
                // A malformed capabilities column must not take the dashboard
                // down, so it degrades to "none announced".
                capabilities: serde_json::from_str(&capabilities).unwrap_or_default(),
                equivocating: equiv != 0,
                pending_events: pending,
                first_seen: parse_timestamp("created_at", &first)?,
            });
        }
        Ok(peers)
    }

    /// Fetches a node record by ID.
    pub fn get_node(&self, node_id: &str) -> CoreResult<NodeRecord> {
        let conn = self.conn();
        let mut statement =
            conn.prepare(&format!("SELECT {SELECT_COLUMNS} FROM nodes WHERE id = ?1"))?;

        let row = statement
            .query_row(params![node_id], NodeRow::from_row)
            .map_err(|err| match err {
                rusqlite::Error::QueryReturnedNoRows => {
                    CoreError::not_found("no node with that identifier")
                }
                other => CoreError::from(other),
            })?;

        row.into_domain()
    }

    /// Lists every node this device knows about, including itself.
    pub fn list_nodes(&self) -> CoreResult<Vec<NodeRecord>> {
        let conn = self.conn();
        let mut statement = conn.prepare(&format!(
            "SELECT {SELECT_COLUMNS} FROM nodes ORDER BY created_at ASC"
        ))?;

        let rows = statement.query_map([], NodeRow::from_row)?;

        let mut nodes = Vec::new();
        for row in rows {
            nodes.push(row?.into_domain()?);
        }
        Ok(nodes)
    }

    /// Number of *peers* currently reachable — this node is never counted.
    ///
    /// Phase 1 has no networking, so this is always zero. It reads the table
    /// rather than returning a constant so the dashboard begins reporting real
    /// numbers the moment Phase 2 starts writing peer rows.
    pub fn count_connected_peers(&self) -> CoreResult<u64> {
        let conn = self.conn();
        let count: i64 = conn.query_row(
            "SELECT count(*) FROM nodes WHERE status = ?1",
            params![NodeStatus::Online.as_str()],
            |row| row.get(0),
        )?;
        Ok(count.max(0) as u64)
    }

    /// Total number of known peers, reachable or not.
    pub fn count_known_peers(&self) -> CoreResult<u64> {
        let conn = self.conn();
        let count: i64 = conn.query_row(
            "SELECT count(*) FROM nodes WHERE status <> ?1",
            params![NodeStatus::Local.as_str()],
            |row| row.get(0),
        )?;
        Ok(count.max(0) as u64)
    }
}

struct NodeRow {
    id: String,
    node_name: String,
    public_key: String,
    status: String,
    last_seen: Option<String>,
    created_at: String,
}

impl NodeRow {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            node_name: row.get(1)?,
            public_key: row.get(2)?,
            status: row.get(3)?,
            last_seen: row.get(4)?,
            created_at: row.get(5)?,
        })
    }

    fn into_domain(self) -> CoreResult<NodeRecord> {
        let last_seen = match self.last_seen {
            Some(ref value) => Some(parse_timestamp("last_seen", value)?),
            None => None,
        };

        Ok(NodeRecord {
            id: self.id,
            node_name: self.node_name,
            public_key: self.public_key,
            status: self.status.parse::<NodeStatus>()?,
            last_seen,
            created_at: parse_timestamp("created_at", &self.created_at)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    const NODE: &str = "a7f32c9e00000000000000000000000000000000000000000000000000000000";

    fn fixture() -> (TempDir, Database) {
        let dir = TempDir::new().unwrap();
        let db = Database::open(dir.path().join("node.sqlite")).unwrap();
        (dir, db)
    }

    #[test]
    fn the_local_node_can_be_registered_and_read_back() {
        let (_dir, db) = fixture();
        let created_at = Utc::now();
        db.register_local_node(NODE, "SM-A7F32", &"ab".repeat(32), created_at)
            .unwrap();

        let record = db.get_node(NODE).unwrap();
        assert_eq!(record.id, NODE);
        assert_eq!(record.node_name, "SM-A7F32");
        assert_eq!(record.public_key, "ab".repeat(32));
        assert_eq!(record.status, NodeStatus::Local);
        assert!(record.last_seen.is_some());
    }

    #[test]
    fn re_registering_is_idempotent_and_refreshes_last_seen() {
        let (_dir, db) = fixture();
        let created_at = Utc::now();
        db.register_local_node(NODE, "SM-A7F32", &"ab".repeat(32), created_at)
            .unwrap();
        let first_seen = db.get_node(NODE).unwrap().last_seen.unwrap();

        std::thread::sleep(std::time::Duration::from_millis(5));
        db.register_local_node(NODE, "SM-A7F32", &"ab".repeat(32), created_at)
            .unwrap();

        assert_eq!(db.list_nodes().unwrap().len(), 1, "must not duplicate rows");
        assert!(db.get_node(NODE).unwrap().last_seen.unwrap() >= first_seen);
    }

    #[test]
    fn an_unknown_node_reports_not_found() {
        let (_dir, db) = fixture();
        let err = db.get_node("unknown").unwrap_err();
        assert_eq!(err.code(), "NOT_FOUND");
    }

    #[test]
    fn the_local_node_is_never_counted_as_a_peer() {
        let (_dir, db) = fixture();
        db.register_local_node(NODE, "SM-A7F32", &"ab".repeat(32), Utc::now())
            .unwrap();

        assert_eq!(db.count_connected_peers().unwrap(), 0);
        assert_eq!(db.count_known_peers().unwrap(), 0);
        assert_eq!(db.list_nodes().unwrap().len(), 1);
    }

    #[test]
    fn peer_counts_reflect_reachability() {
        let (_dir, db) = fixture();
        db.register_local_node(NODE, "SM-A7F32", &"ab".repeat(32), Utc::now())
            .unwrap();

        // Phase 2 will insert peers through the sync layer; until then this
        // exercises the counting logic directly against the schema.
        let conn = db.conn();
        conn.execute(
            "INSERT INTO nodes (id, node_name, public_key, status, last_seen, created_at)
             VALUES ('peer-online', 'SM-B1111', 'cc', 'ONLINE', NULL, '2026-01-01T00:00:00.000Z'),
                    ('peer-offline', 'SM-C2222', 'dd', 'OFFLINE', NULL, '2026-01-01T00:00:00.000Z')",
            [],
        )
        .unwrap();
        drop(conn);

        assert_eq!(db.count_connected_peers().unwrap(), 1);
        assert_eq!(db.count_known_peers().unwrap(), 2);
    }

    #[test]
    fn two_nodes_cannot_share_a_public_key() {
        let (_dir, db) = fixture();
        let key = "ab".repeat(32);
        db.register_local_node(NODE, "SM-A7F32", &key, Utc::now())
            .unwrap();

        let err = db
            .register_local_node("a-different-node-id", "SM-BBBBB", &key, Utc::now())
            .unwrap_err();
        assert_eq!(err.code(), "STORAGE_ERROR");
    }

    #[test]
    fn node_registration_survives_reopening_the_database() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("node.sqlite");

        {
            let db = Database::open(&path).unwrap();
            db.register_local_node(NODE, "SM-A7F32", &"ab".repeat(32), Utc::now())
                .unwrap();
        }

        let reopened = Database::open(&path).unwrap();
        assert_eq!(reopened.get_node(NODE).unwrap().node_name, "SM-A7F32");
    }
}
