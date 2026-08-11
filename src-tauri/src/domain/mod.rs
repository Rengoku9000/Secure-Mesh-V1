//! Domain types shared by the storage layer, the command layer, and (from
//! Phase 2) the sync engine.
//!
//! These types own their invariants: a value of type [`Incident`] has already
//! been validated, so no layer downstream needs to re-check it. Construction
//! from untrusted input goes through [`incident::NewIncident::validate`].

pub mod event;
pub mod incident;
pub mod node;
pub mod peer;
pub mod trust;

pub use event::{EventKind, IncidentCreatedPayload, IncidentObservationPayload, MeshEvent};
pub use incident::{Incident, NewIncident, Observation, Severity, SyncStatus};
pub use node::{NodeRecord, NodeStatus};
pub use peer::{ConnectionState, Peer};
pub use trust::{Capability, PeerRole, TrustEvent, TrustEventKind, TrustState};

use chrono::{DateTime, Utc};

/// The current time, truncated to the precision the storage layer records.
///
/// Timestamps are persisted as RFC 3339 strings with millisecond precision.
/// Taking `Utc::now()` directly would give a record sub-millisecond precision
/// in memory that it loses on the way to disk, so a freshly created record
/// would not compare equal to the same record read back. Truncating at
/// creation makes the in-memory value and the stored value identical.
pub fn now() -> DateTime<Utc> {
    let millis = Utc::now().timestamp_millis();
    DateTime::from_timestamp_millis(millis).unwrap_or_else(Utc::now)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Timelike;

    #[test]
    fn now_has_no_sub_millisecond_component() {
        for _ in 0..100 {
            assert_eq!(now().nanosecond() % 1_000_000, 0);
        }
    }

    #[test]
    fn now_is_monotonic_enough_to_be_a_real_clock() {
        let first = now();
        std::thread::sleep(std::time::Duration::from_millis(2));
        assert!(now() > first);
    }
}
