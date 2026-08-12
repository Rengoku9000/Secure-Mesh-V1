//! Persistence for incidents.
//!
//! Every statement here is parameterised. Values supplied by the user reach
//! SQLite only as bound parameters, never as SQL text.

use super::{format_timestamp, parse_timestamp, Database};
use crate::domain::{Incident, Observation, Severity, SyncStatus};
use crate::error::{CoreError, CoreResult};
use crate::security::{audit, AuditEvent, AuditOutcome};
use rusqlite::{params, Row};

/// Hard ceiling on how many incidents a single query may return, so a caller
/// cannot ask the node to materialise an unbounded result set.
pub const MAX_PAGE_SIZE: u32 = 500;

/// Default page size used when the caller does not specify one.
pub const DEFAULT_PAGE_SIZE: u32 = 100;

const SELECT_COLUMNS: &str = "id, created_by, description, severity, latitude, longitude, \
                              created_at, updated_at, sync_status";

impl Database {
    /// Writes a validated incident.
    ///
    /// Takes an [`Incident`] rather than raw fields, which means an unvalidated
    /// record cannot reach the database: the only way to obtain an `Incident`
    /// is through `NewIncident::validate`.
    pub fn insert_incident(&self, incident: &Incident) -> CoreResult<()> {
        let conn = self.conn();
        let result = conn.execute(
            "INSERT INTO incidents (
                 id, created_by, description, severity, latitude, longitude,
                 created_at, updated_at, sync_status
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                incident.id,
                incident.created_by,
                incident.description,
                incident.severity.as_str(),
                incident.latitude,
                incident.longitude,
                format_timestamp(incident.created_at),
                format_timestamp(incident.updated_at),
                incident.sync_status.as_str(),
            ],
        );

        match result {
            Ok(_) => {
                // Severity and sync status are recorded; the description is not,
                // since it may carry sensitive operational detail.
                audit(
                    AuditEvent::IncidentCreated,
                    AuditOutcome::Success,
                    &format!("id={} severity={}", incident.id, incident.severity),
                );
                Ok(())
            }
            Err(err) => {
                audit(
                    AuditEvent::IncidentCreated,
                    AuditOutcome::Failure,
                    &format!("id={}", incident.id),
                );
                Err(map_insert_error(err))
            }
        }
    }

    /// Returns the most recently created incidents, newest first.
    ///
    /// `limit` is clamped to [`MAX_PAGE_SIZE`]; `None` uses
    /// [`DEFAULT_PAGE_SIZE`].
    pub fn list_incidents(&self, limit: Option<u32>) -> CoreResult<Vec<Incident>> {
        let limit = limit.unwrap_or(DEFAULT_PAGE_SIZE).clamp(1, MAX_PAGE_SIZE);

        let conn = self.conn();
        let mut statement = conn.prepare(&format!(
            "SELECT {SELECT_COLUMNS} FROM incidents ORDER BY created_at DESC, id DESC LIMIT ?1"
        ))?;

        let rows = statement.query_map(params![limit], IncidentRow::from_row)?;

        let mut incidents = Vec::new();
        for row in rows {
            incidents.push(row?.into_domain()?);
        }
        Ok(incidents)
    }

    /// Fetches a single incident by ID.
    pub fn get_incident(&self, id: &str) -> CoreResult<Incident> {
        let conn = self.conn();
        let mut statement = conn.prepare(&format!(
            "SELECT {SELECT_COLUMNS} FROM incidents WHERE id = ?1"
        ))?;

        let row = statement
            .query_row(params![id], IncidentRow::from_row)
            .map_err(|err| match err {
                rusqlite::Error::QueryReturnedNoRows => {
                    CoreError::not_found("no incident with that identifier")
                }
                other => CoreError::from(other),
            })?;

        row.into_domain()
    }

    /// Observations appended to an incident, oldest first.
    ///
    /// Ordered by the author's timestamp only for readability. Correctness
    /// never depends on it: observations are independent facts, so their
    /// display order carries no meaning that a skewed clock could corrupt.
    pub fn list_observations(&self, incident_id: &str) -> CoreResult<Vec<Observation>> {
        let conn = self.conn();
        let mut statement = conn.prepare(
            "SELECT id, incident_id, author_node, note, created_at
             FROM incident_observations
             WHERE incident_id = ?1
             ORDER BY created_at ASC, id ASC",
        )?;

        let rows = statement.query_map(params![incident_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?;

        let mut observations = Vec::new();
        for row in rows {
            let (id, incident_id, author_node, note, created_at) = row?;
            observations.push(Observation {
                id,
                incident_id,
                author_node,
                note,
                created_at: parse_timestamp("created_at", &created_at)?,
            });
        }
        Ok(observations)
    }

    /// Number of observations attached to an incident.
    pub fn count_observations(&self, incident_id: &str) -> CoreResult<u64> {
        let conn = self.conn();
        let count: i64 = conn.query_row(
            "SELECT count(*) FROM incident_observations WHERE incident_id = ?1",
            params![incident_id],
            |row| row.get(0),
        )?;
        Ok(count.max(0) as u64)
    }

    /// Total number of incidents held locally.
    pub fn count_incidents(&self) -> CoreResult<u64> {
        let conn = self.conn();
        let count: i64 = conn.query_row("SELECT count(*) FROM incidents", [], |row| row.get(0))?;
        Ok(count.max(0) as u64)
    }

    /// Number of incidents in a given synchronisation state.
    pub fn count_incidents_by_sync_status(&self, status: SyncStatus) -> CoreResult<u64> {
        let conn = self.conn();
        let count: i64 = conn.query_row(
            "SELECT count(*) FROM incidents WHERE sync_status = ?1",
            params![status.as_str()],
            |row| row.get(0),
        )?;
        Ok(count.max(0) as u64)
    }
}

/// Turns a constraint violation into an error the operator can act on.
fn map_insert_error(err: rusqlite::Error) -> CoreError {
    let text = err.to_string();
    if text.contains("FOREIGN KEY") {
        CoreError::validation("incident author is not a known node")
    } else if text.contains("UNIQUE") {
        CoreError::validation("an incident with that identifier already exists")
    } else {
        CoreError::from(err)
    }
}

/// The raw column values, before domain types are reconstructed.
///
/// Kept separate so a malformed row becomes a `CoreError` rather than forcing
/// the rusqlite row mapper to deal in domain errors.
struct IncidentRow {
    id: String,
    created_by: String,
    description: String,
    severity: String,
    latitude: Option<f64>,
    longitude: Option<f64>,
    created_at: String,
    updated_at: String,
    sync_status: String,
}

impl IncidentRow {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            created_by: row.get(1)?,
            description: row.get(2)?,
            severity: row.get(3)?,
            latitude: row.get(4)?,
            longitude: row.get(5)?,
            created_at: row.get(6)?,
            updated_at: row.get(7)?,
            sync_status: row.get(8)?,
        })
    }

    fn into_domain(self) -> CoreResult<Incident> {
        Ok(Incident {
            id: self.id,
            created_by: self.created_by,
            description: self.description,
            severity: self.severity.parse::<Severity>().map_err(|_| {
                CoreError::storage("database holds an unrecognised incident severity")
            })?,
            latitude: self.latitude,
            longitude: self.longitude,
            created_at: parse_timestamp("created_at", &self.created_at)?,
            updated_at: parse_timestamp("updated_at", &self.updated_at)?,
            sync_status: self.sync_status.parse::<SyncStatus>()?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::NewIncident;
    use chrono::Utc;
    use tempfile::TempDir;

    const NODE: &str = "a7f32c9e00000000000000000000000000000000000000000000000000000000";

    /// Builds a database with the local node already registered, since
    /// incident authorship is a foreign key.
    fn fixture() -> (TempDir, Database) {
        let dir = TempDir::new().unwrap();
        let db = Database::open(dir.path().join("node.sqlite")).unwrap();
        db.register_local_node(NODE, "SM-A7F32", &"aa".repeat(32), Utc::now())
            .unwrap();
        (dir, db)
    }

    fn incident(description: &str, severity: &str) -> Incident {
        NewIncident {
            description: description.to_string(),
            severity: severity.to_string(),
            latitude: None,
            longitude: None,
        }
        .validate(NODE)
        .unwrap()
    }

    #[test]
    fn an_inserted_incident_can_be_read_back() {
        let (_dir, db) = fixture();
        let created = incident("Landslide blocking access road", "HIGH");
        db.insert_incident(&created).unwrap();

        let fetched = db.get_incident(&created.id).unwrap();
        assert_eq!(fetched, created);
    }

    #[test]
    fn coordinates_survive_the_round_trip() {
        let (_dir, db) = fixture();
        let mut created = incident("Relief camp established", "MEDIUM");
        created.latitude = Some(12.9716);
        created.longitude = Some(77.5946);
        db.insert_incident(&created).unwrap();

        let fetched = db.get_incident(&created.id).unwrap();
        assert_eq!(fetched.latitude, Some(12.9716));
        assert_eq!(fetched.longitude, Some(77.5946));
    }

    #[test]
    fn a_missing_incident_reports_not_found() {
        let (_dir, db) = fixture();
        let err = db
            .get_incident("00000000-0000-4000-8000-000000000000")
            .unwrap_err();
        assert_eq!(err.code(), "NOT_FOUND");
    }

    #[test]
    fn incidents_are_listed_newest_first() {
        let (_dir, db) = fixture();
        for label in ["first", "second", "third"] {
            db.insert_incident(&incident(label, "LOW")).unwrap();
            // The timestamp has millisecond resolution; make ordering unambiguous.
            std::thread::sleep(std::time::Duration::from_millis(2));
        }

        let listed = db.list_incidents(None).unwrap();
        let descriptions: Vec<&str> = listed.iter().map(|i| i.description.as_str()).collect();
        assert_eq!(descriptions, vec!["third", "second", "first"]);
    }

    #[test]
    fn listing_is_bounded_by_the_requested_limit() {
        let (_dir, db) = fixture();
        for n in 0..5 {
            db.insert_incident(&incident(&format!("incident {n}"), "LOW"))
                .unwrap();
        }

        assert_eq!(db.list_incidents(Some(2)).unwrap().len(), 2);
        assert_eq!(db.list_incidents(Some(50)).unwrap().len(), 5);
    }

    #[test]
    fn an_absurd_limit_is_clamped_rather_than_honoured() {
        let (_dir, db) = fixture();
        db.insert_incident(&incident("only one", "LOW")).unwrap();

        // Must not panic or attempt to allocate for u32::MAX rows.
        assert_eq!(db.list_incidents(Some(u32::MAX)).unwrap().len(), 1);
        assert_eq!(db.list_incidents(Some(0)).unwrap().len(), 1);
    }

    #[test]
    fn an_empty_database_lists_nothing() {
        let (_dir, db) = fixture();
        assert!(db.list_incidents(None).unwrap().is_empty());
        assert_eq!(db.count_incidents().unwrap(), 0);
    }

    #[test]
    fn counts_reflect_stored_incidents() {
        let (_dir, db) = fixture();
        for n in 0..3 {
            db.insert_incident(&incident(&format!("i{n}"), "LOW"))
                .unwrap();
        }

        assert_eq!(db.count_incidents().unwrap(), 3);
        assert_eq!(
            db.count_incidents_by_sync_status(SyncStatus::Pending)
                .unwrap(),
            3
        );
        assert_eq!(
            db.count_incidents_by_sync_status(SyncStatus::Synced)
                .unwrap(),
            0
        );
    }

    #[test]
    fn new_incidents_start_pending() {
        let (_dir, db) = fixture();
        let created = incident("awaiting mesh", "CRITICAL");
        db.insert_incident(&created).unwrap();

        assert_eq!(
            db.get_incident(&created.id).unwrap().sync_status,
            SyncStatus::Pending
        );
    }

    #[test]
    fn every_severity_round_trips() {
        let (_dir, db) = fixture();
        for severity in Severity::ALL {
            let created = incident("severity check", severity.as_str());
            db.insert_incident(&created).unwrap();
            assert_eq!(db.get_incident(&created.id).unwrap().severity, severity);
        }
    }

    #[test]
    fn an_incident_from_an_unknown_author_is_rejected() {
        let (_dir, db) = fixture();
        let orphan = NewIncident {
            description: "from a node we have never met".to_string(),
            severity: "LOW".to_string(),
            latitude: None,
            longitude: None,
        }
        .validate("ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff")
        .unwrap();

        let err = db.insert_incident(&orphan).unwrap_err();
        assert_eq!(err.code(), "VALIDATION_ERROR");
        assert!(err.message().contains("not a known node"));
    }

    #[test]
    fn inserting_the_same_incident_twice_is_rejected() {
        let (_dir, db) = fixture();
        let created = incident("duplicate", "LOW");
        db.insert_incident(&created).unwrap();

        let err = db.insert_incident(&created).unwrap_err();
        assert!(err.message().contains("already exists"));
    }

    #[test]
    fn sql_metacharacters_in_a_description_are_stored_literally() {
        let (_dir, db) = fixture();
        let hostile = "'); DROP TABLE incidents; --";
        let created = incident(hostile, "LOW");
        db.insert_incident(&created).unwrap();

        // The table still exists and the text was stored verbatim.
        assert_eq!(db.get_incident(&created.id).unwrap().description, hostile);
        assert_eq!(db.count_incidents().unwrap(), 1);
    }

    #[test]
    fn incidents_survive_reopening_the_database() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("node.sqlite");

        let id = {
            let db = Database::open(&path).unwrap();
            db.register_local_node(NODE, "SM-A7F32", &"aa".repeat(32), Utc::now())
                .unwrap();
            let created = incident("must outlive the process", "CRITICAL");
            db.insert_incident(&created).unwrap();
            created.id
        };

        let reopened = Database::open(&path).unwrap();
        let fetched = reopened.get_incident(&id).unwrap();
        assert_eq!(fetched.description, "must outlive the process");
        assert_eq!(fetched.severity, Severity::Critical);
    }
}
