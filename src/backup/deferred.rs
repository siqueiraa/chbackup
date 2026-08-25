//! Deferred-freeze record: ownership tracking for freezes held across create -> upload.
//!
//! # Why this exists
//!
//! For local disks, `backup::create` pins data by hardlinking shadow files into the backup
//! directory, so a merge cannot destroy it. For **S3 object disks** it only records object
//! *pointers*; the shadow metadata hardlinks are the sole refcount ClickHouse holds on the
//! remote objects. Releasing the freeze at the end of `create` therefore lets ClickHouse
//! merge those parts away and garbage-collect their objects before `upload` performs its
//! CopyObject, which then fails with `NoSuchKey` -- observed in production after a 4.5 hour
//! backup.
//!
//! So for tables with S3-disk parts the freeze must be **held until the objects are
//! copied**, which spans a process boundary in the standalone `create` + `upload` case.
//!
//! # Why a persisted record rather than recomputing names
//!
//! Freeze names are deterministic, so `upload` could recompute them. That is not enough:
//! a recomputed name proves nothing about *whether the freeze was deliberately deferred*
//! (versus left behind by a crash) or *who owns it*. Two concurrent operations must never
//! release each other's freezes. The record makes ownership explicit.
//!
//! # Write-ahead ordering is the safety property
//!
//! The record is published **before** any unfreeze is skipped. A crash in that gap leaves
//! a record with nothing retained -- harmless, recovery is a no-op -- rather than a live
//! freeze with no record, which leaks invisibly with nothing to find it by. If publication
//! fails, the caller must **not** defer: an unrecorded freeze is strictly worse than
//! running with the race.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use super::freeze::FreezeInfo;

/// File name of the record, stored beside `metadata.json` in the backup directory.
const RECORD_FILE: &str = "deferred_freeze.json";

/// Default maximum age at which an *orphaned* deferred freeze becomes reapable.
///
/// The hold has to be bounded: it pins the objects of parts merged away during the hold. But
/// the cost is asymmetric, so the bound is generous. Releasing too early turns the next upload
/// into a hard `NoSuchKey` failure and loses a backup, while holding too long pins only a
/// delta of the object-disk data.
///
/// Sized against real timings: a full backup's upload alone takes >4h on a multi-TiB object
/// disk, and in server mode the clock runs from *create* end (the record is never re-adopted,
/// because create and upload share the process), so a queued or retried upload adds more on
/// top. 6h was measurably too tight; 24h leaves room for a retry cycle on a daily schedule.
///
/// Overridable via `clickhouse.deferred_freeze_ttl_secs`.
pub const DEFAULT_TTL_SECS: u64 = 24 * 60 * 60;

/// A freeze intentionally held past the end of `create`, awaiting object upload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeferredFreezeRecord {
    /// Backup this record belongs to. Cross-checked on load.
    pub backup_name: String,

    /// Tables left frozen, with the exact names needed to UNFREEZE them.
    pub retained: Vec<FreezeInfo>,

    /// PID of the process that owns the freeze, or `0` for a **demoted** record -- one whose
    /// operation has ended while leaving the freeze in place. `is_pid_alive(0)` is false by
    /// design, so a demoted record is bounded by its TTL rather than by liveness.
    pub owner_pid: u32,

    /// Start-time token of the owning process, pairing with `owner_pid` to form an identity.
    ///
    /// `None` means identity could not be established: a record written before this field
    /// existed, or one written on a host without `/proc`. Such a record is treated as **not
    /// live**, leaving the TTL as its bound -- see [`Self::owner_is_live`].
    #[serde(default)]
    pub owner_boot_token: Option<String>,

    /// Unix seconds when the record was published.
    ///
    /// Never advanced by adoption or demotion: the TTL must bound the age of the *freeze*, not
    /// the age of the latest owner, or repeated restarts extend the hold without limit.
    pub created_at_secs: u64,

    /// Seconds after `created_at_secs` at which the hold is considered overdue.
    #[serde(default = "default_ttl")]
    pub ttl_secs: u64,
}

fn default_ttl() -> u64 {
    DEFAULT_TTL_SECS
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Path of the record for a backup.
///
/// Deliberately a single canonical location -- the default backup dir, never per-disk -- so
/// that shadow cleanup can find it from `data_path` + `name` without new parameters, and so
/// the existing backup-dir cleanup removes it alongside `metadata.json`.
pub fn record_path(backup_dir: &Path) -> PathBuf {
    backup_dir.join(RECORD_FILE)
}

/// Path of the record given a data path and backup name.
pub fn record_path_for(data_path: &str, backup_name: &str) -> PathBuf {
    PathBuf::from(data_path)
        .join("backup")
        .join(backup_name)
        .join(RECORD_FILE)
}

impl DeferredFreezeRecord {
    /// Create a record owned by the current process.
    pub fn new(backup_name: &str, retained: Vec<FreezeInfo>, ttl_secs: u64) -> Self {
        let owner_pid = std::process::id();
        Self {
            backup_name: backup_name.to_string(),
            retained,
            owner_pid,
            owner_boot_token: crate::lock::process_start_token(owner_pid),
            created_at_secs: now_secs(),
            ttl_secs,
        }
    }

    /// Whether an operation still owns this freeze.
    ///
    /// "Owns" means *an operation is in flight*, not merely *a process exists*. The distinction
    /// is the whole point: in server mode chbackup is a single long-lived PID 1, so a bare
    /// liveness check reports every record it ever wrote as owned, forever, and the TTL is
    /// unreachable. Two things make the answer honest -- the identity token here, and demotion
    /// by whoever ends the operation (see [`Self::demote`]).
    ///
    /// The three cases, and their fail directions:
    ///
    /// - **Token present**: live only if the PID is alive *and* the process occupying it now is
    ///   the same one. If the current token cannot be read, **fail closed and report live** --
    ///   an unreadable `/proc` entry is not evidence the owner is gone, and calling it dead
    ///   could unfreeze underneath a running finaliser.
    /// - **Token absent**: identity cannot be established, so report **not live** and let the
    ///   TTL bound it. Falling back to PID-only here would leave every record written before
    ///   this field existed permanently protected, because [`protection_status`] answers
    ///   `Protected` on liveness before it ever reaches the TTL branch.
    pub fn owner_is_live(&self) -> bool {
        let alive = crate::lock::is_pid_alive(self.owner_pid);
        let current = crate::lock::process_start_token(self.owner_pid);
        self.liveness_from(alive, current.as_deref())
    }

    /// Pure core of [`Self::owner_is_live`], with the environment passed in.
    ///
    /// Split out so every combination is testable on any host. The real accessor reads
    /// `/proc`, which does not exist on the darwin dev box, so without this seam the
    /// interesting cases would only be exercisable in CI on Linux.
    fn liveness_from(&self, pid_alive: bool, current_token: Option<&str>) -> bool {
        if !pid_alive {
            return false;
        }
        match self.owner_boot_token.as_deref() {
            None => false,
            Some(recorded) => match current_token {
                Some(current) => current == recorded,
                // Indeterminate: assume the owner is live rather than release its freeze.
                None => true,
            },
        }
    }

    /// Whether the hold has exceeded its TTL.
    ///
    /// Note this does **not** license outside deletion: an overdue but owner-live record is
    /// still protected. The TTL's job is to make the owning upload stop and release.
    pub fn is_expired(&self) -> bool {
        now_secs().saturating_sub(self.created_at_secs) > self.ttl_secs
    }

    /// Whether this record is owned by the calling process.
    ///
    /// Token-aware for the same reason as [`Self::owner_is_live`], and checked *first* by
    /// [`load`] -- so bare PID equality would let any fresh PID 1 claim a dead pod's record and
    /// skip both the liveness check and the TTL. The fail-closed answer here is the opposite
    /// one: "not mine". Both directions err toward leaving the freeze protected.
    pub fn owned_by_current_process(&self) -> bool {
        let pid = std::process::id();
        self.ownership_from(pid, crate::lock::process_start_token(pid).as_deref())
    }

    /// Pure core of [`Self::owned_by_current_process`], with the environment passed in.
    ///
    /// Both tokens must be present and equal. An absent *recorded* token means the record
    /// predates the field, and an absent *current* token means this host cannot establish
    /// identity at all -- in neither case can ownership be proven, and the fail-closed answer
    /// for ownership is "not mine".
    fn ownership_from(&self, current_pid: u32, current_token: Option<&str>) -> bool {
        if self.owner_pid != current_pid {
            return false;
        }
        matches!(
            (self.owner_boot_token.as_deref(), current_token),
            (Some(recorded), Some(current)) if recorded == current
        )
    }

    /// Take ownership, for adoption of an orphaned-but-valid record.
    ///
    /// Refreshes the owner identity but deliberately **not** `created_at_secs`: the adopting
    /// operation is protected by the per-backup lock it holds while it copies, and advancing the
    /// timestamp would restart the TTL on every adoption, so N restarts would grant N × TTL and
    /// the hold would be unbounded again.
    pub fn adopt(&mut self) {
        self.owner_pid = std::process::id();
        self.owner_boot_token = crate::lock::process_start_token(self.owner_pid);
    }

    /// Give up ownership while leaving the freeze in place.
    ///
    /// Called by whoever ends an operation without releasing the freeze -- a failed upload
    /// keeping its objects pinned for a retry, or a partial UNFREEZE keeping the leak visible.
    /// The freeze stays held, but liveness stops protecting it, so the TTL becomes its actual
    /// bound. Without this, a long-lived server's records are protected forever: the process
    /// that wrote them is still running and its token still matches.
    ///
    /// `created_at_secs` is preserved, so repeated failures cannot extend the hold.
    pub fn demote(&mut self) {
        self.owner_pid = 0;
        self.owner_boot_token = None;
    }
}

/// Publish a record atomically (write to `.tmp`, then rename).
///
/// Must be called **before** the corresponding unfreeze is skipped -- see the module docs.
/// Errors are propagated deliberately: the caller has to fall back to unfreezing normally
/// rather than deferring without a record.
pub fn publish(backup_dir: &Path, record: &DeferredFreezeRecord) -> Result<()> {
    let path = record_path(backup_dir);
    crate::resume::save_state_file(&path, record).with_context(|| {
        format!(
            "Failed to publish deferred-freeze record {}",
            path.display()
        )
    })?;
    info!(
        backup = %record.backup_name,
        tables = record.retained.len(),
        owner_pid = record.owner_pid,
        path = %path.display(),
        "Published deferred-freeze record (S3 object-disk tables stay frozen until upload copies their objects)"
    );
    Ok(())
}

/// Outcome of loading a record.
#[derive(Debug)]
pub enum LoadOutcome {
    /// No record present -- nothing was deferred.
    None,
    /// A usable record. Already adopted if it was orphaned.
    Usable(DeferredFreezeRecord),
    /// A record exists but is unreadable/corrupt. The freeze state is unknown.
    ///
    /// Callers must **not** guess which tables to unfreeze: abort and leave the shadow in
    /// place for operator recovery. Refusing costs storage; guessing can release a
    /// different live backup's freeze.
    Corrupt(String),
    /// The record belongs to a different, still-running process.
    ForeignLive(DeferredFreezeRecord),
}

/// Load the record for a backup, adopting it if it is orphaned but valid.
///
/// Callers must hold the per-backup PID lock: the adopt decision has to be serialized, or
/// two processes could release each other's freezes.
pub fn load(backup_dir: &Path, expected_backup_name: &str) -> LoadOutcome {
    let path = record_path(backup_dir);
    if !path.exists() {
        return LoadOutcome::None;
    }

    let record: DeferredFreezeRecord =
        match crate::resume::load_state_file::<DeferredFreezeRecord>(&path) {
            Ok(Some(r)) => r,
            Ok(None) => return LoadOutcome::None,
            Err(e) => {
                return LoadOutcome::Corrupt(format!("failed to parse {}: {e:#}", path.display()))
            }
        };

    if record.backup_name != expected_backup_name {
        return LoadOutcome::Corrupt(format!(
            "record at {} is for backup {:?}, expected {:?}",
            path.display(),
            record.backup_name,
            expected_backup_name
        ));
    }

    if record.owned_by_current_process() {
        return LoadOutcome::Usable(record);
    }

    if record.owner_is_live() {
        return LoadOutcome::ForeignLive(record);
    }

    // Orphaned but structurally valid: adopt rather than unfreeze. A shadow snapshot does
    // not go stale -- the objects are still pinned and still correct -- so releasing it
    // before copying would recreate the very race this mechanism exists to prevent.
    let mut adopted = record;
    let previous_pid = adopted.owner_pid;
    adopted.adopt();
    if let Err(e) = publish(backup_dir, &adopted) {
        warn!(
            error = %e,
            "Failed to rewrite deferred-freeze record while adopting it; continuing with in-memory ownership"
        );
    }
    info!(
        backup = %adopted.backup_name,
        previous_owner_pid = previous_pid,
        tables = adopted.retained.len(),
        "Adopted orphaned deferred-freeze record"
    );
    LoadOutcome::Usable(adopted)
}

/// Remove the record. Call only after the freeze has actually been released.
pub fn delete(backup_dir: &Path) -> Result<()> {
    let path = record_path(backup_dir);
    if !path.exists() {
        return Ok(());
    }
    std::fs::remove_file(&path)
        .with_context(|| format!("Failed to remove deferred-freeze record {}", path.display()))
}

/// Release a backup's deferred freeze on an operator's explicit instruction, TTL or not.
///
/// The escape hatch for a record that nothing else will clear. Every automatic path is bounded:
/// `create` pre-flight and failed-backup cleanup only touch *expired* records, and the `clean`
/// routes hold the global lock and so reap nothing at all. Before this existed the only recourse
/// was hand-editing `deferred_freeze.json` inside a running pod.
///
/// Deliberately **not** a flag on `clean`. `clean` takes the `Global` lock, which is mutually
/// exclusive with `Backup(name)`, so a release hung off it could never acquire the lock it needs.
///
/// The caller is responsible for holding the `Backup(name)` lock — `lock_for_command` maps
/// `release-deferred` into the backup-scoped arm for exactly that reason. Releasing under a live
/// upload recreates the `NoSuchKey` race, hence the warning.
///
/// Goes through `unfreeze_all_checked` + [`retain_failed`], never unfreeze-then-delete, so a
/// partial failure leaves the still-frozen entries recorded rather than invisible.
pub async fn release_now(
    ch: &crate::clickhouse::client::ChClient,
    data_path: &str,
    backup_name: &str,
) -> Result<usize> {
    let backup_dir = PathBuf::from(data_path).join("backup").join(backup_name);
    let path = record_path(&backup_dir);
    if !path.exists() {
        return Ok(0);
    }

    let record = match crate::resume::load_state_file::<DeferredFreezeRecord>(&path) {
        Ok(Some(r)) if r.backup_name == backup_name => r,
        Ok(Some(r)) => anyhow::bail!(
            "deferred-freeze record at {} is for backup {:?}, not {:?}; refusing to act on it",
            path.display(),
            r.backup_name,
            backup_name
        ),
        Ok(None) => return Ok(0),
        Err(e) => anyhow::bail!(
            "deferred-freeze record at {} is unreadable ({e:#}); refusing to guess which tables \
             to UNFREEZE, since releasing the wrong entry would drop another backup's protection",
            path.display()
        ),
    };

    let count = record.retained.len();
    warn!(
        backup = %backup_name,
        tables = count,
        age_secs = now_secs().saturating_sub(record.created_at_secs),
        ttl_secs = record.ttl_secs,
        "Releasing a deferred S3 object-disk freeze on explicit instruction, bypassing the TTL. \
         If an upload for this backup is still running, its CopyObject may now fail with \
         NoSuchKey."
    );

    let mut record = record;
    let retained = std::mem::take(&mut record.retained);
    let mut guard = super::freeze::FreezeGuard::from_frozen(retained);
    match guard.unfreeze_all_checked(ch).await {
        Ok(()) => {
            delete(&backup_dir)?;
            Ok(count)
        }
        Err(failed) => {
            let still_frozen = failed.len();
            retain_failed(&backup_dir, backup_name, failed, &record);
            anyhow::bail!(
                "released {} of {} deferred freezes for '{}'; {} could not be unfrozen and remain \
                 recorded. Fix the cause and re-run.",
                count - still_frozen,
                count,
                backup_name,
                still_frozen
            )
        }
    }
}

/// Mark the record as no longer owned by a running operation, keeping the freeze in place.
///
/// Called by whoever ends an operation without releasing the freeze -- principally a failed
/// upload, which keeps its objects pinned so a retry stays protected. Until this exists, such a
/// record is protected *forever* in server mode: the process that wrote it is still running and
/// its start-time token still matches, so liveness answers `Protected` and the TTL branch is
/// never reached.
///
/// Reads the record directly rather than through [`load`], because `load` **adopts** an orphaned
/// record as a side effect, and an operation that is ending must not take ownership of anything.
///
/// This is also the one place an overrun is observable: if the hold already outlived its TTL
/// while the operation was still running, the record was reapable underneath a live operation --
/// exactly the race the TTL exists to prevent. Nothing in the binary can prevent it (the
/// worst-case upload duration is not knowable in advance), so it is logged loudly instead.
pub fn demote_on_operation_end(backup_dir: &Path, backup_name: &str) {
    let path = record_path(backup_dir);
    if !path.exists() {
        return;
    }

    let mut record = match crate::resume::load_state_file::<DeferredFreezeRecord>(&path) {
        Ok(Some(r)) if r.backup_name == backup_name => r,
        Ok(Some(r)) => {
            warn!(
                backup = %backup_name,
                record_backup = %r.backup_name,
                "Deferred-freeze record names a different backup; not demoting it"
            );
            return;
        }
        Ok(None) => return,
        Err(e) => {
            warn!(
                backup = %backup_name,
                error = %e,
                "Deferred-freeze record is unreadable; leaving it as-is. It stays protected, \
                 and `chbackup release-deferred` is the way out."
            );
            return;
        }
    };

    let elapsed = now_secs().saturating_sub(record.created_at_secs);
    if elapsed > record.ttl_secs {
        warn!(
            backup = %backup_name,
            elapsed_secs = elapsed,
            ttl_secs = record.ttl_secs,
            "Deferred S3 object-disk freeze outlived its TTL while its operation was still \
             running -- it was reapable underneath a live operation, which risks a CopyObject \
             NoSuchKey. Raise clickhouse.deferred_freeze_ttl_secs above the worst-case upload \
             duration for this cluster."
        );
    }

    if record.owner_pid == 0 && record.owner_boot_token.is_none() {
        return; // already demoted; nothing to rewrite
    }

    record.demote();
    if let Err(e) = publish(backup_dir, &record) {
        warn!(
            backup = %backup_name,
            error = %e,
            "Failed to demote deferred-freeze record; it stays owner-live and will not expire \
             until released with `chbackup release-deferred`"
        );
    }
}

/// Rewrite the record with only the entries that are still frozen.
///
/// Used when an unfreeze partially fails: the record must not be deleted while a table
/// remains frozen, or the leak becomes invisible.
///
/// Takes `prior` -- the record being replaced -- rather than rebuilding from scratch, because
/// two of its fields must survive the rewrite:
///
/// - **`ttl_secs`**: the freeze must stay bounded by the value it was *published* under. Using
///   the constant here silently reset a configured TTL (an operator who raised it because their
///   uploads run long got 24h back), and using current config would let a config change
///   retroactively shorten a hold already in flight.
/// - **`created_at_secs`**: the TTL bounds the age of the freeze, not the age of the most recent
///   failure. Restarting the clock on every partial failure would let a daily retry keep the hold
///   alive indefinitely.
///
/// The rewritten record is **demoted**: this function is only ever reached as an operation ends,
/// so nothing is in flight to protect. That is what lets the TTL apply -- see
/// [`DeferredFreezeRecord::demote`].
pub fn retain_failed(
    backup_dir: &Path,
    backup_name: &str,
    failed: Vec<FreezeInfo>,
    prior: &DeferredFreezeRecord,
) {
    let record = DeferredFreezeRecord {
        backup_name: backup_name.to_string(),
        retained: failed,
        owner_pid: 0,
        owner_boot_token: None,
        created_at_secs: prior.created_at_secs,
        ttl_secs: prior.ttl_secs,
    };
    if let Err(e) = publish(backup_dir, &record) {
        warn!(
            backup = %backup_name,
            error = %e,
            "Failed to rewrite deferred-freeze record after partial unfreeze; \
             run `chbackup clean` to remove leftover shadow directories"
        );
    }
}

/// Why a destructive operation on a backup's local data is or is not permitted.
#[derive(Debug, PartialEq, Eq)]
pub enum ProtectionStatus {
    /// No deferred freeze recorded -- nothing to protect.
    NotProtected,
    /// A deferred freeze is held. Destroying local data would strand it: for object disks the
    /// refcount lives in the shadow metadata files, so removing them is not an UNFREEZE.
    Protected {
        reason: &'static str,
        owner_pid: u32,
    },
    /// The record is expired and no operation holds the lock, so a *lock-holding reaper* may
    /// release it. Ordinary callers must still not delete blindly -- they have to reap first.
    Reapable(DeferredFreezeRecord),
}

/// Whether a deferred freeze protects this backup's local data from destruction.
///
/// Used by shadow cleanup and by local-backup deletion. Both must refuse while a freeze is
/// held, because `rm -rf` of the shadow metadata does **not** release ClickHouse's refcount on
/// the referenced S3 objects -- only `UNFREEZE` does.
///
/// The predicate is deliberately **not** PID equality. In server mode `create` and `upload` run
/// inside the same long-lived process, so every record carries the server's PID; treating
/// "same PID" as ownership would let an unrelated request in that process destroy a live
/// freeze. Ownership is instead evidenced by holding the **per-backup PID lock**, which is
/// what actually serializes operations on a backup.
///
/// Fails **closed** on an unreadable record: ownership cannot be established from corrupt
/// data, so assume it is protected.
///
/// # Three independent signals, deliberately
///
/// Protection holds if *any* of: the per-backup lock is active, the recorded owner process is
/// alive, or the TTL has not expired. That layering is what makes the cancellation window safe
/// without transferring lock ownership into the upload task:
///
/// - Cancelling an operation drops the caller's `PidLock` while `upload`'s spawned task keeps
///   running to finalise. The lock signal is gone for that window.
/// - **Server mode**: the record's owner is the long-lived server process, still alive, so
///   signal 2 protects it.
/// - **CLI cross-process**: the creator has exited, but the TTL has not expired, so signal 3
///   protects it.
///
/// A single-signal predicate keyed only on the lock would need the guard handed into the
/// finalising task — which `run_operation` cannot do (it is generic over a closure and does not
/// know it is calling upload), and which `create_remote` would break (it holds the lock across
/// create *and* upload, so upload acquiring its own would fail). The layered predicate gets the
/// same safety for less machinery. See the tests named for the cancellation window.
pub fn protection_status(data_path: &str, backup_name: &str) -> ProtectionStatus {
    status_inner(data_path, backup_name, true)
}

/// [`protection_status`] for a caller that already holds this backup's PID lock.
///
/// The lock signal has to be omitted for such a caller, because it would read its **own** lock
/// file and conclude that an operation is in flight. `reap_expired` does exactly that: it
/// acquires the per-backup lock and then re-reads the status, so with the lock signal included
/// the recheck always answered `Protected` and the reaper could never reap anything. That was
/// masked for as long as no record reached `Reapable` at all.
///
/// Dropping the signal is safe here precisely *because* the caller holds the lock: the lock is
/// what excludes an upload across the whole re-read -> UNFREEZE -> delete sequence, so
/// re-deriving that exclusion from the lock file adds a filesystem read and no safety.
fn status_under_lock(data_path: &str, backup_name: &str) -> ProtectionStatus {
    status_inner(data_path, backup_name, false)
}

fn status_inner(data_path: &str, backup_name: &str, consult_lock: bool) -> ProtectionStatus {
    let path = record_path_for(data_path, backup_name);
    if !path.exists() {
        return ProtectionStatus::NotProtected;
    }

    let record = match crate::resume::load_state_file::<DeferredFreezeRecord>(&path) {
        Ok(Some(r)) => r,
        Ok(None) => return ProtectionStatus::NotProtected,
        Err(e) => {
            warn!(
                backup = %backup_name,
                error = %e,
                path = %path.display(),
                "Deferred-freeze record is unreadable -- treating the backup as protected. \
                 Ownership cannot be established from corrupt data."
            );
            return ProtectionStatus::Protected {
                reason: "deferred-freeze record is unreadable",
                owner_pid: 0,
            };
        }
    };

    // An operation is in flight for this backup: it owns the freeze, whoever recorded it.
    if consult_lock && backup_lock_is_active(backup_name) {
        return ProtectionStatus::Protected {
            reason: "an operation holds the per-backup lock",
            owner_pid: record.owner_pid,
        };
    }

    // No lock held. A record whose owner is still alive is mid-flight without the lock
    // (e.g. queued, or between lock acquisition points) -- leave it alone.
    if record.owner_is_live() {
        return ProtectionStatus::Protected {
            reason: "the recorded owner process is still alive",
            owner_pid: record.owner_pid,
        };
    }

    // Orphaned. Still protected until the TTL expires: the upload it belongs to may simply be
    // between attempts, and releasing early recreates the race this record exists to prevent.
    if !record.is_expired() {
        return ProtectionStatus::Protected {
            reason: "the deferred freeze is orphaned but has not yet expired",
            owner_pid: record.owner_pid,
        };
    }

    ProtectionStatus::Reapable(record)
}

/// Whether shadow cleanup or local deletion must refuse to touch this backup.
///
/// `Reapable` counts as blocking here: an ordinary caller must not delete a backup whose
/// freeze is still registered with ClickHouse. Only the lock-holding reaper may act on it,
/// and it uses [`protection_status`] directly.
pub fn blocks_destructive_op(data_path: &str, backup_name: &str, op: &str) -> bool {
    match protection_status(data_path, backup_name) {
        ProtectionStatus::NotProtected => false,
        ProtectionStatus::Protected { reason, owner_pid } => {
            warn!(
                backup = %backup_name,
                op = %op,
                owner_pid = owner_pid,
                reason = %reason,
                "Refusing to destroy local backup data: a deferred S3 object-disk freeze is held"
            );
            true
        }
        ProtectionStatus::Reapable(_) => {
            warn!(
                backup = %backup_name,
                op = %op,
                "Refusing to destroy local backup data: an expired deferred freeze must be \
                 released first. Run `chbackup release-deferred {backup_name}`, or wait for the \
                 next `create`, whose pre-flight reaps it. `chbackup clean` cannot -- it holds \
                 the global lock and so can never acquire the per-backup lock a release needs."
            );
            true
        }
    }
}

/// Release expired, orphaned deferred freezes across all local backups.
///
/// Returns the number of backups whose freeze was released.
///
/// # Where this runs, and why
///
/// A `create` whose `upload` never arrives leaves a live freeze that nothing else releases. The
/// deployment that motivated the mechanism never runs `clean` — its CronJob only does create and
/// upload — so a clean-only reaper would let orphans accumulate indefinitely.
///
/// **It is only effective for callers that do not hold the `Global` lock.** Acquiring a
/// `Backup(name)` lock fails while `Global` is held, since the tiers are mutually exclusive, so
/// the two reachable-from-`clean` routes (`clean` CLI and the API `clean` route, both via
/// `list::clean_shadow`) reap nothing. The two that work are `create` pre-flight and
/// `cleanup_failed_backup`, both `Backup`-scoped. A skipped-everything pass is logged at debug so
/// the no-op is visible rather than silent.
///
/// # Locking
///
/// For each candidate the per-backup PID lock is **acquired and held** across the whole
/// sequence — re-read the record, UNFREEZE, delete the record — and the record is re-read
/// *after* acquiring. A point-in-time liveness check would be a TOCTOU gap: an upload could
/// start between the check and the UNFREEZE, and we would release a freeze it still needs.
/// A backup whose lock cannot be acquired is simply skipped; it is in use.
///
/// `own_lock` names the backup whose lock the **caller** already holds, if any. For that one
/// candidate the lock is self-noise: including it would skip the record at the pre-check, which
/// is why `create`'s documented same-name coverage never actually worked. Such a candidate is
/// judged with [`status_under_lock`] and not re-acquired — a same-tier, same-name acquisition by
/// a process that already holds it would fail.
pub async fn reap_expired(
    ch: &crate::clickhouse::client::ChClient,
    data_path: &str,
    own_lock: Option<&str>,
) -> usize {
    let backups_dir = PathBuf::from(data_path).join("backup");
    let entries = match std::fs::read_dir(&backups_dir) {
        Ok(e) => e,
        Err(_) => return 0, // no local backups at all
    };

    let mut reaped = 0usize;
    let mut candidates = 0usize;
    let mut skipped_on_lock = 0usize;
    for entry in entries.flatten() {
        let backup_dir = entry.path();
        if !backup_dir.is_dir() || !record_path(&backup_dir).exists() {
            continue;
        }
        let backup_name = match backup_dir.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };
        candidates += 1;

        // Does the caller already hold this backup's lock? If so the lock is self-noise and must
        // be excluded from every judgement below, including the pre-check.
        let self_owned = own_lock == Some(backup_name.as_str());
        let status = if self_owned {
            status_under_lock(data_path, &backup_name)
        } else {
            protection_status(data_path, &backup_name)
        };

        // Cheap pre-check before taking the lock; the authoritative read happens below.
        if !matches!(status, ProtectionStatus::Reapable(_)) {
            continue;
        }

        // Acquire and HOLD the per-backup lock for the whole release. Going through
        // `acquire_scoped` rather than `PidLock::acquire` keeps the reaper inside the
        // acquisition gate, so it neither races the cross-tier scan nor publishes a lock file
        // that a gated scan could observe mid-publication.
        //
        // Skipped entirely when the caller already holds this lock: a same-tier, same-name
        // acquisition would fail against ourselves.
        let _lock = if self_owned {
            None
        } else {
            let scope = crate::lock::LockScope::Backup(backup_name.clone());
            match crate::lock::acquire_scoped(
                crate::lock::default_lock_dir(),
                &scope,
                "reap_deferred_freeze",
            ) {
                Ok(l) => Some(l),
                Err(_) => {
                    // In use, or we hold `Global` and the tiers exclude each other.
                    skipped_on_lock += 1;
                    continue;
                }
            }
        };

        // Re-read under the lock: the state may have changed while we were acquiring. Uses
        // `status_under_lock` because the lock we now hold is our own.
        let record = match status_under_lock(data_path, &backup_name) {
            ProtectionStatus::Reapable(r) => r,
            _ => continue,
        };

        warn!(
            backup = %backup_name,
            tables = record.retained.len(),
            age_secs = now_secs().saturating_sub(record.created_at_secs),
            "Reaping expired deferred S3 object-disk freeze -- its upload never completed"
        );

        let mut record = record;
        let retained = std::mem::take(&mut record.retained);
        let mut guard = super::freeze::FreezeGuard::from_frozen(retained);
        match guard.unfreeze_all_checked(ch).await {
            Ok(()) => {
                if let Err(e) = delete(&backup_dir) {
                    warn!(error = %e, "Failed to remove deferred-freeze record after reaping");
                }
                reaped += 1;
            }
            Err(failed) => {
                // Keep the still-frozen entries so the next run retries them.
                retain_failed(&backup_dir, &backup_name, failed, &record);
            }
        }
    }

    // Make a wholly ineffective pass visible. The `clean` routes always land here, because they
    // hold `Global` and can never acquire a `Backup` lock -- worth a breadcrumb rather than
    // looking like "nothing needed reaping".
    if reaped == 0 && skipped_on_lock > 0 && skipped_on_lock == candidates {
        debug!(
            candidates = candidates,
            "Deferred-freeze reap released nothing: every candidate's lock was unavailable. \
             Expected when the caller holds the global lock (`clean`); the effective reap points \
             are `create` pre-flight and failed-backup cleanup."
        );
    }

    reaped
}

/// Whether some operation currently holds this backup's PID lock.
fn backup_lock_is_active(backup_name: &str) -> bool {
    let scope = crate::lock::LockScope::Backup(backup_name.to_string());
    match crate::lock::lock_path_for_scope(crate::lock::default_lock_dir(), &scope) {
        Some(p) => crate::lock::is_lock_file_active(&p),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(db: &str, table: &str) -> FreezeInfo {
        FreezeInfo {
            database: db.to_string(),
            table: table.to_string(),
            freeze_name: crate::clickhouse::freeze_name("bk-1", db, table),
        }
    }

    #[test]
    fn test_publish_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let rec = DeferredFreezeRecord::new("bk-1", vec![info("db", "t")], DEFAULT_TTL_SECS);
        publish(dir.path(), &rec).unwrap();

        match load(dir.path(), "bk-1") {
            LoadOutcome::Usable(loaded) => {
                assert_eq!(loaded.retained, rec.retained);
                assert_eq!(loaded.backup_name, "bk-1");
                // Recognised as ours only where process identity can be established. Without
                // `/proc` the record is instead adopted, which also yields `Usable` -- see
                // `test_ownership_matrix_is_host_independent` for the semantics themselves.
                if crate::lock::process_start_token(std::process::id()).is_some() {
                    assert!(loaded.owned_by_current_process());
                }
            }
            other => panic!("expected Usable, got {other:?}"),
        }
    }

    #[test]
    fn test_load_none_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(load(dir.path(), "bk-1"), LoadOutcome::None));
    }

    #[test]
    fn test_load_corrupt_is_not_silently_empty() {
        // A corrupt record must never degrade to "nothing was deferred" -- that would
        // guarantee a silent leak.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(record_path(dir.path()), b"{ not json").unwrap();
        assert!(matches!(load(dir.path(), "bk-1"), LoadOutcome::Corrupt(_)));
    }

    #[test]
    fn test_load_rejects_mismatched_backup_name() {
        let dir = tempfile::tempdir().unwrap();
        let rec = DeferredFreezeRecord::new("other", vec![info("db", "t")], DEFAULT_TTL_SECS);
        publish(dir.path(), &rec).unwrap();
        assert!(matches!(load(dir.path(), "bk-1"), LoadOutcome::Corrupt(_)));
    }

    #[test]
    fn test_orphaned_record_is_adopted_not_unfrozen() {
        let dir = tempfile::tempdir().unwrap();
        let mut rec = DeferredFreezeRecord::new("bk-1", vec![info("db", "t")], DEFAULT_TTL_SECS);
        // PID 0 is never a live user process.
        rec.owner_pid = 0;
        publish(dir.path(), &rec).unwrap();

        match load(dir.path(), "bk-1") {
            LoadOutcome::Usable(loaded) => {
                assert_eq!(
                    loaded.owner_pid,
                    std::process::id(),
                    "adoption must take ownership of the PID"
                );
                if crate::lock::process_start_token(std::process::id()).is_some() {
                    assert!(
                        loaded.owned_by_current_process(),
                        "and, where identity is available, be recognisable as ours"
                    );
                }
                assert_eq!(loaded.retained.len(), 1, "retained entries preserved");
            }
            other => panic!("expected adoption, got {other:?}"),
        }
    }

    #[test]
    fn test_expired_but_foreign_live_still_blocks_cleanup() {
        // TTL expiry must NOT license outside deletion while the owner is alive: a
        // long-running upload can exceed its deadline before its finaliser runs, and
        // deleting its shadow mid-copy reintroduces the race this mechanism prevents.
        let tmp = tempfile::tempdir().unwrap();
        let data_path = tmp.path().to_str().unwrap();
        let backup_dir = tmp.path().join("backup").join("bk-1");
        std::fs::create_dir_all(&backup_dir).unwrap();

        let mut rec = DeferredFreezeRecord::new("bk-1", vec![info("db", "t")], 0);
        rec.created_at_secs = 1; // long past
        rec.owner_pid = 1; // live, and not us
        publish(&backup_dir, &rec).unwrap();

        assert!(rec.is_expired(), "fixture must be expired");
        assert!(
            blocks_destructive_op(data_path, "bk-1", "test"),
            "expired record with a live foreign owner must still block cleanup"
        );
    }

    #[test]
    fn test_pid_zero_is_treated_as_dead() {
        // kill(0, 0) targets the caller's process group and succeeds, so PID 0 would
        // otherwise be reported alive and could pin a shadow forever.
        assert!(!crate::lock::is_pid_alive(0));
    }

    /// Helper: publish a record for `bk` under `tmp` and return the data_path.
    fn publish_at(tmp: &std::path::Path, bk: &str, mutate: impl FnOnce(&mut DeferredFreezeRecord)) {
        let backup_dir = tmp.join("backup").join(bk);
        std::fs::create_dir_all(&backup_dir).unwrap();
        let mut rec = DeferredFreezeRecord::new(bk, vec![info("db", "t")], DEFAULT_TTL_SECS);
        mutate(&mut rec);
        publish(&backup_dir, &rec).unwrap();
    }

    #[test]
    fn test_same_pid_record_still_blocks() {
        // Regression: PID equality is NOT ownership. In server mode create and upload share
        // one long-lived process, so every record carries the server's PID -- treating that as
        // "mine, safe to delete" let an unrelated request destroy a live freeze.
        let tmp = tempfile::tempdir().unwrap();
        let data_path = tmp.path().to_str().unwrap();
        publish_at(tmp.path(), "bk-1", |_| {}); // owner_pid = current process

        assert!(
            blocks_destructive_op(data_path, "bk-1", "test"),
            "a record owned by the current PID must still block when we hold no lock"
        );
    }

    #[test]
    fn test_foreign_live_record_blocks() {
        let tmp = tempfile::tempdir().unwrap();
        let data_path = tmp.path().to_str().unwrap();
        // PID 1 exists on every Unix system and is not us.
        publish_at(tmp.path(), "bk-1", |r| r.owner_pid = 1);

        assert!(blocks_destructive_op(data_path, "bk-1", "test"));
    }

    #[test]
    fn test_unexpired_orphan_blocks() {
        // Regression: an orphaned record used to be treated as free to delete regardless of
        // TTL, so `clean <name>` would rm -rf a live deferred shadow in the create->upload
        // gap, where the creator process is legitimately gone.
        let tmp = tempfile::tempdir().unwrap();
        let data_path = tmp.path().to_str().unwrap();
        publish_at(tmp.path(), "bk-1", |r| r.owner_pid = 0); // orphan, TTL not expired

        match protection_status(data_path, "bk-1") {
            ProtectionStatus::Protected { .. } => {}
            other => panic!("unexpired orphan must be Protected, got {other:?}"),
        }
        assert!(blocks_destructive_op(data_path, "bk-1", "test"));
    }

    #[test]
    fn test_expired_orphan_is_reapable_but_still_blocks_ordinary_callers() {
        let tmp = tempfile::tempdir().unwrap();
        let data_path = tmp.path().to_str().unwrap();
        publish_at(tmp.path(), "bk-1", |r| {
            r.owner_pid = 0;
            r.created_at_secs = 1;
            r.ttl_secs = 0;
        });

        match protection_status(data_path, "bk-1") {
            ProtectionStatus::Reapable(rec) => assert_eq!(rec.retained.len(), 1),
            other => panic!("expired orphan must be Reapable, got {other:?}"),
        }
        // Reapable is still not a licence for an ordinary caller to rm -rf: the freeze is
        // registered with ClickHouse and needs a real UNFREEZE first.
        assert!(blocks_destructive_op(data_path, "bk-1", "test"));
    }

    #[test]
    fn test_record_survives_for_retry_semantics() {
        // Documents the invariant that upload's finaliser relies on for 1b: after a failed
        // upload the record must still be loadable, because upload.state.json survives and
        // the retry needs the objects still pinned. A released freeze would make every retry
        // hit the NoSuchKey race.
        let dir = tempfile::tempdir().unwrap();
        let rec = DeferredFreezeRecord::new("bk-1", vec![info("db", "t")], DEFAULT_TTL_SECS);
        publish(dir.path(), &rec).unwrap();

        // Simulate "upload failed, finaliser kept the record" -- nothing deleted it.
        assert!(record_path(dir.path()).exists());
        match load(dir.path(), "bk-1") {
            LoadOutcome::Usable(loaded) => assert_eq!(loaded.retained.len(), 1),
            other => panic!("retry must still find the record, got {other:?}"),
        }
    }

    #[test]
    fn test_cancellation_window_server_mode_stays_protected() {
        // Cancelling an operation drops the caller's PidLock while upload's spawned task keeps
        // running to finalise. In server mode the record's owner is the long-lived server
        // process, so owner-liveness must protect the freeze even with no lock held --
        // otherwise the reaper could release it mid-copy.
        let tmp = tempfile::tempdir().unwrap();
        let data_path = tmp.path().to_str().unwrap();
        publish_at(tmp.path(), "bk-1", |_| {}); // owner = current (live) process

        assert!(
            !backup_lock_is_active("bk-1"),
            "no lock held, as after a cancel"
        );
        // The invariant is that it stays Protected. *Which* signal does it depends on the host:
        // where process identity can be established the owner reads as live (signal 2); where it
        // cannot -- no `/proc`, i.e. the darwin dev box -- the record is treated as orphaned and
        // the unexpired TTL protects it instead (signal 3). Either is correct; the layered design
        // exists precisely so that any one signal suffices.
        match protection_status(data_path, "bk-1") {
            ProtectionStatus::Protected { reason, .. } => {
                if crate::lock::process_start_token(std::process::id()).is_some() {
                    assert!(
                        reason.contains("alive"),
                        "with identity available, owner-liveness should be the protecting \
                         signal: {reason}"
                    );
                }
            }
            other => panic!("cancellation window must stay protected, got {other:?}"),
        }
    }

    #[test]
    fn test_liveness_matrix_is_host_independent() {
        // The cases that matter, exercised through the injectable core so they run everywhere
        // rather than only on Linux.
        let mut rec = DeferredFreezeRecord::new("bk-1", vec![info("db", "t")], DEFAULT_TTL_SECS);

        rec.owner_boot_token = Some("start-42".to_string());
        assert!(
            rec.liveness_from(true, Some("start-42")),
            "same process: live"
        );
        assert!(
            !rec.liveness_from(true, Some("start-99")),
            "PID recycled to a different process: not live"
        );
        assert!(
            rec.liveness_from(true, None),
            "indeterminate current token must fail CLOSED (assume live)"
        );
        assert!(!rec.liveness_from(false, Some("start-42")), "dead PID");

        rec.owner_boot_token = None;
        assert!(
            !rec.liveness_from(true, Some("start-42")),
            "legacy record: identity unknowable, so not live -- this is what lets the TTL apply \
             and is the fix for the wedged records in production"
        );
    }

    #[test]
    fn test_ownership_matrix_is_host_independent() {
        let mut rec = DeferredFreezeRecord::new("bk-1", vec![info("db", "t")], DEFAULT_TTL_SECS);
        rec.owner_pid = 1;
        rec.owner_boot_token = Some("start-42".to_string());

        assert!(rec.ownership_from(1, Some("start-42")));
        assert!(
            !rec.ownership_from(1, Some("start-99")),
            "a fresh PID 1 must not claim a dead pod's record"
        );
        assert!(!rec.ownership_from(2, Some("start-42")), "different PID");
        assert!(
            !rec.ownership_from(1, None),
            "cannot prove ownership without a current token"
        );

        rec.owner_boot_token = None;
        assert!(
            !rec.ownership_from(1, Some("start-42")),
            "bare PID equality must not imply ownership"
        );
    }

    #[test]
    fn test_cancellation_window_cli_stays_protected_until_ttl() {
        // Cross-process CLI: the creator has exited, so owner-liveness cannot help. The TTL is
        // the signal that keeps the freeze protected across a cancelled upload.
        let tmp = tempfile::tempdir().unwrap();
        let data_path = tmp.path().to_str().unwrap();
        publish_at(tmp.path(), "bk-1", |r| r.owner_pid = 0); // dead creator, TTL unexpired

        match protection_status(data_path, "bk-1") {
            ProtectionStatus::Protected { reason, .. } => {
                assert!(
                    reason.contains("expired"),
                    "expected TTL to protect: {reason}"
                );
            }
            other => panic!("cancellation window must stay protected, got {other:?}"),
        }
    }

    #[test]
    fn test_no_record_is_not_protected() {
        let tmp = tempfile::tempdir().unwrap();
        let data_path = tmp.path().to_str().unwrap();
        std::fs::create_dir_all(tmp.path().join("backup").join("bk-1")).unwrap();
        assert_eq!(
            protection_status(data_path, "bk-1"),
            ProtectionStatus::NotProtected
        );
        assert!(!blocks_destructive_op(data_path, "bk-1", "test"));
    }

    #[test]
    fn test_corrupt_record_fails_closed() {
        let tmp = tempfile::tempdir().unwrap();
        let data_path = tmp.path().to_str().unwrap();
        let backup_dir = tmp.path().join("backup").join("bk-1");
        std::fs::create_dir_all(&backup_dir).unwrap();
        std::fs::write(record_path(&backup_dir), b"garbage").unwrap();

        assert!(
            blocks_destructive_op(data_path, "bk-1", "test"),
            "unreadable record must fail closed, not assume the owner is dead"
        );
    }

    #[test]
    fn test_reap_candidate_selection() {
        // reap_expired needs a live ChClient to UNFREEZE, so exercise the selection predicate
        // it uses rather than the unfreeze itself (that path is covered by integration T75).
        let tmp = tempfile::tempdir().unwrap();
        let data_path = tmp.path().to_str().unwrap();

        // Reapable: orphaned and expired.
        publish_at(tmp.path(), "expired-orphan", |r| {
            r.owner_pid = 0;
            r.created_at_secs = 1;
            r.ttl_secs = 0;
        });
        // Not reapable: orphaned but within TTL.
        publish_at(tmp.path(), "fresh-orphan", |r| r.owner_pid = 0);
        // Not reapable: owner alive.
        publish_at(tmp.path(), "live-owner", |_| {});

        assert!(matches!(
            protection_status(data_path, "expired-orphan"),
            ProtectionStatus::Reapable(_)
        ));
        assert!(matches!(
            protection_status(data_path, "fresh-orphan"),
            ProtectionStatus::Protected { .. }
        ));
        assert!(matches!(
            protection_status(data_path, "live-owner"),
            ProtectionStatus::Protected { .. }
        ));
    }

    #[test]
    fn test_ttl_is_configurable_and_generous_by_default() {
        // 6h was measurably too tight: upload alone took 4h17m in production, and in server
        // mode the clock runs from create-end because the record is never re-adopted.
        // const block: the value is compile-time known, and clippy rejects a plain assert!.
        const { assert!(DEFAULT_TTL_SECS >= 12 * 60 * 60) };
        // Honoured per-record, not read from the constant at check time.
        let rec = DeferredFreezeRecord::new("bk", vec![], 42);
        assert_eq!(rec.ttl_secs, 42);
    }

    #[test]
    fn test_delete_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        delete(dir.path()).unwrap();
        let rec = DeferredFreezeRecord::new("bk-1", vec![], DEFAULT_TTL_SECS);
        publish(dir.path(), &rec).unwrap();
        delete(dir.path()).unwrap();
        assert!(!record_path(dir.path()).exists());
        delete(dir.path()).unwrap();
    }

    #[test]
    fn test_retain_failed_rewrites_rather_than_deleting() {
        let dir = tempfile::tempdir().unwrap();
        let rec = DeferredFreezeRecord::new(
            "bk-1",
            vec![info("db", "t1"), info("db", "t2")],
            DEFAULT_TTL_SECS,
        );
        publish(dir.path(), &rec).unwrap();

        retain_failed(dir.path(), "bk-1", vec![info("db", "t2")], &rec);

        match load(dir.path(), "bk-1") {
            LoadOutcome::Usable(loaded) => {
                assert_eq!(loaded.retained.len(), 1);
                assert_eq!(loaded.retained[0].table, "t2");
            }
            other => panic!("expected Usable, got {other:?}"),
        }
    }

    // -- TTL and ownership bookkeeping across a partial-unfreeze rewrite --

    #[test]
    fn test_retain_failed_preserves_configured_ttl_and_timestamp() {
        // `retain_failed` used to rebuild the record with the DEFAULT_TTL_SECS constant, so an
        // operator who raised the TTL because their uploads run long silently got 24h back --
        // reintroducing the very race the TTL exists to prevent. It must carry the value the
        // freeze was published under, and must not restart the clock either, or a daily retry
        // loop keeps the hold alive forever.
        let dir = tempfile::tempdir().unwrap();
        let custom_ttl = DEFAULT_TTL_SECS * 3;
        let mut rec =
            DeferredFreezeRecord::new("bk-1", vec![info("db", "t1"), info("db", "t2")], custom_ttl);
        rec.created_at_secs = now_secs() - 5_000;
        publish(dir.path(), &rec).unwrap();

        retain_failed(dir.path(), "bk-1", vec![info("db", "t2")], &rec);

        let path = record_path(dir.path());
        let rewritten = crate::resume::load_state_file::<DeferredFreezeRecord>(&path)
            .unwrap()
            .expect("record should still exist");
        assert_eq!(
            rewritten.ttl_secs, custom_ttl,
            "configured TTL must survive the rewrite"
        );
        assert_eq!(
            rewritten.created_at_secs, rec.created_at_secs,
            "the TTL clock must not restart on a partial failure"
        );
    }

    #[test]
    fn test_retain_failed_demotes_the_owner() {
        // A partial unfreeze ends the operation. If the record kept a live owner, a long-lived
        // server would protect it forever and the TTL branch would never be reached -- the
        // production wedge, reached by a different branch than a failed upload.
        let dir = tempfile::tempdir().unwrap();
        let rec = DeferredFreezeRecord::new("bk-1", vec![info("db", "t1")], DEFAULT_TTL_SECS);
        publish(dir.path(), &rec).unwrap();

        retain_failed(dir.path(), "bk-1", vec![info("db", "t1")], &rec);

        let rewritten =
            crate::resume::load_state_file::<DeferredFreezeRecord>(&record_path(dir.path()))
                .unwrap()
                .unwrap();
        assert_eq!(rewritten.owner_pid, 0, "owner must be cleared");
        assert!(rewritten.owner_boot_token.is_none());
        assert!(
            !rewritten.owner_is_live(),
            "a demoted record must not be protected by liveness"
        );
    }

    #[test]
    fn test_demoted_record_is_reapable_once_expired_despite_live_process() {
        // THE production case. On the affected cluster chbackup runs as a long-lived PID 1
        // server: the process that wrote the record is still running and its token still
        // matches, so token-aware liveness alone keeps the record Protected forever. Only
        // demotion lets the TTL apply. Without it the whole fix is inert in production.
        let dir = tempfile::tempdir().unwrap();
        let data_path = dir.path();
        let backup_dir = data_path.join("backup").join("bk-1");
        std::fs::create_dir_all(&backup_dir).unwrap();

        let mut rec = DeferredFreezeRecord::new("bk-1", vec![info("db", "t1")], 60);
        // Owner is this very process -- alive, token matching. Exactly the server-mode case.
        assert!(rec.owner_is_live() || rec.owner_boot_token.is_none());
        rec.created_at_secs = now_secs() - 600; // well past its 60s TTL
        publish(&backup_dir, &rec).unwrap();

        // Before demotion, liveness of the still-running owner protects it on any host that can
        // establish identity.
        if rec.owner_boot_token.is_some() {
            assert!(matches!(
                protection_status(data_path.to_str().unwrap(), "bk-1"),
                ProtectionStatus::Protected { .. }
            ));
        }

        demote_on_operation_end(&backup_dir, "bk-1");

        assert!(
            matches!(
                protection_status(data_path.to_str().unwrap(), "bk-1"),
                ProtectionStatus::Reapable(_)
            ),
            "an expired, demoted record must be reapable even though its writer is alive"
        );
    }

    #[test]
    fn test_successful_create_handoff_leaves_a_demoted_record() {
        // The gap Codex found: `create` publishes the record and succeeds, then a SEPARATE
        // `upload` command is meant to release it. In server mode the publishing process is
        // still alive with a matching token, so without demotion at end-of-create the record
        // is owner-live forever and the TTL branch is unreachable -- which is exactly how
        // records pile up when the upload is delayed, never runs, or queues behind a stall.
        let dir = tempfile::tempdir().unwrap();
        let data_path = dir.path();
        let backup_dir = data_path.join("backup").join("bk-1");
        std::fs::create_dir_all(&backup_dir).unwrap();

        // As create leaves it: owned by this (live) process.
        let rec = DeferredFreezeRecord::new("bk-1", vec![info("db", "t1")], 60);
        publish(&backup_dir, &rec).unwrap();

        // End of a successful create.
        demote_on_operation_end(&backup_dir, "bk-1");

        // A later upload must still be able to take it: unowned + not live => adopt.
        match load(&backup_dir, "bk-1") {
            LoadOutcome::Usable(adopted) => {
                assert_eq!(adopted.owner_pid, std::process::id(), "upload re-owns it");
                assert_eq!(adopted.retained.len(), 1, "the freeze is still recorded");
                assert_eq!(
                    adopted.created_at_secs, rec.created_at_secs,
                    "adoption must not restart the TTL clock"
                );
            }
            other => panic!("a demoted record must be adoptable by the upload, got {other:?}"),
        }
    }

    #[test]
    fn test_claiming_a_demoted_record_protects_it_past_its_ttl() {
        // The race Codex found in the create-demotion change: after create demotes, an upload
        // that is cancelled and detached holds neither the caller's PID lock nor owner
        // liveness, so an EXPIRED record could be reaped while CopyObject is still reading.
        // Claiming the record at upload start closes it -- liveness protects the copy even
        // once the TTL has passed, and the TTL still bounds the freeze if the process dies.
        let dir = tempfile::tempdir().unwrap();
        let data_path = dir.path();
        let backup_dir = data_path.join("backup").join("bk-1");
        std::fs::create_dir_all(&backup_dir).unwrap();

        let mut rec = DeferredFreezeRecord::new("bk-1", vec![info("db", "t1")], 60);
        rec.created_at_secs = now_secs() - 600; // already past its TTL
        publish(&backup_dir, &rec).unwrap();
        demote_on_operation_end(&backup_dir, "bk-1");

        // Unclaimed and expired: reapable, which is what makes the detached copy unsafe.
        assert!(matches!(
            protection_status(data_path.to_str().unwrap(), "bk-1"),
            ProtectionStatus::Reapable(_)
        ));

        // Upload claims it (this is exactly what `load` does at upload start).
        assert!(matches!(load(&backup_dir, "bk-1"), LoadOutcome::Usable(_)));

        // Now owner-live, so it survives its TTL for as long as the upload runs. Only
        // meaningful where process identity can be established.
        if crate::lock::process_start_token(std::process::id()).is_some() {
            assert!(
                matches!(
                    protection_status(data_path.to_str().unwrap(), "bk-1"),
                    ProtectionStatus::Protected { .. }
                ),
                "a claimed record must not be reapable while its upload is still copying"
            );
        }

        // And the claim must not have pushed the deadline out.
        let after =
            crate::resume::load_state_file::<DeferredFreezeRecord>(&record_path(&backup_dir))
                .unwrap()
                .unwrap();
        assert_eq!(
            after.created_at_secs, rec.created_at_secs,
            "claiming must not restart the TTL clock"
        );
    }

    #[test]
    fn test_demoted_create_record_expires_if_no_upload_ever_runs() {
        let dir = tempfile::tempdir().unwrap();
        let data_path = dir.path();
        let backup_dir = data_path.join("backup").join("bk-1");
        std::fs::create_dir_all(&backup_dir).unwrap();

        let mut rec = DeferredFreezeRecord::new("bk-1", vec![info("db", "t1")], 60);
        rec.created_at_secs = now_secs() - 600; // create finished long ago
        publish(&backup_dir, &rec).unwrap();
        demote_on_operation_end(&backup_dir, "bk-1");

        assert!(
            matches!(
                protection_status(data_path.to_str().unwrap(), "bk-1"),
                ProtectionStatus::Reapable(_)
            ),
            "an abandoned create->upload handoff must become reapable once its TTL passes"
        );
    }

    #[test]
    fn test_demotion_does_not_extend_the_hold() {
        let dir = tempfile::tempdir().unwrap();
        let mut rec = DeferredFreezeRecord::new("bk-1", vec![info("db", "t1")], 60);
        let published_at = now_secs() - 600;
        rec.created_at_secs = published_at;
        publish(dir.path(), &rec).unwrap();

        // Two successive failures must not push the expiry out.
        demote_on_operation_end(dir.path(), "bk-1");
        demote_on_operation_end(dir.path(), "bk-1");

        let after =
            crate::resume::load_state_file::<DeferredFreezeRecord>(&record_path(dir.path()))
                .unwrap()
                .unwrap();
        assert_eq!(after.created_at_secs, published_at);
        assert!(after.is_expired());
    }

    #[test]
    fn test_adopt_preserves_created_at() {
        // Adoption refreshes the owner but must not restart the TTL: otherwise N pod restarts
        // grant N x TTL and the hold is unbounded again by a slower route. Protection during
        // the adopting operation comes from the per-backup lock it holds, not a fresh TTL.
        let mut rec = DeferredFreezeRecord::new("bk-1", vec![info("db", "t1")], 60);
        let published_at = now_secs() - 600;
        rec.created_at_secs = published_at;
        rec.owner_pid = 4_000_000; // a PID that is not alive
        rec.owner_boot_token = Some("stale-token".to_string());

        rec.adopt();

        assert_eq!(rec.owner_pid, std::process::id());
        assert_eq!(
            rec.created_at_secs, published_at,
            "adoption must not restart the TTL"
        );
        assert!(rec.is_expired());
    }

    #[test]
    fn test_absent_token_is_not_live_so_ttl_governs() {
        // A record written before `owner_boot_token` existed. Falling back to PID-only liveness
        // here is what kept the records on the affected cluster wedged: with a live PID 1,
        // `protection_status` answers Protected on liveness and never reaches the TTL branch.
        let mut rec = DeferredFreezeRecord::new("bk-1", vec![info("db", "t1")], 60);
        rec.owner_pid = std::process::id(); // alive
        rec.owner_boot_token = None; // identity unknowable
        assert!(
            !rec.owner_is_live(),
            "an unidentifiable owner must not protect the record"
        );

        let dir = tempfile::tempdir().unwrap();
        let backup_dir = dir.path().join("backup").join("bk-1");
        std::fs::create_dir_all(&backup_dir).unwrap();

        rec.created_at_secs = now_secs(); // unexpired: TTL still protects
        publish(&backup_dir, &rec).unwrap();
        assert!(matches!(
            protection_status(dir.path().to_str().unwrap(), "bk-1"),
            ProtectionStatus::Protected { .. }
        ));

        rec.created_at_secs = now_secs() - 600; // expired: now reapable
        publish(&backup_dir, &rec).unwrap();
        assert!(matches!(
            protection_status(dir.path().to_str().unwrap(), "bk-1"),
            ProtectionStatus::Reapable(_)
        ));
    }

    #[test]
    fn test_absent_token_defeats_ownership_claim() {
        // `load()` checks ownership BEFORE liveness, and bare PID equality let any fresh PID 1
        // claim a dead pod's record, skipping both the liveness check and the TTL.
        let mut rec = DeferredFreezeRecord::new("bk-1", vec![info("db", "t1")], 60);
        rec.owner_pid = std::process::id();
        rec.owner_boot_token = None;
        assert!(
            !rec.owned_by_current_process(),
            "ownership must not be inferred from a PID number alone"
        );
    }

    #[test]
    fn test_mismatched_token_defeats_ownership_and_liveness() {
        let mut rec = DeferredFreezeRecord::new("bk-1", vec![info("db", "t1")], 60);
        rec.owner_pid = std::process::id();
        rec.owner_boot_token = Some("definitely-not-this-processes-start-time".to_string());
        assert!(!rec.owned_by_current_process());
        // Only meaningful where a current token can actually be read.
        if crate::lock::process_start_token(std::process::id()).is_some() {
            assert!(!rec.owner_is_live());
        }
    }
}
