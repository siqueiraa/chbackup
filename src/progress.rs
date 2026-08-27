//! Phase-level progress tracking for every long-running pipeline.
//!
//! # Why this is a headless counter, not a progress bar
//!
//! The predecessor of this module wrapped `indicatif` directly, which made it useless exactly
//! where it mattered: `bar` was `None` whenever stdout was not a TTY, so under Kubernetes --
//! where operators read `kubectl logs` -- it counted nothing and reported nothing. It also had
//! no getter, so the count lived in indicatif's atomics and nothing else could read it.
//!
//! So the counter is now the primary thing and TTY rendering is an optional side effect. A
//! [`PhaseProgress`] is always live: it counts, it can be snapshotted by the heartbeat thread
//! and the HTTP API, and it renders a bar *as well* when someone is watching a terminal.
//!
//! # The registry, and why phases must be finished explicitly
//!
//! Phases publish themselves into a process-global [`ProgressRegistry`] holding `Weak`
//! references, so the heartbeat and `/api/v1/status` can enumerate live work without threading
//! an `Option<&PhaseProgress>` through ~20 function signatures.
//!
//! `Weak` alone is *not* enough to keep the registry honest. Worker tasks hold strong clones,
//! and a detached or panicked task's clone can outlive the operation, which would leave a phase
//! published forever and the heartbeat reporting it as live. The invariant that prevents that:
//! **the owner -- never a worker -- calls [`PhaseProgress::finish`] or
//! [`PhaseProgress::fail`] on every exit path**, and the registry filters on that flag.
//!
//! # No reference to server state
//!
//! `PhaseProgress` must never reference `AppState`. The registry is deliberately outside the
//! documented `action_log -> running_ops` lock order, and snapshotting takes a
//! `std::sync::Mutex` for the duration of a `Vec` copy with no `.await`, so it cannot
//! participate in a deadlock with the server's locks.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};

use indicatif::{ProgressBar, ProgressStyle};
use tracing::info;

/// How often the TTY bar redraws itself, so a stalled operation looks stalled rather than
/// simply stopped. The predecessor never called `enable_steady_tick`, so even in a terminal a
/// wedged upload rendered as a frozen bar indistinguishable from a slow one.
const STEADY_TICK: Duration = Duration::from_millis(500);

/// A single in-flight item, used to report what a stalled phase is actually waiting on.
#[derive(Debug, Clone)]
struct InflightItem {
    label: String,
    started: Instant,
}

/// Shared state behind a [`PhaseProgress`]. Cloning a `PhaseProgress` shares this.
struct PhaseInner {
    id: u64,
    op: &'static str,
    phase: &'static str,
    unit: &'static str,
    backup_name: String,
    op_id: Option<u64>,

    done: AtomicU64,
    failed: AtomicU64,
    total: AtomicU64,
    bytes_done: AtomicU64,
    bytes_total: AtomicU64,

    started: Instant,
    /// Nanoseconds since `started` at the last counter advance, for `stalled_secs`.
    last_advance_nanos: AtomicU64,

    inflight: Mutex<HashMap<u64, InflightItem>>,
    next_item_id: AtomicU64,

    finished: AtomicBool,
    bar: Option<Arc<ProgressBar>>,
}

impl PhaseInner {
    fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    fn stalled(&self) -> Duration {
        let last = Duration::from_nanos(self.last_advance_nanos.load(Ordering::Relaxed));
        self.elapsed().saturating_sub(last)
    }

    fn mark_advance(&self) {
        let nanos = self.elapsed().as_nanos().min(u64::MAX as u128) as u64;
        self.last_advance_nanos.store(nanos, Ordering::Relaxed);
    }
}

/// A running phase of an operation: `upload`/`upload_parts`, `restore`/`attach_parts`, and so on.
///
/// Cheap to clone; clones share the counters, so spawned workers can hold one.
#[derive(Clone)]
pub struct PhaseProgress {
    inner: Arc<PhaseInner>,
}

/// Identity of a phase, kept separate so constructing one does not need six positional args.
#[derive(Debug, Clone, Copy)]
pub struct PhaseId {
    pub op: &'static str,
    pub phase: &'static str,
    pub unit: &'static str,
}

impl PhaseId {
    pub const fn new(op: &'static str, phase: &'static str, unit: &'static str) -> Self {
        Self { op, phase, unit }
    }
}

impl PhaseProgress {
    /// Start a phase and publish it to the global registry.
    ///
    /// Logs the stable message `"Phase started"`. `total` may be 0 when it is not yet known --
    /// `create`'s shadow walk discovers its total per table -- in which case call
    /// [`PhaseProgress::add_total`] as it becomes known.
    pub fn start(id: PhaseId, backup_name: impl Into<String>, total: u64) -> Self {
        Self::start_with(id, backup_name, total, None, false)
    }

    /// As [`PhaseProgress::start`], but records the API operation id for correlation with
    /// `/api/v1/status` and `/kill?id=N`, and can suppress the TTY bar.
    pub fn start_with(
        id: PhaseId,
        backup_name: impl Into<String>,
        total: u64,
        op_id: Option<u64>,
        disable_bar: bool,
    ) -> Self {
        let backup_name = backup_name.into();
        let bar = build_bar(id.op, id.unit, total, disable_bar);

        let inner = Arc::new(PhaseInner {
            id: next_phase_id(),
            op: id.op,
            phase: id.phase,
            unit: id.unit,
            backup_name: backup_name.clone(),
            op_id,
            done: AtomicU64::new(0),
            failed: AtomicU64::new(0),
            total: AtomicU64::new(total),
            bytes_done: AtomicU64::new(0),
            bytes_total: AtomicU64::new(0),
            started: Instant::now(),
            last_advance_nanos: AtomicU64::new(0),
            inflight: Mutex::new(HashMap::new()),
            next_item_id: AtomicU64::new(0),
            finished: AtomicBool::new(false),
            bar,
        });

        let me = Self { inner };
        registry().register(&me);

        info!(
            op = me.inner.op,
            op_id = me.inner.op_id,
            backup_name = %me.inner.backup_name,
            phase = me.inner.phase,
            unit = me.inner.unit,
            total = total,
            "Phase started"
        );

        me
    }

    /// Raise the total once more work is discovered. Used where the total is not known up front.
    pub fn add_total(&self, n: u64) {
        let new = self.inner.total.fetch_add(n, Ordering::Relaxed) + n;
        if let Some(ref bar) = self.inner.bar {
            bar.set_length(new);
        }
    }

    /// Record the byte total, when the phase knows it.
    pub fn set_bytes_total(&self, n: u64) {
        self.inner.bytes_total.store(n, Ordering::Relaxed);
    }

    /// Count a completed item. Prefer [`PhaseProgress::start_item`] where the item can fail,
    /// so failures are not silently counted as done.
    pub fn inc(&self) {
        self.inner.done.fetch_add(1, Ordering::Relaxed);
        self.inner.mark_advance();
        if let Some(ref bar) = self.inner.bar {
            bar.inc(1);
        }
    }

    /// Add transferred bytes. Called from the copy loops, which is what turns a long silence
    /// into a byte rate that visibly moves -- or visibly does not.
    pub fn add_bytes(&self, n: u64) {
        self.inner.bytes_done.fetch_add(n, Ordering::Relaxed);
        self.inner.mark_advance();
    }

    /// Begin an item, returning a guard that counts it done only if explicitly succeeded.
    pub fn start_item(&self, label: impl Into<String>) -> ItemGuard {
        let id = self.inner.next_item_id.fetch_add(1, Ordering::Relaxed);
        let item = InflightItem {
            label: label.into(),
            started: Instant::now(),
        };
        lock_inflight(&self.inner).insert(id, item);
        ItemGuard {
            phase: self.clone(),
            id,
            settled: false,
        }
    }

    /// Mark the phase complete. Only the owner should call this. Idempotent.
    pub fn finish(&self) {
        self.end("Phase complete");
    }

    /// Mark the phase failed. Only the owner should call this. Idempotent.
    pub fn fail(&self) {
        self.end("Phase failed");
    }

    fn end(&self, message: &'static str) {
        // swap, not store: the log line must be emitted exactly once even if two exit paths
        // both try to finish the phase.
        if self.inner.finished.swap(true, Ordering::SeqCst) {
            return;
        }
        if let Some(ref bar) = self.inner.bar {
            bar.finish_and_clear();
        }
        let s = self.snapshot();
        info!(
            op = %s.op,
            op_id = s.op_id,
            backup_name = %s.backup_name,
            phase = %s.phase,
            unit = %s.unit,
            done = s.done,
            failed = s.failed,
            total = s.total,
            bytes_done = s.bytes_done,
            elapsed_secs = s.elapsed_secs,
            rate_items_per_sec = s.rate_items_per_sec,
            rate_bytes_per_sec = s.rate_bytes_per_sec,
            message
        );
    }

    /// True once [`PhaseProgress::finish`] or [`PhaseProgress::fail`] has run.
    pub fn is_finished(&self) -> bool {
        self.inner.finished.load(Ordering::SeqCst)
    }

    /// Whether a TTY bar is being rendered for this phase. The heartbeat skips such phases,
    /// because a bar and a log line interleave into garbage.
    pub fn has_bar(&self) -> bool {
        self.inner.bar.is_some()
    }

    /// Point-in-time copy of every counter.
    pub fn snapshot(&self) -> PhaseSnapshot {
        snapshot_inner(&self.inner)
    }
}

/// Owner-side handle that guarantees a phase ends, even on an early return.
///
/// # Why this exists
///
/// The rule is that the owner must end the phase on *every* exit path, because worker tasks
/// hold strong clones: an unfinished phase stays in the registry and the heartbeat reports it
/// as live for the rest of the process's life. Relying on hand-written calls made that a
/// standing trap -- any `?` between start and finish leaks, and the integration suite caught
/// exactly that in `create` (3 phases started, 2 ended).
///
/// So ending is now structural. `Drop` fails the phase; an explicit [`PhaseOwner::finish`]
/// marks it complete first. [`PhaseProgress::end`] swaps a flag, so whichever runs first
/// wins and the second is a no-op -- which is what makes 'finish then drop' safe.
///
/// Deliberately **not** `Clone`: a second owner would end the phase when the first was
/// dropped. Workers get a counting handle from [`PhaseOwner::handle`] instead.
pub struct PhaseOwner {
    phase: PhaseProgress,
}

impl PhaseOwner {
    /// Start a phase and take ownership of ending it.
    pub fn start(
        id: PhaseId,
        backup_name: impl Into<String>,
        total: u64,
        op_id: Option<u64>,
        disable_bar: bool,
    ) -> Self {
        Self {
            phase: PhaseProgress::start_with(id, backup_name, total, op_id, disable_bar),
        }
    }

    /// A counting handle for a worker. Advancing it advances the same counters.
    pub fn handle(&self) -> PhaseProgress {
        self.phase.clone()
    }

    /// Mark the phase complete. Without this, `Drop` records it as failed.
    pub fn finish(&self) {
        self.phase.finish();
    }
}

impl std::ops::Deref for PhaseOwner {
    type Target = PhaseProgress;
    fn deref(&self) -> &PhaseProgress {
        &self.phase
    }
}

impl Drop for PhaseOwner {
    fn drop(&mut self) {
        // No-op when finish() already ran. Otherwise this is an early return, a `?`, a
        // panic unwind or a dropped future -- all of which are failures, and all of which
        // must still take the phase out of the registry.
        self.phase.fail();
    }
}

/// Counts an item done only on explicit success.
///
/// A bare drop counts it **failed**, never done. Conflating the two would make the counters lie
/// in exactly the failure cases this module exists to illuminate: a phase that errored on half
/// its parts would report `done` equal to `total` and look like a clean run.
pub struct ItemGuard {
    phase: PhaseProgress,
    id: u64,
    settled: bool,
}

impl ItemGuard {
    /// Count the item as done.
    pub fn succeed(mut self) {
        self.settled = true;
        lock_inflight(&self.phase.inner).remove(&self.id);
        self.phase.inc();
    }

    /// How long this item has been running.
    pub fn elapsed(&self) -> Duration {
        lock_inflight(&self.phase.inner)
            .get(&self.id)
            .map(|i| i.started.elapsed())
            .unwrap_or_default()
    }
}

impl Drop for ItemGuard {
    fn drop(&mut self) {
        if self.settled {
            return;
        }
        lock_inflight(&self.phase.inner).remove(&self.id);
        self.phase.inner.failed.fetch_add(1, Ordering::Relaxed);
        self.phase.inner.mark_advance();
    }
}

/// Serializable view of a phase, used by the heartbeat, the metrics refresh and the HTTP API.
///
/// Field names here are the canonical log/JSON field names. A serde test pins every one of
/// them, because these are what dashboards and alerts match on.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PhaseSnapshot {
    pub id: u64,
    pub op: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub op_id: Option<u64>,
    pub backup_name: String,
    pub phase: String,
    pub unit: String,
    pub done: u64,
    pub failed: u64,
    pub total: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub percent: Option<f64>,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub elapsed_secs: f64,
    pub rate_items_per_sec: f64,
    pub rate_bytes_per_sec: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub eta_secs: Option<f64>,
    pub stalled_secs: f64,
    pub inflight: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slowest_item: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slowest_item_secs: Option<f64>,
    pub finished: bool,
}

impl PhaseSnapshot {
    /// One-line human rendering for the heartbeat's terminal readers.
    pub fn human_summary(&self) -> String {
        let mut s = format!("{}/{}", self.op, self.phase);
        if self.total > 0 {
            s.push_str(&format!(" {}/{} {}", self.done, self.total, self.unit));
        } else {
            s.push_str(&format!(" {} {}", self.done, self.unit));
        }
        if let Some(p) = self.percent {
            s.push_str(&format!(" ({:.0}%)", p));
        }
        if self.bytes_done > 0 {
            s.push_str(&format!(
                " {} at {}/s",
                human_bytes(self.bytes_done),
                human_bytes(self.rate_bytes_per_sec as u64)
            ));
        }
        s.push_str(&format!(" in {}", human_duration(self.elapsed_secs)));
        if let Some(eta) = self.eta_secs {
            s.push_str(&format!(", ETA {}", human_duration(eta)));
        }
        if self.failed > 0 {
            s.push_str(&format!(", {} failed", self.failed));
        }
        s
    }
}

fn snapshot_inner(inner: &PhaseInner) -> PhaseSnapshot {
    let done = inner.done.load(Ordering::Relaxed);
    let total = inner.total.load(Ordering::Relaxed);
    let bytes_done = inner.bytes_done.load(Ordering::Relaxed);
    let elapsed = inner.elapsed();

    let (slowest_item, slowest_item_secs, inflight_count) = {
        let guard = lock_inflight(inner);
        let items: Vec<(String, Duration)> = guard
            .values()
            .map(|i| (i.label.clone(), i.started.elapsed()))
            .collect();
        let count = items.len() as u64;
        match slowest(&items) {
            Some((label, d)) => (Some(label.to_string()), Some(d.as_secs_f64()), count),
            None => (None, None, count),
        }
    };

    PhaseSnapshot {
        id: inner.id,
        op: inner.op.to_string(),
        op_id: inner.op_id,
        backup_name: inner.backup_name.clone(),
        phase: inner.phase.to_string(),
        unit: inner.unit.to_string(),
        done,
        failed: inner.failed.load(Ordering::Relaxed),
        total,
        percent: percent(done, total),
        bytes_done,
        bytes_total: inner.bytes_total.load(Ordering::Relaxed),
        elapsed_secs: elapsed.as_secs_f64(),
        rate_items_per_sec: rate_per_sec(done, elapsed),
        rate_bytes_per_sec: rate_per_sec(bytes_done, elapsed),
        eta_secs: eta_secs(done, total, elapsed),
        stalled_secs: stalled_secs(done, total, inner.stalled()),
        inflight: inflight_count,
        slowest_item,
        slowest_item_secs,
        finished: inner.finished.load(Ordering::SeqCst),
    }
}

/// Poison-tolerant lock over the in-flight map.
///
/// A worker panicking mid-item poisons this mutex, and that must not cascade into every later
/// snapshot: the heartbeat losing its ability to report is the worst possible response to a
/// panic. Matches the existing convention in `upload/mod.rs`.
fn lock_inflight(inner: &PhaseInner) -> std::sync::MutexGuard<'_, HashMap<u64, InflightItem>> {
    inner.inflight.lock().unwrap_or_else(|e| e.into_inner())
}

fn build_bar(
    op: &'static str,
    unit: &'static str,
    total: u64,
    disable: bool,
) -> Option<Arc<ProgressBar>> {
    if disable || total == 0 || !std::io::IsTerminal::is_terminal(&std::io::stdout()) {
        return None;
    }
    let bar = ProgressBar::new(total);
    let template = format!(
        "{op} {{bar:40.cyan/blue}} {{percent}}% {{pos}}/{{len}} {unit} {{per_sec}} ETA {{eta}}"
    );
    if let Ok(style) = ProgressStyle::with_template(&template) {
        bar.set_style(style.progress_chars("##-"));
    }
    // Without a steady tick a stalled bar is indistinguishable from a slow one.
    bar.enable_steady_tick(STEADY_TICK);
    Some(Arc::new(bar))
}

fn next_phase_id() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

/// Holds `Weak` handles to live phases so the heartbeat and HTTP API can enumerate them.
#[derive(Default)]
pub struct ProgressRegistry {
    phases: Mutex<Vec<Weak<PhaseInner>>>,
}

impl ProgressRegistry {
    /// A registry independent of the global one, so tests do not share process state.
    pub fn new() -> Self {
        Self::default()
    }

    fn register(&self, phase: &PhaseProgress) {
        let mut guard = self.phases.lock().unwrap_or_else(|e| e.into_inner());
        // Prune dead entries opportunistically; there is no separate reaper.
        guard.retain(|w| w.strong_count() > 0);
        guard.push(Arc::downgrade(&phase.inner));
    }

    /// Snapshots of every phase that is still alive and not finished.
    ///
    /// Finished phases are excluded rather than removed, because a worker's strong clone can
    /// keep the `Arc` alive past the owner's `finish()`.
    pub fn snapshots(&self) -> Vec<PhaseSnapshot> {
        let mut guard = self.phases.lock().unwrap_or_else(|e| e.into_inner());
        guard.retain(|w| w.strong_count() > 0);
        let mut out: Vec<PhaseSnapshot> = guard
            .iter()
            .filter_map(|w| w.upgrade())
            .filter(|inner| !inner.finished.load(Ordering::SeqCst))
            .map(|inner| snapshot_inner(&inner))
            .collect();
        out.sort_by_key(|s| s.id);
        out
    }

    /// How many phases are currently tracked, finished or not. Test helper.
    pub fn len(&self) -> usize {
        let mut guard = self.phases.lock().unwrap_or_else(|e| e.into_inner());
        guard.retain(|w| w.strong_count() > 0);
        guard.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// The process-global registry.
pub fn registry() -> &'static ProgressRegistry {
    static REGISTRY: OnceLock<ProgressRegistry> = OnceLock::new();
    REGISTRY.get_or_init(ProgressRegistry::new)
}

// ---------------------------------------------------------------------------
// Pure helpers
//
// These are free functions over primitives on purpose: there is no `tracing-test` dependency,
// so the log lines themselves cannot be asserted. Keeping every derived value in a pure
// function makes the arithmetic -- which is where the NaN and divide-by-zero bugs live --
// directly testable.
// ---------------------------------------------------------------------------

/// Throughput in units per second, or `0.0` when no time has elapsed.
///
/// Guards against a zero elapsed time, which would otherwise yield `inf` or `NaN` in a log
/// field and break a JSON consumer.
pub fn rate_per_sec(count: u64, elapsed: Duration) -> f64 {
    let secs = elapsed.as_secs_f64();
    if secs <= 0.0 || count == 0 {
        return 0.0;
    }
    count as f64 / secs
}

/// Completion percentage, or `None` when there is no meaningful total.
///
/// Clamped to 100: `done` can exceed `total` when a total was estimated and then revised.
pub fn percent(done: u64, total: u64) -> Option<f64> {
    if total == 0 {
        return None;
    }
    Some(((done as f64 / total as f64) * 100.0).min(100.0))
}

/// Estimated seconds remaining, or `None` when no honest estimate exists.
///
/// `None` when nothing is done yet, when the total is unknown, or when `done` already exceeds
/// `total`. A garbage ETA is worse than no ETA: `create`'s shadow walk has an unknown total,
/// and an ETA computed from a total of zero would read as "finishing now" forever.
pub fn eta_secs(done: u64, total: u64, elapsed: Duration) -> Option<f64> {
    if done == 0 || total == 0 || total < done {
        return None;
    }
    let rate = rate_per_sec(done, elapsed);
    if rate <= 0.0 {
        return None;
    }
    Some((total - done) as f64 / rate)
}

/// Seconds since the phase last advanced -- but **zero once its work is complete**.
///
/// A phase that has reached its total is finished working even if its owner has not closed
/// it yet, and several do stay open: restore ends `restore_s3_objects` and `attach_parts`
/// together, so the object phase can sit at 100% while parts are still attaching. Reporting
/// a climbing `stalled_secs` there would cross the documented
/// `chbackup_phase_stalled_seconds > 600` alert on a healthy long restore -- a false page,
/// and worse than no alert because it teaches operators to ignore it.
///
/// A phase with an unknown total (0) cannot make this judgement, so it reports the raw value.
pub fn stalled_secs(done: u64, total: u64, since_advance: Duration) -> f64 {
    if total > 0 && done >= total {
        return 0.0;
    }
    since_advance.as_secs_f64()
}

/// The longest-running item, used to name what a stalled phase is waiting on.
pub fn slowest(items: &[(String, Duration)]) -> Option<(&str, Duration)> {
    items
        .iter()
        .max_by_key(|(_, d)| *d)
        .map(|(label, d)| (label.as_str(), *d))
}

/// Byte count in binary units, for the human-readable half of the logs.
pub fn human_bytes(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    let b = bytes as f64;
    if b < KIB {
        return format!("{bytes} B");
    }
    const UNITS: [&str; 5] = ["KiB", "MiB", "GiB", "TiB", "PiB"];
    let mut value = b / KIB;
    let mut unit = 0;
    while value >= KIB && unit + 1 < UNITS.len() {
        value /= KIB;
        unit += 1;
    }
    format!("{:.1} {}", value, UNITS[unit])
}

/// Duration as a compact `1h2m3s` style string.
pub fn human_duration(secs: f64) -> String {
    if !secs.is_finite() || secs < 0.0 {
        return "0s".to_string();
    }
    let total = secs as u64;
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    if h > 0 {
        format!("{h}h{m}m{s}s")
    } else if m > 0 {
        format!("{m}m{s}s")
    } else if total > 0 {
        format!("{s}s")
    } else {
        format!("{secs:.1}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_id() -> PhaseId {
        PhaseId::new("upload", "upload_parts", "parts")
    }

    // -- rate_per_sec ------------------------------------------------------

    #[test]
    fn rate_per_sec_zero_elapsed_is_not_infinite() {
        let r = rate_per_sec(1024, Duration::ZERO);
        assert_eq!(r, 0.0);
        assert!(r.is_finite());
    }

    #[test]
    fn rate_per_sec_zero_count() {
        assert_eq!(rate_per_sec(0, Duration::from_secs(5)), 0.0);
    }

    #[test]
    fn rate_per_sec_known_values() {
        assert_eq!(rate_per_sec(1000, Duration::from_secs(2)), 500.0);
        assert_eq!(rate_per_sec(1_048_576, Duration::from_secs(1)), 1_048_576.0);
    }

    #[test]
    fn rate_per_sec_halving_duration_doubles_rate() {
        let fast = rate_per_sec(4096, Duration::from_secs(1));
        let slow = rate_per_sec(4096, Duration::from_secs(2));
        assert!((fast - slow * 2.0).abs() < f64::EPSILON);
    }

    // -- percent -----------------------------------------------------------

    #[test]
    fn percent_zero_total_is_none() {
        assert!(percent(0, 0).is_none());
        assert!(percent(5, 0).is_none());
    }

    #[test]
    fn percent_is_clamped_when_done_exceeds_total() {
        // A revised total must not produce 250%.
        assert_eq!(percent(5, 2), Some(100.0));
    }

    #[test]
    fn percent_known_values() {
        assert_eq!(percent(0, 10), Some(0.0));
        assert_eq!(percent(5, 10), Some(50.0));
        assert_eq!(percent(10, 10), Some(100.0));
    }

    // -- eta_secs ----------------------------------------------------------

    #[test]
    fn eta_is_none_without_an_honest_estimate() {
        let e = Duration::from_secs(10);
        assert!(eta_secs(0, 10, e).is_none(), "nothing done yet");
        assert!(eta_secs(5, 0, e).is_none(), "unknown total");
        assert!(eta_secs(11, 10, e).is_none(), "done exceeds total");
        assert!(eta_secs(5, 10, Duration::ZERO).is_none(), "no elapsed time");
    }

    #[test]
    fn eta_extrapolates_from_observed_rate() {
        // 5 of 10 in 10s -> 0.5/s -> 5 remaining -> 10s.
        let eta = eta_secs(5, 10, Duration::from_secs(10)).unwrap();
        assert!((eta - 10.0).abs() < 1e-9, "got {eta}");
    }

    // -- stalled_secs ------------------------------------------------------

    #[test]
    fn a_phase_at_its_total_is_not_reported_as_stalled() {
        // restore closes attach_parts and restore_s3_objects together, so the object phase
        // can sit at 100% for minutes. Reporting that as a stall would false-page the
        // documented stalled_seconds > 600 alert on a perfectly healthy restore.
        assert_eq!(stalled_secs(110, 110, Duration::from_secs(900)), 0.0);
        assert_eq!(stalled_secs(111, 110, Duration::from_secs(900)), 0.0);
    }

    #[test]
    fn an_incomplete_phase_reports_its_real_stall_time() {
        // This is the incident signature and must survive the exemption above.
        assert_eq!(stalled_secs(27, 30, Duration::from_secs(1420)), 1420.0);
    }

    #[test]
    fn an_unknown_total_cannot_claim_completion_so_reports_raw_stall() {
        assert_eq!(stalled_secs(0, 0, Duration::from_secs(60)), 60.0);
        assert_eq!(stalled_secs(5, 0, Duration::from_secs(60)), 60.0);
    }

    // -- slowest -----------------------------------------------------------

    #[test]
    fn slowest_picks_the_longest_running_item() {
        let items = vec![
            ("a".to_string(), Duration::from_secs(3)),
            ("b".to_string(), Duration::from_secs(30)),
            ("c".to_string(), Duration::from_secs(1)),
        ];
        let (label, d) = slowest(&items).unwrap();
        assert_eq!(label, "b");
        assert_eq!(d, Duration::from_secs(30));
    }

    #[test]
    fn slowest_of_nothing_is_none() {
        assert!(slowest(&[]).is_none());
    }

    // -- formatting --------------------------------------------------------

    #[test]
    fn human_bytes_boundaries() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(1023), "1023 B");
        assert_eq!(human_bytes(1024), "1.0 KiB");
        assert_eq!(human_bytes(1024 * 1024), "1.0 MiB");
        assert_eq!(human_bytes(6_487_078_358), "6.0 GiB");
    }

    #[test]
    fn human_duration_boundaries() {
        assert_eq!(human_duration(0.0), "0.0s");
        assert_eq!(human_duration(0.4), "0.4s");
        assert_eq!(human_duration(45.0), "45s");
        assert_eq!(human_duration(90.0), "1m30s");
        assert_eq!(human_duration(3725.0), "1h2m5s");
    }

    #[test]
    fn human_duration_rejects_nonsense_rather_than_printing_nan() {
        assert_eq!(human_duration(f64::NAN), "0s");
        assert_eq!(human_duration(-5.0), "0s");
    }

    // -- ItemGuard ---------------------------------------------------------

    #[test]
    fn item_guard_succeed_counts_done_once() {
        let p = PhaseProgress::start_with(test_id(), "b", 3, None, true);
        p.start_item("part-1").succeed();

        let s = p.snapshot();
        assert_eq!(s.done, 1);
        assert_eq!(s.failed, 0);
        assert_eq!(s.inflight, 0, "a settled item must leave the in-flight map");
        p.finish();
    }

    #[test]
    fn item_guard_bare_drop_counts_failed_not_done() {
        // The whole point: a failed part must not inflate `done`, or a half-broken phase
        // reports as a clean run.
        let p = PhaseProgress::start_with(test_id(), "b", 3, None, true);
        {
            let _g = p.start_item("part-1");
            // dropped without succeed()
        }

        let s = p.snapshot();
        assert_eq!(s.done, 0, "a dropped guard must not count as done");
        assert_eq!(s.failed, 1);
        assert_eq!(s.inflight, 0);
        p.finish();
    }

    #[test]
    fn inflight_items_are_visible_and_named_while_running() {
        let p = PhaseProgress::start_with(test_id(), "b", 2, None, true);
        let g = p.start_item("part-slow");

        let s = p.snapshot();
        assert_eq!(s.inflight, 1);
        assert_eq!(s.slowest_item.as_deref(), Some("part-slow"));
        assert!(s.slowest_item_secs.is_some());

        g.succeed();
        p.finish();
    }

    // -- counters ----------------------------------------------------------

    #[test]
    fn add_total_raises_the_denominator_for_unknown_total_phases() {
        let p = PhaseProgress::start_with(
            PhaseId::new("create", "collect_parts", "parts"),
            "b",
            0,
            None,
            true,
        );
        assert!(p.snapshot().percent.is_none(), "no total yet -> no percent");

        p.add_total(4);
        p.inc();
        let s = p.snapshot();
        assert_eq!(s.total, 4);
        assert_eq!(s.percent, Some(25.0));
        p.finish();
    }

    #[test]
    fn add_bytes_accumulates_and_reports_a_rate() {
        let p = PhaseProgress::start_with(test_id(), "b", 1, None, true);
        p.add_bytes(1024);
        p.add_bytes(1024);
        let s = p.snapshot();
        assert_eq!(s.bytes_done, 2048);
        assert!(s.rate_bytes_per_sec.is_finite());
        p.finish();
    }

    // -- registry ----------------------------------------------------------

    #[test]
    fn registry_excludes_finished_phases() {
        let reg = ProgressRegistry::new();
        let p = PhaseProgress::start_with(test_id(), "reg-test", 1, None, true);
        reg.register(&p);

        assert_eq!(reg.snapshots().len(), 1, "a live phase is reported");

        p.finish();
        assert!(
            reg.snapshots().is_empty(),
            "a finished phase must not be reported as live, even though a clone still holds \
             the Arc -- otherwise a detached worker keeps it published forever"
        );
        // Still tracked (the Arc is alive), just filtered out of snapshots.
        assert_eq!(reg.len(), 1);
    }

    #[test]
    fn registry_prunes_dropped_phases() {
        let reg = ProgressRegistry::new();
        {
            let p = PhaseProgress::start_with(test_id(), "gone", 1, None, true);
            reg.register(&p);
            assert_eq!(reg.len(), 1);
        }
        assert_eq!(reg.len(), 0, "dropping every clone must prune the Weak");
        assert!(reg.snapshots().is_empty());
    }

    #[test]
    fn registry_snapshots_are_ordered_by_phase_id() {
        let reg = ProgressRegistry::new();
        let a = PhaseProgress::start_with(test_id(), "a", 1, None, true);
        let b = PhaseProgress::start_with(test_id(), "b", 1, None, true);
        reg.register(&b);
        reg.register(&a);

        let ids: Vec<u64> = reg.snapshots().iter().map(|s| s.id).collect();
        let mut sorted = ids.clone();
        sorted.sort();
        assert_eq!(ids, sorted, "deterministic order, not registration order");
        a.finish();
        b.finish();
    }

    // -- snapshot shape ----------------------------------------------------

    /// Anti-drift guard: dashboards and alerts match on these exact names.
    #[test]
    fn snapshot_serializes_every_canonical_field_name() {
        let p = PhaseProgress::start_with(test_id(), "backup-1", 4, Some(7), true);
        p.add_bytes(2048);
        p.set_bytes_total(4096);
        let g = p.start_item("part-x");
        p.inc();

        let v = serde_json::to_value(p.snapshot()).unwrap();
        for field in [
            "id",
            "op",
            "op_id",
            "backup_name",
            "phase",
            "unit",
            "done",
            "failed",
            "total",
            "percent",
            "bytes_done",
            "bytes_total",
            "elapsed_secs",
            "rate_items_per_sec",
            "rate_bytes_per_sec",
            "eta_secs",
            "stalled_secs",
            "inflight",
            "slowest_item",
            "slowest_item_secs",
            "finished",
        ] {
            assert!(
                v.get(field).is_some(),
                "canonical field `{field}` missing from PhaseSnapshot JSON"
            );
        }
        assert_eq!(v.get("op").unwrap(), "upload");
        assert_eq!(v.get("phase").unwrap(), "upload_parts");
        assert_eq!(v.get("op_id").unwrap(), 7);

        g.succeed();
        p.finish();
    }

    #[test]
    fn optional_fields_are_omitted_rather_than_null() {
        // op_id/eta/slowest are absent for a CLI phase with no progress yet, and a consumer
        // should not have to distinguish null from 0.
        let p = PhaseProgress::start_with(test_id(), "b", 0, None, true);
        let v = serde_json::to_value(p.snapshot()).unwrap();
        assert!(v.get("op_id").is_none());
        assert!(v.get("percent").is_none());
        assert!(v.get("eta_secs").is_none());
        assert!(v.get("slowest_item").is_none());
        p.finish();
    }

    #[test]
    fn human_summary_is_readable_for_a_running_phase() {
        let p = PhaseProgress::start_with(test_id(), "b", 10, None, true);
        p.inc();
        p.add_bytes(1024 * 1024);
        let s = p.snapshot().human_summary();
        assert!(s.contains("upload/upload_parts"), "got {s}");
        assert!(s.contains("1/10 parts"), "got {s}");
        p.finish();
    }

    #[test]
    fn phase_owner_drop_ends_the_phase_on_an_early_return() {
        // The regression T77 caught: a phase started and never ended stays in the registry
        // and the heartbeat reports it live forever.
        let reg = ProgressRegistry::new();
        {
            let owner = PhaseOwner::start(test_id(), "early-return", 5, None, true);
            reg.register(&owner);
            assert_eq!(reg.snapshots().len(), 1, "live while owned");
            // Simulate `?` returning here: no finish() call.
        }
        assert!(
            reg.snapshots().is_empty(),
            "dropping the owner without finish() must still end the phase"
        );
    }

    #[test]
    fn phase_owner_finish_then_drop_reports_complete_not_failed() {
        let owner = PhaseOwner::start(test_id(), "ok", 1, None, true);
        owner.inc();
        owner.finish();
        let s = owner.snapshot();
        assert!(s.finished);
        assert_eq!(s.done, 1);
        assert_eq!(
            s.failed, 0,
            "a finished phase must not also be counted failed"
        );
        // Drop runs here and must be a no-op.
    }

    #[test]
    fn phase_owner_handle_shares_counters_with_the_owner() {
        let owner = PhaseOwner::start(test_id(), "shared", 2, None, true);
        let worker = owner.handle();
        worker.inc();
        assert_eq!(owner.snapshot().done, 1);
        owner.finish();
    }

    /// A worker outliving the owner must not resurrect the phase: the registry filters on
    /// the finished flag precisely because the worker's strong Arc keeps it allocated.
    #[test]
    fn a_worker_handle_cannot_keep_a_phase_live_after_the_owner_ends_it() {
        let reg = ProgressRegistry::new();
        let worker;
        {
            let owner = PhaseOwner::start(test_id(), "detached", 3, None, true);
            reg.register(&owner);
            worker = owner.handle();
        } // owner dropped -> phase ended

        assert!(
            reg.snapshots().is_empty(),
            "a surviving worker handle must not keep the phase published"
        );
        // The handle is still usable and must not panic.
        worker.inc();
    }

    #[test]
    fn finish_is_idempotent() {
        let p = PhaseProgress::start_with(test_id(), "b", 1, None, true);
        p.finish();
        p.finish();
        p.fail();
        assert!(p.is_finished());
    }

    #[test]
    fn disabled_bar_when_requested() {
        let p = PhaseProgress::start_with(test_id(), "b", 10, None, true);
        assert!(!p.has_bar(), "explicitly disabled");
        p.finish();
    }

    #[test]
    fn clone_shares_counters_so_spawned_workers_advance_the_same_phase() {
        let p = PhaseProgress::start_with(test_id(), "b", 2, None, true);
        let worker = p.clone();
        worker.inc();
        assert_eq!(p.snapshot().done, 1, "a clone must not get its own counter");
        p.finish();
    }
}
