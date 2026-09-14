//! Bounded, queued access to the local generation model.
//!
//! # Why a gate
//!
//! The generation runtime is one CPU-bound process. Two requests sent at once
//! do not finish faster — they share the same cores and both finish late — and
//! before this gate nothing stopped an operator (or two UI panels) sending
//! five. Each would hold a loopback connection for up to the runtime's request
//! timeout, stacking minutes of work the operator had already given up on.
//!
//! The gate lets one request run, lets a few wait their turn for a bounded
//! time, and refuses the rest immediately with a message that says why. It
//! guards only the generation model: embedding is milliseconds and has its own
//! process, and nothing on the incident-capture or sync path touches this at
//! all — a full queue can only ever delay a *question*, never a record.

use crate::error::{CoreError, CoreResult};
use serde::Serialize;
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// Requests allowed to wait while one runs.
pub const MAX_WAITING: usize = 3;

/// Longest a request waits for its turn.
///
/// Measured generation is 2–5 s per request on the reference laptop, so three
/// queued requests clear well inside this; a request still waiting after it is
/// stuck behind something pathological and is better refused.
pub const MAX_WAIT: Duration = Duration::from_secs(60);

#[derive(Debug, Default)]
struct State {
    busy: bool,
    waiting: usize,
}

/// What the gate is doing, for status reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GateSnapshot {
    pub busy: bool,
    pub waiting: usize,
}

/// One-at-a-time access to the generation model.
#[derive(Debug)]
pub struct InferenceGate {
    state: Mutex<State>,
    freed: Condvar,
    max_waiting: usize,
    max_wait: Duration,
}

/// Held while a request uses the model. Dropping it — including by an early
/// return or a panic — admits the next waiter.
#[derive(Debug)]
pub struct Permit<'a> {
    gate: &'a InferenceGate,
}

impl Drop for Permit<'_> {
    fn drop(&mut self) {
        let mut state = self.gate.lock();
        state.busy = false;
        drop(state);
        self.gate.freed.notify_one();
    }
}

impl Default for InferenceGate {
    fn default() -> Self {
        Self::new(MAX_WAITING, MAX_WAIT)
    }
}

impl InferenceGate {
    pub fn new(max_waiting: usize, max_wait: Duration) -> Self {
        Self {
            state: Mutex::new(State::default()),
            freed: Condvar::new(),
            max_waiting,
            max_wait,
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Waits for the model, or refuses.
    ///
    /// Refuses at once when the queue is full, and after [`MAX_WAIT`]
    /// otherwise. Never blocks indefinitely.
    pub fn acquire(&self) -> CoreResult<Permit<'_>> {
        let mut state = self.lock();
        if !state.busy {
            state.busy = true;
            return Ok(Permit { gate: self });
        }

        if state.waiting >= self.max_waiting {
            return Err(CoreError::internal(format!(
                "The local model is busy and {} request(s) are already waiting. \
                 Try again shortly — incident capture and synchronisation are unaffected.",
                state.waiting
            )));
        }

        state.waiting += 1;
        let deadline = Instant::now() + self.max_wait;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                state.waiting -= 1;
                return Err(CoreError::internal(
                    "Timed out waiting for the local model. Try again shortly — \
                     incident capture and synchronisation are unaffected.",
                ));
            }
            let (next, _) = self
                .freed
                .wait_timeout(state, remaining)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state = next;
            if !state.busy {
                state.busy = true;
                state.waiting -= 1;
                return Ok(Permit { gate: self });
            }
        }
    }

    pub fn snapshot(&self) -> GateSnapshot {
        let state = self.lock();
        GateSnapshot {
            busy: state.busy,
            waiting: state.waiting,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[test]
    fn a_free_gate_admits_immediately_and_frees_on_drop() {
        let gate = InferenceGate::default();
        {
            let _permit = gate.acquire().unwrap();
            assert!(gate.snapshot().busy);
        }
        assert!(!gate.snapshot().busy);
    }

    #[test]
    fn only_one_request_runs_at_a_time() {
        let gate = Arc::new(InferenceGate::new(8, Duration::from_secs(10)));
        let running = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));

        let handles: Vec<_> = (0..6)
            .map(|_| {
                let (gate, running, peak) = (gate.clone(), running.clone(), peak.clone());
                std::thread::spawn(move || {
                    let _permit = gate.acquire().unwrap();
                    let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(now, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(20));
                    running.fetch_sub(1, Ordering::SeqCst);
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }
        assert_eq!(peak.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_full_queue_refuses_at_once_rather_than_piling_up() {
        let gate = Arc::new(InferenceGate::new(0, Duration::from_secs(10)));
        let _held = gate.acquire().unwrap();

        let started = Instant::now();
        let err = gate.acquire().unwrap_err();
        assert!(started.elapsed() < Duration::from_millis(500));
        assert!(err.message().contains("busy"));
        assert!(err.message().contains("unaffected"));
    }

    #[test]
    fn a_waiter_gives_up_after_the_bound() {
        let gate = InferenceGate::new(2, Duration::from_millis(50));
        let _held = gate.acquire().unwrap();
        let err = gate.acquire().unwrap_err();
        assert!(err.message().contains("Timed out"));
        // It left the queue on the way out.
        assert_eq!(gate.snapshot().waiting, 0);
    }

    #[test]
    fn a_panicking_holder_still_releases_the_gate() {
        let gate = Arc::new(InferenceGate::default());
        let clone = gate.clone();
        let _ = std::thread::spawn(move || {
            let _permit = clone.acquire().unwrap();
            panic!("the model call blew up");
        })
        .join();
        assert!(gate.acquire().is_ok());
    }
}
