//! Where other nodes say they are.
//!
//! # Ephemeral by design
//!
//! A peer's position is **operational state, not a record**. It lives in memory
//! and dies with the process, and that is deliberate:
//!
//! - it describes *now*, and a stale row surviving a crash would be a lie;
//! - a five-minute heartbeat writing to an append-only log would add hundreds of
//!   rows a day that nobody will ever read;
//! - an incident is something that happened, which is worth keeping forever. A
//!   node being somewhere five minutes ago is not.
//!
//! Nothing here reaches the event log, the incident table, or replication.
//!
//! # Ordering is by sequence, never by clock
//!
//! Two nodes do not share a clock. A heartbeat that arrives late, twice, or out
//! of order is identified by the origin's own monotonic counter — the same
//! principle the event log already uses for replication. Comparing wall-clock
//! timestamps would let a peer with a fast clock overwrite fresher data, and
//! would make the ordering depend on something neither node controls.
//!
//! # Freshness is not reachability
//!
//! A node can be connected and unable to see the sky. `ONLINE` and "we know
//! where it is" are separate questions, and conflating them would show an
//! operator a confident position for a node whose last fix was an hour ago.

use crate::domain::incident::LocationSource;
use crate::error::{CoreError, CoreResult};
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::collections::HashMap;

/// How long a position is treated as describing where a node is *now*.
pub const CURRENT_FOR_SECONDS: i64 = 5 * 60;

/// How long a position remains worth showing, marked as ageing.
///
/// Three heartbeat intervals. One missed heartbeat is ordinary — a fix can fail
/// for a hundred reasons — so a position is not written off until several have
/// been missed in a row.
pub const EXPIRES_AFTER_SECONDS: i64 = 15 * 60;

/// How much of a position an operator should trust, given its age.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum LocationFreshness {
    /// Updated within the heartbeat interval. Describes where the node is.
    Current,
    /// One or two heartbeats missed. Probably still right; no longer certain.
    Stale,
    /// Old enough that it says where the node *was*, not where it is.
    Expired,
}

impl LocationFreshness {
    pub fn as_str(self) -> &'static str {
        match self {
            LocationFreshness::Current => "CURRENT",
            LocationFreshness::Stale => "STALE",
            LocationFreshness::Expired => "EXPIRED",
        }
    }

    /// Whether this position may be drawn as a node's present location.
    ///
    /// An expired position is still shown — as a last known position — but it
    /// must not be framed, counted, or presented as current.
    pub fn is_positionable(self) -> bool {
        !matches!(self, LocationFreshness::Expired)
    }
}

/// A position a peer reported, with everything needed to judge it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerLocation {
    /// The **authenticated** peer this came from.
    ///
    /// Taken from the transport session, never from the message body. A node
    /// cannot report a position on another node's behalf, because there is no
    /// field in which it could name one.
    pub node_id: String,
    pub latitude: f64,
    pub longitude: f64,
    pub accuracy_meters: Option<f64>,
    pub source: LocationSource,
    /// When the sender measured the position.
    pub captured_at: DateTime<Utc>,
    /// When this node received it. Deliberately distinct from `captured_at`:
    /// a node can send an hour-old fix a second ago.
    pub received_at: DateTime<Utc>,
    /// The origin's own monotonic counter, which decides what is newer.
    pub sequence: u64,
}

impl PeerLocation {
    /// How much of this position to trust, as of `now`.
    ///
    /// Measured from `received_at` rather than `captured_at`, because the
    /// question this answers is "is this node still telling us where it is?".
    /// A node reporting an old fix promptly is a different situation from a
    /// node that has gone quiet, and the popup shows both figures.
    pub fn freshness(&self, now: DateTime<Utc>) -> LocationFreshness {
        let age = (now - self.received_at).num_seconds();

        if age < CURRENT_FOR_SECONDS {
            LocationFreshness::Current
        } else if age < EXPIRES_AFTER_SECONDS {
            LocationFreshness::Stale
        } else {
            LocationFreshness::Expired
        }
    }
}

/// A peer's position as the UI needs it: the record plus its judged freshness.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerLocationView {
    #[serde(flatten)]
    pub location: PeerLocation,
    pub freshness: LocationFreshness,
    /// Seconds since this position was received, for "updated N min ago".
    pub age_seconds: i64,
}

/// Why a heartbeat was not accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeartbeatRejection {
    /// The sequence is not newer than what is already held.
    NotNewer { held: u64, offered: u64 },
}

/// The latest position held for each peer.
///
/// **One entry per peer, always the newest.** Not a queue: a heartbeat that
/// arrives while an older one is still held replaces it outright. Nothing
/// accumulates, so a peer that was unreachable for an hour costs exactly one
/// entry when it returns, not twelve.
#[derive(Debug, Default)]
pub struct PeerLocationBook {
    latest: HashMap<String, PeerLocation>,
}

impl PeerLocationBook {
    pub fn new() -> Self {
        Self::default()
    }

    /// Records a position, if it is newer than what is held.
    ///
    /// Returns `Ok(true)` when this is the first position for a peer, which is
    /// the transition worth auditing. A routine update returns `Ok(false)`, and
    /// a stale, duplicate or reordered one is rejected.
    ///
    /// `node_id` comes from the authenticated session. Passing anything else
    /// would defeat the whole authorization model, which is why the type has no
    /// other way in.
    pub fn accept(&mut self, location: PeerLocation) -> Result<bool, HeartbeatRejection> {
        if let Some(held) = self.latest.get(&location.node_id) {
            // `<=` rather than `<`: a repeated sequence carries no new
            // information and may be a replayed packet.
            if location.sequence <= held.sequence {
                return Err(HeartbeatRejection::NotNewer {
                    held: held.sequence,
                    offered: location.sequence,
                });
            }
        }

        let first = !self.latest.contains_key(&location.node_id);
        self.latest.insert(location.node_id.clone(), location);
        Ok(first)
    }

    /// The position held for one peer, if any.
    pub fn get(&self, node_id: &str) -> Option<&PeerLocation> {
        self.latest.get(node_id)
    }

    /// Every position held, with freshness judged as of `now`.
    ///
    /// Expired entries are **included**. They are shown as a last known
    /// position rather than deleted, because "we last saw it here, 18 minutes
    /// ago" is useful and "we know nothing" is not.
    pub fn view(&self, now: DateTime<Utc>) -> Vec<PeerLocationView> {
        let mut views: Vec<PeerLocationView> = self
            .latest
            .values()
            .map(|location| PeerLocationView {
                freshness: location.freshness(now),
                age_seconds: (now - location.received_at).num_seconds().max(0),
                location: location.clone(),
            })
            .collect();

        views.sort_by(|a, b| a.location.node_id.cmp(&b.location.node_id));
        views
    }

    /// Peers whose position has aged past the point of being usable.
    ///
    /// Reported so the transition can be audited once, rather than a heartbeat
    /// being logged every five minutes.
    pub fn expired(&self, now: DateTime<Utc>) -> Vec<&str> {
        self.latest
            .values()
            .filter(|location| location.freshness(now) == LocationFreshness::Expired)
            .map(|location| location.node_id.as_str())
            .collect()
    }

    /// Drops a peer's position entirely.
    ///
    /// Used when a peer is revoked: authorization is withdrawn, so what it told
    /// us about itself should not linger on the map.
    pub fn forget(&mut self, node_id: &str) -> bool {
        self.latest.remove(node_id).is_some()
    }

    pub fn len(&self) -> usize {
        self.latest.len()
    }

    pub fn is_empty(&self) -> bool {
        self.latest.is_empty()
    }
}

/// The contents of a location heartbeat, validated.
///
/// Deliberately carries no node identifier. The authenticated sender of the
/// envelope *is* the subject, so there is no field for a peer to put another
/// node's name in — the spoofing question is removed rather than checked.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LocationReport {
    pub latitude: f64,
    pub longitude: f64,
    pub accuracy_meters: Option<f64>,
    pub source: LocationSource,
    pub captured_at: DateTime<Utc>,
    pub sequence: u64,
}

impl LocationReport {
    /// Validates a report as it arrives from the network.
    ///
    /// Coordinates go through the same [`crate::domain::Location`] constructor
    /// the incident path uses, so a peer cannot publish a position an operator
    /// at this machine would have been refused.
    pub fn validated(self) -> CoreResult<Self> {
        let position = crate::domain::Location::new(
            self.latitude,
            self.longitude,
            self.accuracy_meters,
            self.source,
            Some(self.captured_at),
        )?;

        if self.sequence == 0 {
            return Err(CoreError::validation(
                "a location heartbeat must carry a sequence of at least 1",
            ));
        }

        Ok(Self {
            latitude: position.latitude,
            longitude: position.longitude,
            accuracy_meters: position.accuracy_meters,
            source: position.source,
            captured_at: position.captured_at.unwrap_or(self.captured_at),
            sequence: self.sequence,
        })
    }

    /// Attributes this report to the peer that actually sent it.
    pub fn attributed_to(self, node_id: &str, received_at: DateTime<Utc>) -> PeerLocation {
        PeerLocation {
            node_id: node_id.to_string(),
            latitude: self.latitude,
            longitude: self.longitude,
            accuracy_meters: self.accuracy_meters,
            source: self.source,
            captured_at: self.captured_at,
            received_at,
            sequence: self.sequence,
        }
    }
}

/// Degrees per unit of the fixed-point wire representation.
///
/// 1e-7 degrees is roughly a centimetre — finer than any source SecureMesh
/// reads, and comfortably inside an `i32` for the whole coordinate range.
pub const COORDINATE_SCALE: f64 = 1e7;

impl LocationReport {
    /// Latitude in the fixed-point units the wire uses.
    pub fn latitude_e7(&self) -> i32 {
        (self.latitude * COORDINATE_SCALE).round() as i32
    }

    /// Longitude in the fixed-point units the wire uses.
    pub fn longitude_e7(&self) -> i32 {
        (self.longitude * COORDINATE_SCALE).round() as i32
    }

    /// Accuracy in millimetres, or absent when none was reported.
    ///
    /// Saturating rather than wrapping: an implausibly large radius is already
    /// refused by validation, and a silent wrap would turn one into a
    /// convincingly small one.
    pub fn accuracy_mm(&self) -> Option<u64> {
        self.accuracy_meters
            .map(|metres| (metres * 1000.0).round().max(0.0) as u64)
    }

    /// Rebuilds a report from the fixed-point form received on the wire.
    pub fn from_wire(
        latitude_e7: i32,
        longitude_e7: i32,
        accuracy_mm: Option<u64>,
        source: LocationSource,
        captured_at: DateTime<Utc>,
        sequence: u64,
    ) -> Self {
        Self {
            latitude: f64::from(latitude_e7) / COORDINATE_SCALE,
            longitude: f64::from(longitude_e7) / COORDINATE_SCALE,
            accuracy_meters: accuracy_mm.map(|mm| mm as f64 / 1000.0),
            source,
            captured_at,
            sequence,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(seconds: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_800_000_000 + seconds, 0).unwrap()
    }

    fn report(sequence: u64) -> LocationReport {
        LocationReport {
            latitude: 13.133599,
            longitude: 77.565330,
            accuracy_meters: Some(165.0),
            source: LocationSource::Wireless,
            captured_at: at(0),
            sequence,
        }
    }

    fn location(node_id: &str, sequence: u64, received: i64) -> PeerLocation {
        report(sequence).attributed_to(node_id, at(received))
    }

    // --- Sequence ordering ------------------------------------------------

    #[test]
    fn a_first_heartbeat_is_recorded_and_reported_as_new() {
        let mut book = PeerLocationBook::new();
        assert_eq!(book.accept(location("peer", 1, 0)), Ok(true));
        assert_eq!(book.len(), 1);
    }

    #[test]
    fn a_newer_sequence_replaces_the_one_held() {
        let mut book = PeerLocationBook::new();
        book.accept(location("peer", 1, 0)).unwrap();

        // Not "new" — the peer was already on the map, so nothing to announce.
        assert_eq!(book.accept(location("peer", 2, 300)), Ok(false));
        assert_eq!(book.get("peer").unwrap().sequence, 2);
    }

    #[test]
    fn a_repeated_sequence_is_rejected() {
        // A duplicate carries no new information and may be a replay.
        let mut book = PeerLocationBook::new();
        book.accept(location("peer", 7, 0)).unwrap();

        assert_eq!(
            book.accept(location("peer", 7, 300)),
            Err(HeartbeatRejection::NotNewer {
                held: 7,
                offered: 7
            })
        );
        assert_eq!(book.get("peer").unwrap().received_at, at(0));
    }

    #[test]
    fn a_reordered_heartbeat_cannot_overwrite_a_newer_one() {
        // The packet that left first can arrive second. Sequence is what
        // decides, which is why wall-clock time is not consulted.
        let mut book = PeerLocationBook::new();
        book.accept(location("peer", 9, 0)).unwrap();

        assert!(book.accept(location("peer", 8, 1)).is_err());
        assert_eq!(book.get("peer").unwrap().sequence, 9);
    }

    #[test]
    fn a_later_wall_clock_does_not_beat_a_higher_sequence() {
        // Two nodes do not share a clock. If time decided, a peer with a fast
        // clock could overwrite fresher data.
        let mut book = PeerLocationBook::new();
        book.accept(location("peer", 5, 0)).unwrap();

        let mut stale_but_recent = location("peer", 4, 10_000);
        stale_but_recent.captured_at = at(10_000);
        assert!(book.accept(stale_but_recent).is_err());
        assert_eq!(book.get("peer").unwrap().sequence, 5);
    }

    #[test]
    fn each_peer_keeps_its_own_sequence() {
        let mut book = PeerLocationBook::new();
        book.accept(location("a", 50, 0)).unwrap();

        // Low sequence from a different peer is not stale; it is unrelated.
        assert_eq!(book.accept(location("b", 1, 0)), Ok(true));
        assert_eq!(book.len(), 2);
    }

    // --- Coalescing -------------------------------------------------------

    #[test]
    fn only_the_latest_position_is_retained() {
        // One entry per peer, never a queue. A peer unreachable for an hour
        // costs one entry when it returns, not twelve.
        let mut book = PeerLocationBook::new();
        for sequence in 1..=12 {
            book.accept(location("peer", sequence, sequence as i64 * 300))
                .unwrap();
        }

        assert_eq!(book.len(), 1);
        assert_eq!(book.get("peer").unwrap().sequence, 12);
    }

    // --- Freshness --------------------------------------------------------

    #[test]
    fn a_fresh_position_is_current() {
        let held = location("peer", 1, 0);
        assert_eq!(held.freshness(at(0)), LocationFreshness::Current);
        assert_eq!(held.freshness(at(299)), LocationFreshness::Current);
    }

    #[test]
    fn a_position_ages_into_stale_then_expired() {
        let held = location("peer", 1, 0);

        assert_eq!(held.freshness(at(300)), LocationFreshness::Stale);
        assert_eq!(held.freshness(at(899)), LocationFreshness::Stale);
        assert_eq!(held.freshness(at(900)), LocationFreshness::Expired);
        assert_eq!(held.freshness(at(10_000)), LocationFreshness::Expired);
    }

    #[test]
    fn an_expired_position_is_kept_but_not_treated_as_a_position() {
        // "We last saw it here, 18 minutes ago" is useful. Deleting it and
        // showing nothing is not.
        let mut book = PeerLocationBook::new();
        book.accept(location("peer", 1, 0)).unwrap();

        let view = book.view(at(1_200));
        assert_eq!(view.len(), 1, "the last known position is kept");
        assert_eq!(view[0].freshness, LocationFreshness::Expired);
        assert!(!view[0].freshness.is_positionable());
        assert_eq!(view[0].age_seconds, 1_200);
    }

    #[test]
    fn a_fresh_heartbeat_makes_an_expired_position_current_again() {
        let mut book = PeerLocationBook::new();
        book.accept(location("peer", 1, 0)).unwrap();
        assert_eq!(book.expired(at(1_000)), vec!["peer"]);

        // The peer came back.
        book.accept(location("peer", 2, 1_000)).unwrap();
        assert!(book.expired(at(1_000)).is_empty());
        assert_eq!(
            book.view(at(1_000))[0].freshness,
            LocationFreshness::Current
        );
    }

    #[test]
    fn received_time_and_capture_time_are_not_confused() {
        // A node can report an hour-old fix a second ago. Both figures matter
        // and they answer different questions.
        let mut held = location("peer", 1, 3_600);
        held.captured_at = at(0);

        assert_eq!(held.freshness(at(3_600)), LocationFreshness::Current);
        assert_eq!((held.received_at - held.captured_at).num_seconds(), 3_600);
    }

    // --- Withdrawal -------------------------------------------------------

    #[test]
    fn a_forgotten_peer_leaves_no_position_behind() {
        let mut book = PeerLocationBook::new();
        book.accept(location("peer", 1, 0)).unwrap();

        assert!(book.forget("peer"));
        assert!(book.is_empty());
        assert!(!book.forget("peer"), "forgetting twice is not an error");
    }

    // --- Validation -------------------------------------------------------

    #[test]
    fn a_valid_report_passes_and_keeps_its_provenance() {
        let checked = report(1).validated().unwrap();

        assert_eq!(checked.accuracy_meters, Some(165.0));
        // Wireless stays wireless. Nothing promotes it to a satellite fix.
        assert_eq!(checked.source, LocationSource::Wireless);
    }

    #[test]
    fn an_out_of_range_coordinate_is_refused() {
        // The same constructor the incident path uses, so a peer cannot publish
        // a position an operator here would have been refused.
        for (latitude, longitude) in [(91.0, 0.0), (-91.0, 0.0), (0.0, 181.0), (0.0, -181.0)] {
            let mut bad = report(1);
            bad.latitude = latitude;
            bad.longitude = longitude;
            assert!(bad.validated().is_err(), "{latitude},{longitude}");
        }
    }

    #[test]
    fn a_non_finite_coordinate_is_refused() {
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let mut bad = report(1);
            bad.latitude = value;
            assert!(bad.validated().is_err());
        }
    }

    #[test]
    fn a_nonsensical_accuracy_is_refused() {
        for value in [-1.0, f64::NAN, 1.0e12] {
            let mut bad = report(1);
            bad.accuracy_meters = Some(value);
            assert!(bad.validated().is_err(), "{value}");
        }
    }

    #[test]
    fn a_missing_accuracy_is_accepted_as_unknown() {
        // Absent, never zero. Zero would claim a perfect fix.
        let mut unmeasured = report(1);
        unmeasured.accuracy_meters = None;
        assert_eq!(unmeasured.validated().unwrap().accuracy_meters, None);
    }

    #[test]
    fn a_sequence_of_zero_is_refused() {
        // Sequences start at 1, so zero means the sender never published.
        let mut bad = report(0);
        bad.sequence = 0;
        assert!(bad.validated().is_err());
    }

    #[test]
    fn a_report_carries_no_node_identifier_to_spoof() {
        // Attribution comes from the authenticated session, and there is no
        // field in which a peer could name a different node.
        let attributed = report(1).attributed_to("the-real-sender", at(0));
        assert_eq!(attributed.node_id, "the-real-sender");
    }
}
