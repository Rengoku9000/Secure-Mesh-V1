//! The synchronisation engine.
//!
//! Reconciles this node's event log with its peers. The engine is deliberately
//! **synchronous and transport-agnostic**: it is driven by explicit `tick`
//! calls over a [`MeshTransport`], so its behaviour under partition, replay and
//! reordering is a decision to be asserted rather than a race to be hoped for.
//!
//! # The protocol in one paragraph
//!
//! On connecting, a node sends `SYNC_REQUEST` carrying its watermark for every
//! origin it knows. The peer answers with `SYNC_RESPONSE` (what it can offer)
//! followed by `EVENT_BATCH` messages containing the contiguous runs the
//! requester lacks. The requester verifies each event independently, applies
//! it, and returns `ACK` with its new contiguous watermark. The responder
//! records that acknowledgement durably, which is what lets it stop resending.
//!
//! ```text
//!   A                                    B
//!   │── SYNC_REQUEST  have:{A:5, B:2} ──▶│
//!   │◀─ SYNC_RESPONSE available:{B:7} ───│
//!   │◀─ EVENT_BATCH   B:3..7 ────────────│
//!   │── ACK           B accepted:7 ─────▶│   (persisted by B)
//! ```
//!
//! # Why pull, not push
//!
//! The requester states what it has and the responder computes the difference.
//! A node that was offline for a week asks exactly one question and receives
//! exactly what it missed — no per-peer outbound queue to keep in step with the
//! log, and no way for the two to disagree. This is why there is no separate
//! "unsent events" buffer: the log *is* the queue, and
//! `peer_ack_watermarks` records how far each peer has got.
//!
//! # Safety properties
//!
//! - **Idempotent.** Applying an event twice is a no-op at the storage layer,
//!   so duplicate batches, replayed messages and repeated sync rounds all
//!   converge to the same state.
//! - **Order-independent.** Events are stored on arrival regardless of order;
//!   the watermark only advances over a contiguous run, so a gap re-requests
//!   itself on the next round.
//! - **Restart-safe.** Every piece of sync state lives in SQLite. Nothing that
//!   matters is held in memory.
//! - **Authenticated end to end.** Each event is verified against its *origin's*
//!   key, not the key of the peer that delivered it.

pub mod link;
pub mod observe;

pub use link::{LinkSnapshot, LinkState, PeerLink, SyncTrigger};
use observe::{observe, SyncLog};

use crate::domain::trust::{Capability, TrustState};
use crate::domain::MeshEvent as DomainEvent;
use crate::error::{CoreError, CoreResult};
use crate::identity::NodeIdentity;
use crate::security::{audit, AuditEvent, AuditOutcome};
use crate::networking::protocol::{Envelope, MessageBody, OriginWatermark};
use crate::networking::{MeshEvent, MeshTransport, PeerDescriptor};
use crate::storage::events::{ApplyOutcome, MAX_SYNC_BATCH};
use crate::storage::Database;
use std::collections::HashMap;

/// Outcome of one engine tick, for tests, logging, and the dashboard.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SyncReport {
    /// Peers that opened a session during this tick.
    pub peers_connected: usize,
    /// Peers whose session ended.
    pub peers_disconnected: usize,
    /// Events accepted and applied.
    pub events_applied: usize,
    /// Events already held, and therefore ignored.
    pub events_duplicate: usize,
    /// Events refused: bad signature, wrong origin binding, or malformed.
    pub events_rejected: usize,
    /// Equivocations detected.
    pub conflicts_detected: usize,
    /// Messages that failed to decode or validate.
    pub messages_rejected: usize,
    /// Messages accepted and handled.
    ///
    /// Counted so that a tick which did real work — served a sync request,
    /// answered a handshake — is distinguishable from an idle one. Without it a
    /// productive tick looks identical to a quiet one, and anything driving the
    /// engine to quiescence stops a round too early.
    pub messages_processed: usize,
}

impl SyncReport {
    fn merge(&mut self, other: SyncReport) {
        self.peers_connected += other.peers_connected;
        self.peers_disconnected += other.peers_disconnected;
        self.events_applied += other.events_applied;
        self.events_duplicate += other.events_duplicate;
        self.events_rejected += other.events_rejected;
        self.conflicts_detected += other.conflicts_detected;
        self.messages_rejected += other.messages_rejected;
        self.messages_processed += other.messages_processed;
    }
}

/// How often the reconciliation sweep runs.
///
/// **This is a safety net, not the mechanism.** Every legitimate cause of
/// synchronisation has its own trigger (see [`SyncTrigger`]); the sweep exists
/// only to recover from a message lost to a transport failure that produced no
/// disconnection event. Phase 2.5 had this at five seconds and depended on it,
/// which is why an approval could sit idle: the timer *was* the trigger.
///
/// It is deliberately slow. If correctness ever appears to depend on making it
/// faster, a trigger is missing and shortening the interval would only hide
/// that — the determinism tests in `tests/sync_determinism.rs` never run it at
/// all.
pub const RECONCILE_INTERVAL_SECS: u64 = 60;

/// Reconciles the local event log with connected peers.
pub struct SyncEngine<T: MeshTransport> {
    transport: T,
    /// Live sessions and where each has reached in its lifecycle.
    ///
    /// Deliberately in memory: it describes *now*, and a stale "mid-sync" row
    /// surviving a crash would be a lie. Everything durable is in the event log
    /// and the trust store.
    links: HashMap<String, PeerLink>,
}

impl<T: MeshTransport> SyncEngine<T> {
    pub fn new(transport: T) -> Self {
        Self {
            transport,
            links: HashMap::new(),
        }
    }

    pub fn transport(&self) -> &T {
        &self.transport
    }

    /// Peers with an open authenticated session.
    pub fn connected_peers(&self) -> Vec<PeerDescriptor> {
        self.links
            .values()
            .map(|link| PeerDescriptor {
                node_id: link.node_id.clone(),
                public_key: link.public_key.clone(),
                transport_peer_id: link.transport_peer_id.clone(),
            })
            .collect()
    }

    /// The lifecycle state of every live session, for diagnostics and the UI.
    pub fn link_snapshots(&self) -> Vec<LinkSnapshot> {
        self.links.values().map(LinkSnapshot::from).collect()
    }

    /// Processes everything the transport has delivered since the last tick.
    ///
    /// Called on a timer by the runtime. Each tick is a complete, independent
    /// unit of work: nothing is carried between ticks in memory, so a tick that
    /// is interrupted loses no progress.
    pub fn tick(
        &mut self,
        database: &Database,
        identity: &NodeIdentity,
    ) -> CoreResult<SyncReport> {
        let mut report = SyncReport::default();

        // `poll_events` drains the transport, so every event in this batch has
        // already left the inbox. Failing out of the loop would discard the
        // rest of them permanently — including, in the worst case, a
        // `PeerConnected` whose loss leaves a peer connected but invisible to
        // the engine. Each event is therefore handled independently and its
        // failure recorded rather than propagated.
        for event in self.transport.poll_events() {
            match event {
                MeshEvent::PeerConnected(peer) => {
                    report.peers_connected += 1;
                    if let Err(error) = self.on_peer_connected(database, identity, peer) {
                        observe(SyncLog::Failed {
                            peer: "unknown",
                            reason: error.message(),
                        });
                        report.messages_rejected += 1;
                    }
                }
                MeshEvent::PeerDisconnected { node_id } => {
                    report.peers_disconnected += 1;
                    self.links.remove(&node_id);
                    observe(SyncLog::Disconnected { peer: &node_id });
                    if let Err(error) = database.mark_node_offline(&node_id) {
                        observe(SyncLog::Failed {
                            peer: &node_id,
                            reason: error.message(),
                        });
                    }
                }
                MeshEvent::MessageReceived { from, envelope } => {
                    match self.on_message(database, identity, &from, envelope) {
                        Ok(partial) => {
                            report.merge(partial);
                            report.messages_processed += 1;
                        }
                        Err(error) => {
                            // A peer sending something unusable is a routine
                            // condition, not a reason to stop serving others.
                            observe(SyncLog::Rejected {
                                peer: &from.node_id,
                                reason: error.message(),
                            });
                            report.messages_rejected += 1;
                        }
                    }
                }
            }
        }

        // **Trigger: this node's knowledge grew.**
        //
        // Events accepted from one peer may be missing at another, and a relay
        // that stayed quiet about what it just learned would leave a multi-hop
        // path stuck halfway. Opening a round with every peer is safe because a
        // round carries only watermarks: a peer that is already up to date
        // simply finds nothing to ask for.
        //
        // This terminates. A round is only productive while some peer is
        // strictly behind, and every productive round closes that gap.
        if report.events_applied > 0 {
            self.open_rounds(database, identity, SyncTrigger::Relay)?;
        }

        Ok(report)
    }

    /// Opens a round with one peer, if it is authorized and reachable.
    ///
    /// The single entry point for every trigger. Having exactly one means a new
    /// cause of synchronisation cannot accidentally acquire its own slightly
    /// different rules — which is how the Phase 2.5 gap arose, where connection
    /// had a path and authorization did not.
    fn open_round(
        &mut self,
        database: &Database,
        identity: &NodeIdentity,
        peer_node_id: &str,
        trigger: SyncTrigger,
    ) -> CoreResult<bool> {
        let trust = database.trust_state_of(peer_node_id)?;

        let Some(link) = self.links.get_mut(peer_node_id) else {
            return Ok(false); // Not connected; the next connection will trigger.
        };
        link.apply_trust(trust);

        if !link.begin_round(trigger) {
            return Ok(false);
        }

        observe(SyncLog::Started {
            peer: peer_node_id,
            trigger,
        });
        self.request_sync(database, identity, peer_node_id)?;
        Ok(true)
    }

    /// Opens a round with every connected peer that is authorized.
    fn open_rounds(
        &mut self,
        database: &Database,
        identity: &NodeIdentity,
        trigger: SyncTrigger,
    ) -> CoreResult<usize> {
        let peers: Vec<String> = self.links.keys().cloned().collect();
        let mut opened = 0;
        for peer in peers {
            if self.open_round(database, identity, &peer, trigger)? {
                opened += 1;
            }
        }
        Ok(opened)
    }

    /// **Trigger: this node authorized a peer.**
    ///
    /// Called by the runtime the moment an operator approves or reinstates, so
    /// replication starts from the decision itself rather than from whenever
    /// the next sweep happens to run.
    pub fn on_authorization_changed(
        &mut self,
        database: &Database,
        identity: &NodeIdentity,
        peer_node_id: &str,
    ) -> CoreResult<()> {
        let trust = database.trust_state_of(peer_node_id)?;
        if let Some(link) = self.links.get_mut(peer_node_id) {
            link.apply_trust(trust);
        }
        observe(SyncLog::Authorization {
            peer: peer_node_id,
            state: trust,
        });

        if trust.permits_authorized_operations() {
            self.open_round(database, identity, peer_node_id, SyncTrigger::LocalAuthorization)?;
        }
        Ok(())
    }

    /// **Trigger: a local event was appended.**
    ///
    /// Sync is pull-based, so this does not push the event. It opens a round,
    /// which carries this node's watermarks; the peer sees it is behind and
    /// asks. One mechanism, whichever side has the data.
    pub fn on_local_event(
        &mut self,
        database: &Database,
        identity: &NodeIdentity,
    ) -> CoreResult<()> {
        self.open_rounds(database, identity, SyncTrigger::LocalEvent)?;
        Ok(())
    }

    /// **Trigger: the periodic safety net.** See [`RECONCILE_INTERVAL_SECS`].
    pub fn reconcile(&mut self, database: &Database, identity: &NodeIdentity) -> CoreResult<()> {
        self.open_rounds(database, identity, SyncTrigger::Reconciliation)?;
        Ok(())
    }

    /// Registers a newly authenticated peer and opens a sync round.
    fn on_peer_connected(
        &mut self,
        database: &Database,
        identity: &NodeIdentity,
        peer: PeerDescriptor,
    ) -> CoreResult<()> {
        // The transport has already proved this peer holds the key behind its
        // node ID; re-checking the binding here is cheap and means a broken
        // transport cannot quietly widen who we trust.
        if !crate::identity::node_id_matches_key(&peer.node_id, &peer.public_key) {
            return Err(CoreError::validation(
                "transport surfaced a peer whose node ID does not match its key",
            ));
        }

        // Registering records that this key exists and is reachable. It is
        // deliberately *not* an authorization decision: a peer that has never
        // been enrolled stays UNKNOWN, and UNKNOWN denies everything that
        // matters.
        database.register_peer(
            &peer.node_id,
            &peer.public_key,
            Some(&peer.transport_peer_id),
        )?;

        let node_id = peer.node_id.clone();
        self.links.insert(
            node_id.clone(),
            PeerLink::new(peer.node_id, peer.public_key, peer.transport_peer_id),
        );
        observe(SyncLog::Connected { peer: &node_id });

        // The handshake is offered to every authenticated peer regardless of
        // trust, so an unenrolled node can present itself and an operator can
        // see it waiting. It carries no operational data.
        self.send(
            identity,
            &node_id,
            MessageBody::Hello {
                protocol_version: crate::networking::protocol::PROTOCOL_VERSION,
                node_name: identity.node_name().to_string(),
                capabilities: capabilities(),
            },
        )?;

        // **Trigger: connection.** Only toward an authorized peer — asking an
        // unenrolled node for its log would disclose which origins this node
        // holds. An unauthorized peer sits in AwaitingAuthorization until an
        // operator acts, which is then its own trigger.
        let trust = database.trust_state_of(&node_id)?;
        if let Some(link) = self.links.get_mut(&node_id) {
            link.apply_trust(trust);
        }
        if trust.permits_authorized_operations() {
            self.open_round(database, identity, &node_id, SyncTrigger::Connected)?;
        }
        Ok(())
    }

    /// Refuses an operation unless the peer is authorized for it.
    ///
    /// The single gate every operational message passes through. It reads the
    /// trust store on each call rather than trusting a cached value, so a
    /// revocation takes effect on the very next message — including on a
    /// session that was already open when the operator revoked.
    fn authorize(
        database: &Database,
        peer_node_id: &str,
        capability: Capability,
    ) -> CoreResult<()> {
        let state = database.trust_state_of(peer_node_id)?;
        if !state.permits_authorized_operations() {
            audit(
                AuditEvent::AuthorizationDenied,
                AuditOutcome::Failure,
                &format!("peer={peer_node_id} state={state} capability={capability}"),
            );
            return Err(CoreError::validation(format!(
                "peer is {state} and is not authorized for {capability}"
            )));
        }

        let role = database.role_of(peer_node_id)?;
        if !role.grants(capability) {
            audit(
                AuditEvent::AuthorizationDenied,
                AuditOutcome::Failure,
                &format!("peer={peer_node_id} role={role} capability={capability}"),
            );
            return Err(CoreError::validation(format!(
                "peer's role {role} does not grant {capability}"
            )));
        }

        Ok(())
    }

    /// Asks a peer for everything this node is missing.
    fn request_sync(
        &self,
        database: &Database,
        identity: &NodeIdentity,
        peer_node_id: &str,
    ) -> CoreResult<()> {
        let watermarks = database.sync_watermarks()?;
        observe(SyncLog::LocalKnowledge {
            peer: peer_node_id,
            origins: watermarks.len(),
            events: watermarks.iter().map(|(_, mark)| mark).sum(),
        });

        let have = watermarks
            .into_iter()
            .map(|(origin_node, watermark)| OriginWatermark {
                origin_node,
                watermark,
            })
            .collect();

        self.send(identity, peer_node_id, MessageBody::SyncRequest { have })
    }

    /// Handles one validated message.
    fn on_message(
        &mut self,
        database: &Database,
        identity: &NodeIdentity,
        from: &PeerDescriptor,
        envelope: Envelope,
    ) -> CoreResult<SyncReport> {
        // The envelope's own signature must match the peer the transport says
        // delivered it, so a peer cannot speak in another node's name.
        envelope.validate()?;
        if envelope.sender_node_id != from.node_id {
            return Err(CoreError::validation(
                "message sender does not match the authenticated peer",
            ));
        }

        let mut report = SyncReport::default();

        match envelope.body {
            MessageBody::Hello {
                protocol_version,
                ref node_name,
                ref capabilities,
            } => {
                database.record_peer_protocol(&from.node_id, protocol_version)?;

                // HELLO *is* the enrollment request. It already carries the
                // presented name, capabilities and version, and is signed by
                // the peer's key, so a separate message type would add wire
                // surface without adding information.
                //
                // This can only move UNKNOWN to PENDING. It never grants
                // anything, and it cannot lift a REVOKED peer back out of
                // denial — otherwise reconnecting would launder a revocation.
                let state = database.record_enrollment_request(
                    identity,
                    &from.node_id,
                    node_name,
                    capabilities,
                )?;

                // The handshake completes here: version confirmed, identity
                // confirmed, authorization folded in.
                if let Some(link) = self.links.get_mut(&from.node_id) {
                    link.authenticated(protocol_version, state);
                }
                observe(SyncLog::Authenticated {
                    peer: &from.node_id,
                    protocol_version,
                    state,
                });

                self.send(
                    identity,
                    &from.node_id,
                    MessageBody::PeerInfo {
                        node_name: identity.node_name().to_string(),
                        public_key: identity.public_key_hex(),
                        capabilities: capabilities_for(state),
                    },
                )?;

                // **Trigger: handshake completed on an authorized link.**
                // Covers the case where this node received the connection
                // rather than opening it, so neither side depends on having
                // been the initiator.
                if state.permits_authorized_operations() {
                    self.open_round(database, identity, &from.node_id, SyncTrigger::Connected)?;
                }
            }

            MessageBody::PeerInfo { public_key, .. } => {
                // A peer announcing a key other than the one it authenticated
                // with is either broken or hostile.
                if public_key != from.public_key {
                    return Err(CoreError::validation(
                        "peer announced a key different from the one it authenticated with",
                    ));
                }
            }

            // Everything below carries or requests operational data, so each
            // one is gated. An unauthorized peer gets an error, which the
            // caller counts as a rejected message; it never reaches storage.
            MessageBody::SyncRequest { have } => {
                Self::authorize(database, &from.node_id, Capability::IncidentSync)?;
                self.serve_sync_request(database, identity, &from.node_id, have, &envelope.message_id)?;
            }

            MessageBody::SyncResponse { .. } => {
                Self::authorize(database, &from.node_id, Capability::IncidentSync)?;
                // Advisory only: the batches carry the data. Nothing to do.
            }

            MessageBody::EventBatch { origin_node, events, complete } => {
                Self::authorize(database, &from.node_id, Capability::IncidentSync)?;
                let partial = self.apply_batch(
                    database,
                    identity,
                    from,
                    &origin_node,
                    events,
                    &envelope.message_id,
                )?;

                if let Some(link) = self.links.get_mut(&from.node_id) {
                    link.events_received += partial.events_applied as u64;
                    // The sender says it has nothing further for this origin,
                    // so the round is done and its latency can be reported.
                    if complete {
                        if let Some(duration_ms) = link.complete_round() {
                            observe(SyncLog::Completed {
                                peer: &from.node_id,
                                applied: partial.events_applied,
                                duration_ms,
                            });
                        }
                    }
                }
                report.merge(partial);
            }

            MessageBody::Ack { origin_node, accepted_through, .. } => {
                Self::authorize(database, &from.node_id, Capability::IncidentSync)?;
                database.record_peer_ack(&from.node_id, &origin_node, accepted_through)?;
                database.refresh_local_sync_status(identity.node_id())?;
            }

            MessageBody::Ping { nonce } => {
                self.send(identity, &from.node_id, MessageBody::Pong { nonce })?;
            }

            MessageBody::Pong { .. } => {}
        }

        database.touch_peer_seen(&from.node_id)?;
        Ok(report)
    }

    /// Answers a peer's `SYNC_REQUEST` with what it lacks.
    fn serve_sync_request(
        &mut self,
        database: &Database,
        identity: &NodeIdentity,
        peer_node_id: &str,
        peer_has: Vec<OriginWatermark>,
        request_id: &str,
    ) -> CoreResult<()> {
        let peer_watermarks: HashMap<String, u64> = peer_has
            .into_iter()
            .map(|w| (w.origin_node, w.watermark))
            .collect();
        observe(SyncLog::RemoteKnowledge {
            peer: peer_node_id,
            origins: peer_watermarks.len(),
        });

        let local = database.sync_watermarks()?;
        let local_watermarks: HashMap<&String, u64> =
            local.iter().map(|(origin, mark)| (origin, *mark)).collect();

        let mut available = Vec::new();
        for (origin_node, watermark) in &local {
            let peer_watermark = peer_watermarks.get(origin_node).copied().unwrap_or(0);
            if *watermark > peer_watermark {
                available.push(OriginWatermark {
                    origin_node: origin_node.clone(),
                    watermark: *watermark,
                });
            }
        }

        // **Trigger: the peer's knowledge is ahead of ours.**
        //
        // A request tells us what the *peer* holds as well as what it wants.
        // Reciprocating turns any single request, from either direction, into a
        // full bidirectional reconcile — which is what makes convergence
        // independent of who opened the connection or who wrote the data.
        //
        // It terminates because a counter-request is only sent while the peer
        // is *strictly* ahead, and every round closes the gap.
        let peer_is_ahead = peer_watermarks.iter().any(|(origin, peer_mark)| {
            *peer_mark > local_watermarks.get(origin).copied().unwrap_or(0)
        });
        if peer_is_ahead {
            observe(SyncLog::PeerAhead { peer: peer_node_id });
            // Routed through `open_round` rather than sending directly, so this
            // counts as a round with its own trigger and its own start time.
            // Measuring from here is what makes the reported latency the
            // duration of the exchange, rather than however long a round sat
            // open waiting for the other side's operator to approve.
            self.open_round(database, identity, peer_node_id, SyncTrigger::PeerAhead)?;
        }

        self.send(
            identity,
            peer_node_id,
            MessageBody::SyncResponse {
                in_reply_to: request_id.to_string(),
                available: available.clone(),
            },
        )?;

        // Record what the peer told us it holds, so an interrupted round still
        // leaves durable progress behind.
        for (origin_node, watermark) in &peer_watermarks {
            database.record_peer_ack(peer_node_id, origin_node, *watermark)?;
        }
        database.refresh_local_sync_status(identity.node_id())?;

        for offer in available {
            let peer_watermark = peer_watermarks.get(&offer.origin_node).copied().unwrap_or(0);
            let events =
                database.events_since(&offer.origin_node, peer_watermark, MAX_SYNC_BATCH)?;
            if events.is_empty() {
                continue;
            }

            let complete = events
                .last()
                .is_some_and(|last| last.origin_seq >= offer.watermark);

            observe(SyncLog::Sent {
                peer: peer_node_id,
                origin: &offer.origin_node,
                count: events.len(),
            });

            self.send(
                identity,
                peer_node_id,
                MessageBody::EventBatch {
                    origin_node: offer.origin_node.clone(),
                    events,
                    complete,
                },
            )?;
        }

        Ok(())
    }

    /// Verifies and applies a batch, then acknowledges what was accepted.
    fn apply_batch(
        &self,
        database: &Database,
        identity: &NodeIdentity,
        from: &PeerDescriptor,
        origin_node: &str,
        events: Vec<DomainEvent>,
        request_id: &str,
    ) -> CoreResult<SyncReport> {
        let mut report = SyncReport::default();

        for event in events {
            // Verification is against the *origin's* key, carried inside the
            // event, not against the peer that delivered it. This is what makes
            // relaying safe.
            if event.verify().is_err() || event.origin_node != origin_node {
                observe(SyncLog::EventRejected {
                    peer: &from.node_id,
                    origin: origin_node,
                    reason: "signature or origin binding failed verification",
                });
                report.events_rejected += 1;
                continue;
            }

            match database.apply_event(&event, identity.node_id(), Some(&from.node_id)) {
                Ok(ApplyOutcome::Stored) => {
                    observe(SyncLog::Applied {
                        peer: &from.node_id,
                        origin: origin_node,
                        sequence: event.origin_seq,
                    });
                    report.events_applied += 1;
                }
                Ok(ApplyOutcome::Duplicate) => report.events_duplicate += 1,
                Ok(ApplyOutcome::Conflict) => report.conflicts_detected += 1,
                Err(error) => {
                    observe(SyncLog::EventRejected {
                        peer: &from.node_id,
                        origin: origin_node,
                        reason: error.message(),
                    });
                    report.events_rejected += 1;
                }
            }
        }

        // Acknowledge the contiguous watermark actually reached, which may be
        // short of what was sent if the batch had a gap. The sender resends
        // from there, so nothing is silently lost.
        let accepted_through = database.watermark_for(origin_node)?;
        self.send(
            identity,
            &from.node_id,
            MessageBody::Ack {
                in_reply_to: request_id.to_string(),
                origin_node: origin_node.to_string(),
                accepted_through,
            },
        )?;

        Ok(report)
    }

    /// Signs and sends a message, tolerating an unreachable peer.
    fn send(
        &self,
        identity: &NodeIdentity,
        to: &str,
        body: MessageBody,
    ) -> CoreResult<()> {
        let envelope = Envelope::create(identity, body)?;
        // A send failure means the peer went away mid-round. That is expected
        // in the field: the log is durable and the next connection re-runs the
        // round from wherever it got to.
        let _ = self.transport.send(to, &envelope);
        Ok(())
    }

    /// Starts a fresh sync round with every connected peer.
    ///
    /// Called periodically so that events created *after* a session opened
    /// still propagate without waiting for a reconnection.
    pub fn sync_all_peers(
        &mut self,
        database: &Database,
        identity: &NodeIdentity,
    ) -> CoreResult<()> {
        self.open_rounds(database, identity, SyncTrigger::Manual)?;
        Ok(())
    }

    /// How many connected peers are currently authorized to synchronise.
    ///
    /// Diagnostic: "connected but nothing replicating" is otherwise
    /// indistinguishable from "connected and up to date".
    pub fn authorized_peer_count(&self, database: &Database) -> CoreResult<(usize, usize)> {
        let mut authorized = 0;
        for peer_node_id in self.links.keys() {
            if database
                .trust_state_of(peer_node_id)?
                .permits_authorized_operations()
            {
                authorized += 1;
            }
        }
        Ok((authorized, self.links.len()))
    }
}

/// Capabilities this build announces, for forward compatibility.
fn capabilities() -> Vec<String> {
    vec!["sync/1".to_string(), "incidents/1".to_string()]
}

/// Capabilities announced to a peer, reflecting what it is actually allowed.
///
/// An unauthorized peer is told it may enroll and nothing else. This is
/// courtesy, not enforcement — the receiving node is free to ignore it, and
/// this node refuses unauthorized operations regardless of what it advertised.
fn capabilities_for(state: TrustState) -> Vec<String> {
    if state.permits_authorized_operations() {
        capabilities()
    } else {
        vec!["enroll/1".to_string()]
    }
}
