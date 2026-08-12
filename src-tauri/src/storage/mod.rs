//! Local, offline-first persistence.
//!
//! Every SecureMesh node owns its database outright. There is no central
//! server, so the local file is authoritative for this node's own records and
//! is the substrate the Phase 2 sync engine will reconcile against peers.
//!
//! # Conventions
//!
//! - **Always a file, never in-memory.** Operational data must survive an
//!   application restart, a battery pull, and a crash.
//! - **Parameterised SQL only.** No statement in this module is built by
//!   concatenating caller-supplied values.
//! - **Timestamps** are stored as fixed-width RFC 3339 UTC strings, which sort
//!   lexicographically in the same order as they do chronologically, so
//!   `ORDER BY created_at` is correct without a date function.

pub mod events;
pub mod incidents;
pub mod intelligence;
pub mod migrations;
pub mod nodes;
pub mod trust;

use crate::error::{CoreError, CoreResult};
use crate::security::{audit, AuditEvent, AuditOutcome};
use chrono::{DateTime, SecondsFormat, Utc};
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

/// A handle to this node's local database.
///
/// Cheap to share across Tauri command invocations; the single connection is
/// guarded by a mutex, which is ample for a node-local workload and avoids the
/// complexity of a pool.
pub struct Database {
    conn: Mutex<Connection>,
    path: PathBuf,
}

impl Database {
    /// Opens (creating if necessary) the database at `path` and brings its
    /// schema up to date.
    pub fn open(path: impl AsRef<Path>) -> CoreResult<Self> {
        let path = path.as_ref().to_path_buf();

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                CoreError::storage(format!("could not create data directory ({})", e.kind()))
            })?;
        }

        let mut conn = Connection::open(&path)?;
        configure(&conn)?;
        let applied = migrations::apply(&mut conn)?;

        audit(
            AuditEvent::DatabaseOpened,
            AuditOutcome::Success,
            &format!(
                "schema_version={} migrations_applied={}",
                migrations::target_version(),
                applied
            ),
        );

        Ok(Self {
            conn: Mutex::new(conn),
            path,
        })
    }

    /// Where this database lives on disk.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Borrows the connection.
    ///
    /// # The guard is not reentrant
    ///
    /// Never call another `&self` method of `Database` while holding the
    /// returned guard — the mutex is a plain [`std::sync::Mutex`], so a nested
    /// acquisition deadlocks the calling thread rather than failing. Read every
    /// column you need in one query, or drop the guard first.
    ///
    /// A poisoned mutex means another thread panicked mid-query. Rather than
    /// propagating that panic, the poison is cleared and the guard recovered:
    /// SQLite's own transaction handling has already rolled back any partial
    /// write, so the connection remains usable and the node stays up.
    pub(crate) fn conn(&self) -> MutexGuard<'_, Connection> {
        self.conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Cheap liveness probe used by the dashboard's health indicator.
    pub fn health_check(&self) -> CoreResult<()> {
        let conn = self.conn();
        conn.query_row("SELECT 1", [], |row| row.get::<_, i64>(0))?;
        Ok(())
    }

    /// The schema version currently applied to this database.
    pub fn schema_version(&self) -> CoreResult<i32> {
        migrations::current_version(&self.conn())
    }
}

/// Applies the per-connection pragmas SecureMesh relies on.
fn configure(conn: &Connection) -> CoreResult<()> {
    // Foreign keys are off by default in SQLite and are per-connection, so
    // incident authorship would otherwise go unenforced.
    conn.pragma_update(None, "foreign_keys", "ON")?;

    // WAL survives an unclean shutdown better than the rollback journal and
    // lets a reader run while a writer commits — relevant once the sync engine
    // writes concurrently with the UI.
    conn.pragma_update(None, "journal_mode", "WAL")?;

    // FULL costs a device sync per commit but is what makes "the incident
    // survived the power cut" a true statement on field hardware.
    conn.pragma_update(None, "synchronous", "FULL")?;

    Ok(())
}

/// Renders a timestamp in the fixed-width form used throughout the schema.
pub(crate) fn format_timestamp(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// Parses a timestamp read back from the database.
///
/// A malformed value means the file was edited outside the application or is
/// corrupt, so it surfaces as a storage error rather than a panic.
pub(crate) fn parse_timestamp(column: &str, value: &str) -> CoreResult<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|_| CoreError::storage(format!("column '{column}' holds an invalid timestamp")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn opening_creates_a_file_backed_database() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("securemesh.sqlite");

        let db = Database::open(&path).unwrap();
        assert!(path.exists(), "database must be a real file, not in-memory");
        assert_eq!(db.path(), path);
    }

    #[test]
    fn opening_creates_missing_parent_directories() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("nested").join("data").join("node.sqlite");

        Database::open(&path).unwrap();
        assert!(path.exists());
    }

    #[test]
    fn a_new_database_is_at_the_target_schema_version() {
        let dir = TempDir::new().unwrap();
        let db = Database::open(dir.path().join("node.sqlite")).unwrap();

        assert_eq!(db.schema_version().unwrap(), migrations::target_version());
    }

    #[test]
    fn reopening_an_existing_database_succeeds() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("node.sqlite");

        let first = Database::open(&path).unwrap();
        drop(first);

        let second = Database::open(&path).unwrap();
        assert_eq!(
            second.schema_version().unwrap(),
            migrations::target_version()
        );
    }

    #[test]
    fn health_check_passes_on_an_open_database() {
        let dir = TempDir::new().unwrap();
        let db = Database::open(dir.path().join("node.sqlite")).unwrap();
        assert!(db.health_check().is_ok());
    }

    #[test]
    fn foreign_keys_are_enforced() {
        let dir = TempDir::new().unwrap();
        let db = Database::open(dir.path().join("node.sqlite")).unwrap();

        let enabled: i64 = db
            .conn()
            .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
            .unwrap();
        assert_eq!(enabled, 1);
    }

    #[test]
    fn timestamps_round_trip_and_sort_lexicographically() {
        let earlier = DateTime::parse_from_rfc3339("2026-01-01T10:00:00.000Z")
            .unwrap()
            .with_timezone(&Utc);
        let later = DateTime::parse_from_rfc3339("2026-01-01T10:00:01.000Z")
            .unwrap()
            .with_timezone(&Utc);

        let earlier_text = format_timestamp(earlier);
        let later_text = format_timestamp(later);

        assert!(
            earlier_text < later_text,
            "text order must match time order"
        );
        assert_eq!(
            parse_timestamp("created_at", &earlier_text).unwrap(),
            earlier
        );
    }

    #[test]
    fn a_corrupt_timestamp_is_a_storage_error_not_a_panic() {
        let err = parse_timestamp("created_at", "not-a-timestamp").unwrap_err();
        assert_eq!(err.code(), "STORAGE_ERROR");
        assert!(err.message().contains("created_at"));
    }
}
