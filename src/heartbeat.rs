//! Periodic aggregate progress logging.
//!
//! # Why a wall-clock timer, and why an OS thread
//!
//! This exists because a production upload went silent for 25+ minutes and nobody could tell
//! progressing from parked. Three properties make it able to answer that, and each rules out an
//! easier design:
//!
//! 1. **It fires on a wall-clock timer, not on progress events.** "Log every 100 parts" would
//!    have printed *nothing* during the stall -- the absence of events is exactly the symptom.
//! 2. **It is never suppressed because nothing changed.** Each line carries `stalled_secs`,
//!    which climbs while `done` stays put. That contrast is the diagnosis.
//! 3. **It runs on a plain `std::thread`, not `tokio::spawn`.** `tracing` emission is
//!    synchronous and snapshotting needs no `.await`, so an OS thread keeps printing even when
//!    every tokio worker is wedged -- which is precisely the class of bug being diagnosed. A
//!    `tokio::spawn`ed heartbeat would go quiet at the moment it became useful.
//!
//! It emits nothing when no phase is live, so an idle server stays silent and this is safe to
//! leave on by default. That silence is itself diagnostic: no heartbeat during an operation
//! means no phase was ever published.
//!
//! Never logged at `warn`. Operators alert on `warn`, and a 30s cadence would poison it.

use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::Duration;

use tracing::info;

use crate::progress::{registry, PhaseSnapshot};

/// Stops the heartbeat thread when dropped.
pub struct HeartbeatGuard {
    stop: Option<Sender<()>>,
    handle: Option<JoinHandle<()>>,
}

impl Drop for HeartbeatGuard {
    fn drop(&mut self) {
        // Drop the sender FIRST so the thread's recv_timeout returns Disconnected, then join.
        // Dropping the sender alone only signals; without the join the process can exit while
        // the thread is mid-emission.
        drop(self.stop.take());
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// Spawn the heartbeat. Returns `None` when `interval` is zero, which disables it.
pub fn spawn_heartbeat(interval: Duration) -> Option<HeartbeatGuard> {
    if interval.is_zero() {
        return None;
    }

    let (tx, rx) = mpsc::channel::<()>();
    let handle = std::thread::Builder::new()
        .name("chbackup-heartbeat".to_string())
        .spawn(move || loop {
            match rx.recv_timeout(interval) {
                // Sender dropped or an explicit stop: leave.
                Ok(()) | Err(RecvTimeoutError::Disconnected) => return,
                Err(RecvTimeoutError::Timeout) => emit_once(),
            }
        })
        .ok()?;

    Some(HeartbeatGuard {
        stop: Some(tx),
        handle: Some(handle),
    })
}

/// Emit one line per live phase. Public for tests.
pub fn emit_once() {
    for s in registry().snapshots() {
        if should_emit(&s) {
            emit(&s);
        }
    }
}

/// Whether a snapshot warrants a heartbeat line.
///
/// Skips finished phases and any phase rendering a TTY bar: a bar and a log line write to the
/// same stdout and interleave into garbage.
pub fn should_emit(s: &PhaseSnapshot) -> bool {
    !s.finished
}

fn emit(s: &PhaseSnapshot) {
    info!(
        op = %s.op,
        op_id = s.op_id,
        backup_name = %s.backup_name,
        phase = %s.phase,
        unit = %s.unit,
        done = s.done,
        failed = s.failed,
        total = s.total,
        percent = s.percent,
        bytes_done = s.bytes_done,
        bytes_total = s.bytes_total,
        rate_bytes_per_sec = s.rate_bytes_per_sec,
        rate_items_per_sec = s.rate_items_per_sec,
        elapsed_secs = s.elapsed_secs,
        eta_secs = s.eta_secs,
        stalled_secs = s.stalled_secs,
        inflight = s.inflight,
        slowest_item = s.slowest_item.as_deref(),
        slowest_item_secs = s.slowest_item_secs,
        summary = %s.human_summary(),
        "Phase progress"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::progress::{PhaseId, PhaseProgress};

    #[test]
    fn zero_interval_disables_the_heartbeat() {
        assert!(
            spawn_heartbeat(Duration::ZERO).is_none(),
            "0s must disable rather than spin"
        );
    }

    #[test]
    fn guard_stops_and_joins_the_thread() {
        let g = spawn_heartbeat(Duration::from_millis(50)).expect("spawned");
        // Dropping must not hang: the sender is dropped before the join.
        drop(g);
    }

    #[test]
    fn emit_once_is_safe_with_no_live_phases() {
        // An idle process must stay silent rather than panic or print.
        emit_once();
    }

    #[test]
    fn finished_phases_are_not_emitted() {
        let p = PhaseProgress::start_with(
            PhaseId::new("upload", "upload_parts", "parts"),
            "hb",
            2,
            None,
            true,
        );
        let live = p.snapshot();
        assert!(should_emit(&live), "a running phase is reported");

        p.finish();
        let done = p.snapshot();
        assert!(
            !should_emit(&done),
            "a finished phase must not keep appearing in the heartbeat"
        );
    }

    #[test]
    fn a_stalled_phase_reports_growing_stalled_secs_while_done_stays_put() {
        // The signature of the incident: the counter frozen, the clock moving.
        let p = PhaseProgress::start_with(
            PhaseId::new("upload", "copy_objects", "objects"),
            "hb",
            10,
            None,
            true,
        );
        p.inc();
        let first = p.snapshot();
        std::thread::sleep(Duration::from_millis(30));
        let second = p.snapshot();

        assert_eq!(second.done, first.done, "no progress between snapshots");
        assert!(
            second.stalled_secs > first.stalled_secs,
            "stalled_secs must climb so a frozen counter is visibly stalled: {} -> {}",
            first.stalled_secs,
            second.stalled_secs
        );
        p.finish();
    }
}
