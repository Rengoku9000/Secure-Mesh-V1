//! Forward-only schema migrations.
//!
//! SQLite's `user_version` pragma records how many migrations have been
//! applied. On open, every migration with a higher version is applied inside a
//! single transaction, so a node either reaches the new schema or stays on the
//! old one — it never comes up half-migrated in the field.
//!
//! To add a migration: append a `.sql` file under `migrations/` and add an
//! entry to [`MIGRATIONS`]. Never edit a migration that has shipped; nodes
//! already running it will not re-apply it.

use crate::error::{CoreError, CoreResult};
use rusqlite::Connection;

/// An ordered, append-only list of schema versions.
const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "initial_schema",
        sql: include_str!("../../migrations/001_initial_schema.sql"),
    },
    Migration {
        version: 2,
        name: "event_log",
        sql: include_str!("../../migrations/002_event_log.sql"),
    },
    Migration {
        version: 3,
        name: "peer_trust",
        sql: include_str!("../../migrations/003_peer_trust.sql"),
    },
];

struct Migration {
    version: i32,
    name: &'static str,
    sql: &'static str,
}

/// The schema version this build of the core expects.
pub fn target_version() -> i32 {
    MIGRATIONS.last().map_or(0, |m| m.version)
}

/// Reads the schema version currently stored in the database.
pub fn current_version(conn: &Connection) -> CoreResult<i32> {
    Ok(conn.query_row("PRAGMA user_version", [], |row| row.get(0))?)
}

/// Applies every migration newer than the database's current version.
///
/// Returns the number of migrations applied, which is zero for an
/// already-current database.
pub fn apply(conn: &mut Connection) -> CoreResult<usize> {
    let from_version = current_version(conn)?;

    if from_version > target_version() {
        // The database was written by a newer build. Refusing to touch it is
        // safer than running old code against an unknown schema.
        return Err(CoreError::storage(format!(
            "database schema version {from_version} is newer than this build supports ({})",
            target_version()
        )));
    }

    let pending: Vec<&Migration> = MIGRATIONS
        .iter()
        .filter(|m| m.version > from_version)
        .collect();

    if pending.is_empty() {
        return Ok(0);
    }

    let transaction = conn.transaction()?;
    for migration in &pending {
        transaction.execute_batch(migration.sql).map_err(|e| {
            CoreError::storage(format!(
                "migration {} ({}) failed: {e}",
                migration.version, migration.name
            ))
        })?;
    }

    // `user_version` does not accept a bound parameter, so it is formatted in.
    // The value is an i32 from a compile-time constant, never user input.
    let new_version = pending
        .last()
        .map_or(from_version, |m| m.version);
    transaction.pragma_update(None, "user_version", new_version)?;
    transaction.commit()?;

    Ok(pending.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn memory_conn() -> Connection {
        // Migration logic is schema-only, so an in-memory connection is
        // appropriate here. The application itself always uses a file database;
        // persistence is covered by the storage and integration tests.
        Connection::open_in_memory().unwrap()
    }

    #[test]
    fn a_fresh_database_is_migrated_to_the_target_version() {
        let mut conn = memory_conn();
        assert_eq!(current_version(&conn).unwrap(), 0);

        let applied = apply(&mut conn).unwrap();
        assert_eq!(applied, MIGRATIONS.len());
        assert_eq!(current_version(&conn).unwrap(), target_version());
    }

    #[test]
    fn migrations_are_idempotent() {
        let mut conn = memory_conn();
        apply(&mut conn).unwrap();

        // A second open must be a no-op rather than an error or a re-run.
        assert_eq!(apply(&mut conn).unwrap(), 0);
        assert_eq!(current_version(&conn).unwrap(), target_version());
    }

    #[test]
    fn migrating_from_v1_preserves_existing_rows() {
        // Models a node upgraded in the field: it already holds Phase 1 data,
        // and migration 002 must not disturb it.
        let mut conn = memory_conn();
        conn.execute_batch(MIGRATIONS[0].sql).unwrap();
        conn.pragma_update(None, "user_version", 1).unwrap();
        conn.execute(
            "INSERT INTO nodes (id, node_name, public_key, status, last_seen, created_at)
             VALUES ('node-1', 'SM-AAAAA', 'aa', 'LOCAL', NULL, '2026-01-01T00:00:00.000Z')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO incidents (id, created_by, description, severity, latitude, longitude,
                                    created_at, updated_at, sync_status)
             VALUES ('inc-1', 'node-1', 'legacy incident', 'HIGH', NULL, NULL,
                     '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z', 'PENDING')",
            [],
        )
        .unwrap();

        let applied = apply(&mut conn).unwrap();
        assert_eq!(
            applied,
            target_version() as usize - 1,
            "every migration after 001 should run, and no more"
        );
        assert_eq!(current_version(&conn).unwrap(), target_version());

        let description: String = conn
            .query_row("SELECT description FROM incidents WHERE id = 'inc-1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(description, "legacy incident");

        // The new column exists and defaults to NULL for pre-existing rows.
        let origin: Option<String> = conn
            .query_row(
                "SELECT origin_event_id FROM incidents WHERE id = 'inc-1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(origin.is_none());

        // Phase 2.5: the pre-existing local row is bootstrapped as the
        // administrator of its own trust store.
        let (trust, role): (String, String) = conn
            .query_row(
                "SELECT trust_state, peer_role FROM nodes WHERE id = 'node-1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(trust, "TRUSTED");
        assert_eq!(role, "ADMIN");
    }

    #[test]
    fn upgrading_does_not_silently_carry_forward_implicit_peer_trust() {
        // A Phase 2 database synchronised with any peer that connected. After
        // the upgrade those peers must be unauthorised until an operator says
        // otherwise — failing closed is the entire point of the phase.
        let mut conn = memory_conn();
        conn.execute_batch(MIGRATIONS[0].sql).unwrap();
        conn.pragma_update(None, "user_version", 1).unwrap();
        conn.execute(
            "INSERT INTO nodes (id, node_name, public_key, status, last_seen, created_at)
             VALUES ('peer-1', 'SM-BBBBB', 'bb', 'ONLINE', NULL, '2026-01-01T00:00:00.000Z')",
            [],
        )
        .unwrap();

        apply(&mut conn).unwrap();

        let trust: String = conn
            .query_row(
                "SELECT trust_state FROM nodes WHERE id = 'peer-1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(trust, "UNKNOWN", "existing peers must not stay implicitly trusted");
    }

    #[test]
    fn all_expected_tables_exist_after_migration() {
        let mut conn = memory_conn();
        apply(&mut conn).unwrap();

        for table in [
            "nodes",
            "incidents",
            "messages",
            "sync_events",
            "events",
            "event_conflicts",
            "sync_watermarks",
            "peer_ack_watermarks",
            "incident_observations",
            "outbound_queue",
        ] {
            let count: i64 = conn
                .query_row(
                    "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                    [table],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(count, 1, "table {table} should exist");
        }
    }

    #[test]
    fn a_newer_schema_is_refused_rather_than_downgraded() {
        let mut conn = memory_conn();
        conn.pragma_update(None, "user_version", target_version() + 1)
            .unwrap();

        let err = apply(&mut conn).unwrap_err();
        assert_eq!(err.code(), "STORAGE_ERROR");
        assert!(err.message().contains("newer than this build"));
    }

    #[test]
    fn migration_versions_are_unique_and_ascending() {
        let mut previous = 0;
        for migration in MIGRATIONS {
            assert!(
                migration.version > previous,
                "migration versions must ascend"
            );
            previous = migration.version;
        }
    }
}
