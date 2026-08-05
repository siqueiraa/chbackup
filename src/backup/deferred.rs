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
use tracing::{info, warn};

use super::freeze::FreezeInfo;

/// File name of the record, stored beside `metadata.json` in the backup directory.
const RECORD_FILE: &str = "deferred_freeze.json";

/// Default maximum time a deferred freeze may be held before the owning upload must give
/// up and release it. Holding a freeze pins obsolete S3 objects while merges continue, so
/// the hold has to be bounded.
pub const DEFAULT_TTL_SECS: u64 = 6 * 60 * 60;

/// A freeze intentionally held past the end of `create`, awaiting object upload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeferredFreezeRecord {
    /// Backup this record belongs to. Cross-checked on load.
    pub backup_name: String,

    /// Tables left frozen, with the exact names needed to UNFREEZE them.
    pub retained: Vec<FreezeInfo>,

    /// PID of the process that owns the freeze. Liveness of this PID decides whether the
    /// record is active or orphaned.
    pub owner_pid: u32,

    /// Unix seconds when the record was published.
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
        Self {
            backup_name: backup_name.to_string(),
            retained,
            owner_pid: std::process::id(),
            created_at_secs: now_secs(),
            ttl_secs,
        }
    }

    /// Whether the owning process is still running.
    ///
    /// A dead owner means the record is orphaned and its freeze can be reclaimed; a live
    /// owner means the freeze is in use and must not be touched from outside.
    pub fn owner_is_live(&self) -> bool {
        crate::lock::is_pid_alive(self.owner_pid)
    }

    /// Whether the hold has exceeded its TTL.
    ///
    /// Note this does **not** license outside deletion: an overdue but owner-live record is
    /// still protected. The TTL's job is to make the owning upload stop and release.
    pub fn is_expired(&self) -> bool {
        now_secs().saturating_sub(self.created_at_secs) > self.ttl_secs
    }

    /// Whether this record is owned by the calling process.
    pub fn owned_by_current_process(&self) -> bool {
        self.owner_pid == std::process::id()
    }

    /// Take ownership, for adoption of an orphaned-but-valid record.
    pub fn adopt(&mut self) {
        self.owner_pid = std::process::id();
        self.created_at_secs = now_secs();
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

/// Rewrite the record with only the entries that are still frozen.
///
/// Used when an unfreeze partially fails: the record must not be deleted while a table
/// remains frozen, or the leak becomes invisible.
pub fn retain_failed(backup_dir: &Path, backup_name: &str, failed: Vec<FreezeInfo>) {
    let record = DeferredFreezeRecord::new(backup_name, failed, DEFAULT_TTL_SECS);
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
pub fn protection_status(data_path: &str, backup_name: &str) -> ProtectionStatus {
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
    if backup_lock_is_active(backup_name) {
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
                 released first (run `chbackup clean`, or it is reaped at the next `create`)"
            );
            true
        }
    }
}

/// Whether some operation currently holds this backup's PID lock.
fn backup_lock_is_active(backup_name: &str) -> bool {
    let scope = crate::lock::LockScope::Backup(backup_name.to_string());
    match crate::lock::lock_path_for_scope(&scope) {
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
                assert!(loaded.owned_by_current_process());
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
                assert!(loaded.owned_by_current_process(), "should have adopted");
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

        retain_failed(dir.path(), "bk-1", vec![info("db", "t2")]);

        match load(dir.path(), "bk-1") {
            LoadOutcome::Usable(loaded) => {
                assert_eq!(loaded.retained.len(), 1);
                assert_eq!(loaded.retained[0].table, "t2");
            }
            other => panic!("expected Usable, got {other:?}"),
        }
    }
}
