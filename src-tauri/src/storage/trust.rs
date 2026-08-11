//! The persistent peer trust store.
//!
//! SQLite is authoritative for every authorization decision. Nothing here is
//! cached in memory: an in-memory trust map would be a second answer to
//! "is this peer allowed", and a revocation that lived only in RAM would
//! silently undo itself on restart — precisely the failure this phase exists
//! to prevent.
//!
//! Two things are recorded for every decision:
//!
//! 1. the **current state**, on the peer's `nodes` row, which is what the sync
//!    engine consults; and
//! 2. an **append-only audit entry** in `peer_trust_events`, signed by the
//!    deciding node, which is never rewritten.
//!
//! Both are written in one transaction, so a decision cannot take effect
//! without leaving a record, and cannot be recorded without taking effect.

use super::{format_timestamp, parse_timestamp, Database};
use crate::domain::trust::{TrustEvent, TrustEventKind, TrustState};
use crate::domain::PeerRole;
use crate::error::{CoreError, CoreResult};
use crate::identity::NodeIdentity;
use crate::security::{audit, AuditEvent, AuditOutcome};
use rusqlite::{params, OptionalExtension};
use uuid::Uuid;

/// Domain-separation prefix for trust audit signatures.
///
/// Distinct from the event and envelope domains, so a signature over a trust
/// decision can never be replayed as a signature over a replicated event or a
/// protocol message, despite all three using the same node key.
pub const TRUST_SIGNING_DOMAIN: &[u8] = b"securemesh-trust-v1:";

/// Longest accepted operator note.
pub const MAX_TRUST_NOTE_CHARS: usize = 500;

/// Builds the bytes a trust audit entry is signed over.
///
/// Length-prefixed like the other signing domains, so no combination of field
/// contents can be re-parsed as a different decision.
fn trust_signing_bytes(
    sequence: u64,
    node_id: &str,
    kind: TrustEventKind,
    from_state: Option<TrustState>,
    to_state: TrustState,
    actor_node: &str,
    occurred_at: &str,
) -> Vec<u8> {
    let mut buffer = Vec::with_capacity(256);
    buffer.extend_from_slice(TRUST_SIGNING_DOMAIN);

    let mut push = |field: &[u8]| {
        buffer.extend_from_slice(&(field.len() as u64).to_be_bytes());
        buffer.extend_from_slice(field);
    };

    push(&sequence.to_be_bytes());
    push(node_id.as_bytes());
    push(kind.as_str().as_bytes());
    push(from_state.map(|s| s.as_str()).unwrap_or("").as_bytes());
    push(to_state.as_str().as_bytes());
    push(actor_node.as_bytes());
    push(occurred_at.as_bytes());

    buffer
}

impl Database {
    /// The authorization state of a peer.
    ///
    /// A node with no record is `Unknown`, which denies authorized operations.
    /// Failing closed on absence is what makes an unrecognised peer safe by
    /// default rather than by remembering to add a check.
    pub fn trust_state_of(&self, node_id: &str) -> CoreResult<TrustState> {
        let conn = self.conn();
        let stored: Option<String> = conn
            .query_row(
                "SELECT trust_state FROM nodes WHERE id = ?1",
                params![node_id],
                |row| row.get(0),
            )
            .optional()?;

        match stored {
            Some(value) => value.parse(),
            None => Ok(TrustState::Unknown),
        }
    }

    /// The role a node holds. An unrecorded node is an ordinary node, never an
    /// administrator.
    pub fn role_of(&self, node_id: &str) -> CoreResult<PeerRole> {
        let conn = self.conn();
        let stored: Option<String> = conn
            .query_row(
                "SELECT peer_role FROM nodes WHERE id = ?1",
                params![node_id],
                |row| row.get(0),
            )
            .optional()?;

        match stored {
            Some(value) => value.parse(),
            None => Ok(PeerRole::Node),
        }
    }

    /// Sets the role of the local node.
    ///
    /// Used by provisioning to lock a field node down to `NODE`, so its
    /// operator cannot enroll peers. On hardware the operator physically
    /// controls this is a policy control rather than a cryptographic barrier;
    /// see `docs/security/SECURITY.md`.
    pub fn set_local_role(&self, node_id: &str, role: PeerRole) -> CoreResult<()> {
        let conn = self.conn();
        let updated = conn.execute(
            "UPDATE nodes SET peer_role = ?2 WHERE id = ?1 AND status = 'LOCAL'",
            params![node_id, role.as_str()],
        )?;
        if updated != 1 {
            return Err(CoreError::not_found("no local node to assign a role to"));
        }
        Ok(())
    }

    /// Records that a peer has presented itself for enrollment.
    ///
    /// Only ever moves `Unknown` to `Pending`. A peer that is already trusted
    /// stays trusted, and — critically — **a revoked peer stays revoked**:
    /// reconnecting, restarting, or re-announcing must never launder a denial
    /// back into a fresh decision.
    ///
    /// Returns the resulting state, so the caller can tell whether anything
    /// changed. Calling it repeatedly is idempotent.
    pub fn record_enrollment_request(
        &self,
        identity: &NodeIdentity,
        node_id: &str,
        display_name: &str,
        capabilities: &[String],
    ) -> CoreResult<TrustState> {
        let current = self.trust_state_of(node_id)?;

        // Metadata a peer announces is informational and is refreshed whatever
        // its trust state, because it never affects authorization.
        {
            let conn = self.conn();
            conn.execute(
                "UPDATE nodes SET capabilities = ?2 WHERE id = ?1",
                params![node_id, serde_json::to_string(capabilities)?],
            )?;
        }

        if current != TrustState::Unknown {
            return Ok(current);
        }

        self.apply_trust_transition(
            identity,
            node_id,
            TrustEventKind::EnrollmentRequested,
            current,
            TrustState::Pending,
            Some(&format!("presented as {display_name}")),
        )?;

        Ok(TrustState::Pending)
    }

    /// Approves a peer, moving it to `Trusted`.
    ///
    /// The caller is responsible for checking that the local operator holds
    /// [`Capability::PeerEnroll`](crate::domain::Capability::PeerEnroll); this
    /// layer records decisions, it does not decide who may make them.
    pub fn approve_peer(
        &self,
        identity: &NodeIdentity,
        node_id: &str,
        note: Option<&str>,
    ) -> CoreResult<TrustState> {
        let current = self.require_known_peer(node_id)?;
        if current == TrustState::Trusted {
            return Ok(current); // Idempotent.
        }

        let kind = if current == TrustState::Revoked {
            TrustEventKind::Reinstated
        } else {
            TrustEventKind::EnrollmentApproved
        };

        self.apply_trust_transition(identity, node_id, kind, current, TrustState::Trusted, note)?;
        Ok(TrustState::Trusted)
    }

    /// Refuses a peer that has never been trusted.
    pub fn reject_peer(
        &self,
        identity: &NodeIdentity,
        node_id: &str,
        note: Option<&str>,
    ) -> CoreResult<TrustState> {
        let current = self.require_known_peer(node_id)?;
        if current == TrustState::Revoked {
            return Ok(current); // Idempotent.
        }

        self.apply_trust_transition(
            identity,
            node_id,
            TrustEventKind::EnrollmentRejected,
            current,
            TrustState::Revoked,
            note,
        )?;
        Ok(TrustState::Revoked)
    }

    /// Withdraws authorization from a peer.
    ///
    /// The peer record is kept, not deleted. Deleting it would return the node
    /// to `Unknown`, and the next handshake would present it as a fresh
    /// enrollment candidate — turning revocation into a temporary
    /// inconvenience.
    pub fn revoke_peer(
        &self,
        identity: &NodeIdentity,
        node_id: &str,
        note: Option<&str>,
    ) -> CoreResult<TrustState> {
        let current = self.require_known_peer(node_id)?;
        if current == TrustState::Revoked {
            return Ok(current); // Idempotent.
        }

        self.apply_trust_transition(
            identity,
            node_id,
            TrustEventKind::Revoked,
            current,
            TrustState::Revoked,
            note,
        )?;
        Ok(TrustState::Revoked)
    }

    /// Confirms a node is known locally before a decision is recorded about it.
    ///
    /// `peer_trust_events.node_id` is a foreign key, so a decision about a node
    /// whose public key this device has never seen cannot be stored — which is
    /// what keeps every authorization bound to a cryptographic identity rather
    /// than to a name someone typed.
    fn require_known_peer(&self, node_id: &str) -> CoreResult<TrustState> {
        // Both columns are read in one query. Reading them separately would
        // mean calling another `&self` method while this one still holds the
        // connection guard, and the guard is a plain non-reentrant mutex — that
        // is a self-deadlock, not a slow path.
        let row: Option<(String, String)> = {
            let conn = self.conn();
            conn.query_row(
                "SELECT status, trust_state FROM nodes WHERE id = ?1",
                params![node_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
        };

        match row {
            Some((status, _)) if status == "LOCAL" => Err(CoreError::validation(
                "the local node's own authorization cannot be changed",
            )),
            Some((_, trust_state)) => trust_state.parse(),
            None => Err(CoreError::not_found(
                "no such peer — a decision can only be recorded for a node whose public key is held",
            )),
        }
    }

    /// Writes a state change and its audit entry in one transaction.
    fn apply_trust_transition(
        &self,
        identity: &NodeIdentity,
        node_id: &str,
        kind: TrustEventKind,
        from_state: TrustState,
        to_state: TrustState,
        detail: Option<&str>,
    ) -> CoreResult<()> {
        if let Some(note) = detail {
            if note.chars().count() > MAX_TRUST_NOTE_CHARS {
                return Err(CoreError::validation("trust note is too long"));
            }
        }

        let occurred_at = format_timestamp(crate::domain::now());
        let actor = identity.node_id().to_string();

        let mut conn = self.conn();
        let transaction = conn.transaction()?;

        // Allocated inside the transaction, so a crash cannot leave a gap or
        // reuse a sequence number.
        let highest: Option<i64> = transaction.query_row(
            "SELECT max(sequence) FROM peer_trust_events",
            [],
            |row| row.get(0),
        )?;
        let sequence = highest.unwrap_or(0).max(0) as u64 + 1;

        let signature = identity.sign(&trust_signing_bytes(
            sequence,
            node_id,
            kind,
            Some(from_state),
            to_state,
            &actor,
            &occurred_at,
        ));

        transaction.execute(
            "INSERT INTO peer_trust_events (
                 id, sequence, node_id, kind, from_state, to_state,
                 actor_node, occurred_at, detail, signature
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                Uuid::new_v4().to_string(),
                sequence as i64,
                node_id,
                kind.as_str(),
                from_state.as_str(),
                to_state.as_str(),
                actor,
                occurred_at,
                detail,
                hex::encode(signature),
            ],
        )?;

        // Provenance columns record when each kind of decision last happened.
        // Earlier values are preserved, so a revoked peer still shows who
        // approved it originally.
        match to_state {
            TrustState::Trusted => {
                transaction.execute(
                    "UPDATE nodes SET trust_state = ?2, enrolled_at = ?3, enrolled_by = ?4,
                                      trust_notes = ?5
                     WHERE id = ?1",
                    params![node_id, to_state.as_str(), occurred_at, actor, detail],
                )?;
            }
            TrustState::Revoked => {
                transaction.execute(
                    "UPDATE nodes SET trust_state = ?2, revoked_at = ?3, revoked_by = ?4,
                                      trust_notes = ?5
                     WHERE id = ?1",
                    params![node_id, to_state.as_str(), occurred_at, actor, detail],
                )?;
            }
            _ => {
                transaction.execute(
                    "UPDATE nodes SET trust_state = ?2, trust_notes = ?3 WHERE id = ?1",
                    params![node_id, to_state.as_str(), detail],
                )?;
            }
        }

        transaction.commit()?;

        audit(
            AuditEvent::PeerTrustChanged,
            AuditOutcome::Success,
            &format!("{kind} peer={node_id} {from_state}->{to_state} by={actor} seq={sequence}"),
        );
        Ok(())
    }

    /// The trust audit log, newest first. `node_id` narrows it to one peer.
    pub fn trust_audit_log(
        &self,
        node_id: Option<&str>,
        limit: u32,
    ) -> CoreResult<Vec<TrustEvent>> {
        let limit = limit.clamp(1, 500);
        let conn = self.conn();

        let mut statement = conn.prepare(
            "SELECT id, sequence, node_id, kind, from_state, to_state, actor_node,
                    occurred_at, detail
             FROM peer_trust_events
             WHERE (?1 IS NULL OR node_id = ?1)
             ORDER BY sequence DESC
             LIMIT ?2",
        )?;

        let rows = statement.query_map(params![node_id, limit], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, Option<String>>(8)?,
            ))
        })?;

        let mut events = Vec::new();
        for row in rows {
            let (id, sequence, node, kind, from, to, actor, occurred, detail) = row?;
            events.push(TrustEvent {
                id,
                sequence: sequence.max(0) as u64,
                node_id: node,
                kind: kind.parse()?,
                from_state: match from {
                    Some(ref value) => Some(value.parse()?),
                    None => None,
                },
                to_state: to.parse()?,
                actor_node: actor,
                occurred_at: parse_timestamp("occurred_at", &occurred)?,
                detail,
            });
        }
        Ok(events)
    }

    /// Number of peers awaiting an operator decision.
    pub fn count_pending_peers(&self) -> CoreResult<u64> {
        let conn = self.conn();
        let count: i64 = conn.query_row(
            "SELECT count(*) FROM nodes WHERE trust_state = 'PENDING' AND status <> 'LOCAL'",
            [],
            |row| row.get(0),
        )?;
        Ok(count.max(0) as u64)
    }

    /// Number of peers authorized to synchronise.
    pub fn count_trusted_peers(&self) -> CoreResult<u64> {
        let conn = self.conn();
        let count: i64 = conn.query_row(
            "SELECT count(*) FROM nodes WHERE trust_state = 'TRUSTED' AND status <> 'LOCAL'",
            [],
            |row| row.get(0),
        )?;
        Ok(count.max(0) as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::keystore::FileKeyStore;
    use tempfile::TempDir;

    struct Fixture {
        _dir: TempDir,
        db: Database,
        identity: NodeIdentity,
    }

    const PEER: &str = "b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1";

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

    fn register_peer(f: &Fixture) {
        f.db.register_peer(PEER, &"cd".repeat(32), Some("loopback:peer"))
            .unwrap();
    }

    #[test]
    fn an_unseen_node_is_unknown_and_unauthorized() {
        let f = fixture();
        let state = f.db.trust_state_of("never-heard-of-this-node").unwrap();

        assert_eq!(state, TrustState::Unknown);
        assert!(!state.permits_authorized_operations());
    }

    #[test]
    fn a_newly_connected_peer_is_not_trusted() {
        let f = fixture();
        register_peer(&f);

        // Connecting is authentication, not authorization.
        assert_eq!(f.db.trust_state_of(PEER).unwrap(), TrustState::Unknown);
    }

    #[test]
    fn the_local_node_bootstraps_as_a_trusted_admin() {
        let f = fixture();
        assert_eq!(
            f.db.trust_state_of(f.identity.node_id()).unwrap(),
            TrustState::Trusted
        );
        assert_eq!(f.db.role_of(f.identity.node_id()).unwrap(), PeerRole::Admin);
    }

    #[test]
    fn an_unknown_node_defaults_to_the_ordinary_role() {
        let f = fixture();
        assert_eq!(f.db.role_of("nobody").unwrap(), PeerRole::Node);
    }

    // --- Enrollment --------------------------------------------------------

    #[test]
    fn presenting_moves_an_unknown_peer_to_pending() {
        let f = fixture();
        register_peer(&f);

        let state = f
            .db
            .record_enrollment_request(&f.identity, PEER, "SM-BBBBB", &[])
            .unwrap();

        assert_eq!(state, TrustState::Pending);
        assert!(!state.permits_authorized_operations());
    }

    #[test]
    fn repeated_enrollment_requests_are_idempotent() {
        let f = fixture();
        register_peer(&f);

        for _ in 0..5 {
            f.db.record_enrollment_request(&f.identity, PEER, "SM-BBBBB", &[])
                .unwrap();
        }

        assert_eq!(f.db.trust_state_of(PEER).unwrap(), TrustState::Pending);
        // One transition recorded, not five.
        let log = f.db.trust_audit_log(Some(PEER), 100).unwrap();
        assert_eq!(log.len(), 1);
    }

    #[test]
    fn approving_a_pending_peer_makes_it_trusted() {
        let f = fixture();
        register_peer(&f);
        f.db.record_enrollment_request(&f.identity, PEER, "SM-BBBBB", &[])
            .unwrap();

        let state = f.db.approve_peer(&f.identity, PEER, Some("field team")).unwrap();

        assert_eq!(state, TrustState::Trusted);
        assert!(state.permits_authorized_operations());
    }

    #[test]
    fn approval_is_idempotent() {
        let f = fixture();
        register_peer(&f);
        f.db.approve_peer(&f.identity, PEER, None).unwrap();
        f.db.approve_peer(&f.identity, PEER, None).unwrap();

        assert_eq!(f.db.trust_state_of(PEER).unwrap(), TrustState::Trusted);
        assert_eq!(f.db.trust_audit_log(Some(PEER), 100).unwrap().len(), 1);
    }

    #[test]
    fn a_decision_cannot_be_recorded_for_a_node_with_no_public_key() {
        let f = fixture();
        // Never registered, so this device holds no key binding this ID.
        let err = f.db.approve_peer(&f.identity, PEER, None).unwrap_err();
        assert_eq!(err.code(), "NOT_FOUND");
    }

    #[test]
    fn the_local_node_cannot_have_its_own_authorization_changed() {
        let f = fixture();
        let err = f
            .db
            .revoke_peer(&f.identity, f.identity.node_id(), None)
            .unwrap_err();
        assert_eq!(err.code(), "VALIDATION_ERROR");
    }

    // --- Revocation --------------------------------------------------------

    #[test]
    fn revoking_a_trusted_peer_denies_it() {
        let f = fixture();
        register_peer(&f);
        f.db.approve_peer(&f.identity, PEER, None).unwrap();

        let state = f.db.revoke_peer(&f.identity, PEER, Some("device lost")).unwrap();

        assert_eq!(state, TrustState::Revoked);
        assert!(!state.permits_authorized_operations());
    }

    #[test]
    fn revocation_is_idempotent() {
        let f = fixture();
        register_peer(&f);
        f.db.approve_peer(&f.identity, PEER, None).unwrap();
        f.db.revoke_peer(&f.identity, PEER, None).unwrap();
        f.db.revoke_peer(&f.identity, PEER, None).unwrap();

        assert_eq!(f.db.trust_state_of(PEER).unwrap(), TrustState::Revoked);
        // approve + one revoke; the second revoke adds nothing.
        assert_eq!(f.db.trust_audit_log(Some(PEER), 100).unwrap().len(), 2);
    }

    #[test]
    fn revoking_keeps_the_peer_record_rather_than_deleting_it() {
        let f = fixture();
        register_peer(&f);
        f.db.approve_peer(&f.identity, PEER, None).unwrap();
        f.db.revoke_peer(&f.identity, PEER, None).unwrap();

        // Deleting the row would return the peer to UNKNOWN, and the next
        // handshake would offer it as a fresh enrollment candidate.
        assert!(f.db.get_node(PEER).is_ok());
        assert_eq!(f.db.trust_state_of(PEER).unwrap(), TrustState::Revoked);
    }

    #[test]
    fn a_revoked_peer_re_announcing_itself_stays_revoked() {
        let f = fixture();
        register_peer(&f);
        f.db.approve_peer(&f.identity, PEER, None).unwrap();
        f.db.revoke_peer(&f.identity, PEER, None).unwrap();

        // Reconnecting and re-presenting must not launder the denial away.
        let state = f
            .db
            .record_enrollment_request(&f.identity, PEER, "SM-BBBBB", &[])
            .unwrap();

        assert_eq!(state, TrustState::Revoked);
    }

    #[test]
    fn a_revoked_peer_reconnecting_stays_revoked() {
        let f = fixture();
        register_peer(&f);
        f.db.approve_peer(&f.identity, PEER, None).unwrap();
        f.db.revoke_peer(&f.identity, PEER, None).unwrap();

        // register_peer runs on every connection.
        register_peer(&f);

        assert_eq!(f.db.trust_state_of(PEER).unwrap(), TrustState::Revoked);
    }

    #[test]
    fn a_revoked_peer_can_be_deliberately_reinstated() {
        let f = fixture();
        register_peer(&f);
        f.db.approve_peer(&f.identity, PEER, None).unwrap();
        f.db.revoke_peer(&f.identity, PEER, None).unwrap();

        let state = f.db.approve_peer(&f.identity, PEER, Some("recovered")).unwrap();
        assert_eq!(state, TrustState::Trusted);

        let log = f.db.trust_audit_log(Some(PEER), 100).unwrap();
        assert_eq!(log[0].kind, TrustEventKind::Reinstated);
    }

    #[test]
    fn rejection_denies_a_peer_that_was_never_trusted() {
        let f = fixture();
        register_peer(&f);
        f.db.record_enrollment_request(&f.identity, PEER, "SM-BBBBB", &[])
            .unwrap();

        let state = f.db.reject_peer(&f.identity, PEER, Some("not recognised")).unwrap();
        assert_eq!(state, TrustState::Revoked);

        let log = f.db.trust_audit_log(Some(PEER), 100).unwrap();
        assert_eq!(log[0].kind, TrustEventKind::EnrollmentRejected);
    }

    // --- Durability --------------------------------------------------------

    #[test]
    fn revocation_survives_reopening_the_database() {
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
            db.register_peer(PEER, &"cd".repeat(32), None).unwrap();
            db.approve_peer(&identity, PEER, None).unwrap();
            db.revoke_peer(&identity, PEER, None).unwrap();
        }

        let reopened = Database::open(&path).unwrap();
        assert_eq!(reopened.trust_state_of(PEER).unwrap(), TrustState::Revoked);
        assert_eq!(reopened.trust_audit_log(Some(PEER), 100).unwrap().len(), 2);
    }

    // --- Audit log ---------------------------------------------------------

    #[test]
    fn the_audit_log_records_who_decided_what_and_when() {
        let f = fixture();
        register_peer(&f);
        f.db.record_enrollment_request(&f.identity, PEER, "SM-BBBBB", &[])
            .unwrap();
        f.db.approve_peer(&f.identity, PEER, Some("verified in person")).unwrap();

        let log = f.db.trust_audit_log(Some(PEER), 100).unwrap();
        assert_eq!(log.len(), 2);

        let approval = &log[0];
        assert_eq!(approval.kind, TrustEventKind::EnrollmentApproved);
        assert_eq!(approval.node_id, PEER);
        assert_eq!(approval.actor_node, f.identity.node_id());
        assert_eq!(approval.from_state, Some(TrustState::Pending));
        assert_eq!(approval.to_state, TrustState::Trusted);
        assert_eq!(approval.detail.as_deref(), Some("verified in person"));
    }

    #[test]
    fn audit_sequence_numbers_are_monotonic_and_order_the_log() {
        let f = fixture();
        register_peer(&f);
        f.db.record_enrollment_request(&f.identity, PEER, "SM-BBBBB", &[])
            .unwrap();
        f.db.approve_peer(&f.identity, PEER, None).unwrap();
        f.db.revoke_peer(&f.identity, PEER, None).unwrap();

        let log = f.db.trust_audit_log(None, 100).unwrap();
        let sequences: Vec<u64> = log.iter().map(|e| e.sequence).collect();

        // Newest first, strictly descending, with no gaps.
        assert_eq!(sequences, vec![3, 2, 1]);
    }

    #[test]
    fn audit_entries_are_signed_by_the_deciding_node() {
        let f = fixture();
        register_peer(&f);
        f.db.approve_peer(&f.identity, PEER, None).unwrap();

        let conn = f.db.conn();
        let (sequence, kind, from, to, actor, occurred, signature): (
            i64,
            String,
            Option<String>,
            String,
            String,
            String,
            String,
        ) = conn
            .query_row(
                "SELECT sequence, kind, from_state, to_state, actor_node, occurred_at, signature
                 FROM peer_trust_events WHERE node_id = ?1",
                params![PEER],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                    ))
                },
            )
            .unwrap();

        let bytes = trust_signing_bytes(
            sequence as u64,
            PEER,
            kind.parse().unwrap(),
            from.map(|f| f.parse().unwrap()),
            to.parse().unwrap(),
            &actor,
            &occurred,
        );

        assert!(crate::identity::verify_with_public_key(
            &f.identity.public_key_hex(),
            &bytes,
            &hex::decode(signature).unwrap(),
        ));
    }

    #[test]
    fn trust_signing_is_domain_separated_from_the_other_signing_contexts() {
        assert_ne!(TRUST_SIGNING_DOMAIN, crate::domain::event::EVENT_SIGNING_DOMAIN);
        assert_ne!(
            TRUST_SIGNING_DOMAIN,
            crate::networking::protocol::ENVELOPE_SIGNING_DOMAIN
        );
    }

    #[test]
    fn an_overlong_note_is_refused() {
        let f = fixture();
        register_peer(&f);
        let huge = "x".repeat(MAX_TRUST_NOTE_CHARS + 1);

        assert!(f.db.approve_peer(&f.identity, PEER, Some(&huge)).is_err());
        assert_eq!(f.db.trust_state_of(PEER).unwrap(), TrustState::Unknown);
    }

    #[test]
    fn counts_reflect_the_trust_store() {
        let f = fixture();
        register_peer(&f);
        assert_eq!(f.db.count_pending_peers().unwrap(), 0);
        assert_eq!(f.db.count_trusted_peers().unwrap(), 0);

        f.db.record_enrollment_request(&f.identity, PEER, "SM-BBBBB", &[])
            .unwrap();
        assert_eq!(f.db.count_pending_peers().unwrap(), 1);

        f.db.approve_peer(&f.identity, PEER, None).unwrap();
        assert_eq!(f.db.count_pending_peers().unwrap(), 0);
        // The local node is TRUSTED but is not a peer.
        assert_eq!(f.db.count_trusted_peers().unwrap(), 1);
    }

    #[test]
    fn announced_metadata_never_changes_authorization() {
        let f = fixture();
        register_peer(&f);
        f.db.approve_peer(&f.identity, PEER, None).unwrap();

        // A peer renaming itself and claiming new capabilities changes nothing.
        f.db.record_enrollment_request(
            &f.identity,
            PEER,
            "SM-TOTALLY-DIFFERENT",
            &["peer/enroll".to_string(), "admin".to_string()],
        )
        .unwrap();

        assert_eq!(f.db.trust_state_of(PEER).unwrap(), TrustState::Trusted);
        assert_eq!(f.db.role_of(PEER).unwrap(), PeerRole::Node);
    }
}
