use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::error::ChBackupError;

/// Contents written to the PID lock file as JSON.
#[derive(Debug, Serialize, Deserialize)]
struct LockInfo {
    pid: u32,
    command: String,
    timestamp: String,
}

/// A PID-based lock file.
///
/// On [`acquire`](PidLock::acquire), creates a JSON lock file containing the
/// current PID, command name, and ISO-8601 timestamp.  If a lock file already
/// exists, the recorded PID is checked: if the process is still alive the call
/// returns [`ChBackupError::LockError`]; if the process is dead the stale lock
/// is overridden.
///
/// The lock file is removed when the `PidLock` is dropped.
#[derive(Debug)]
pub struct PidLock {
    path: PathBuf,
}

impl PidLock {
    /// Acquire a PID lock at `path` for the given `command`.
    ///
    /// Returns `Ok(PidLock)` on success or `Err(ChBackupError::LockError)` if
    /// another live process already holds the lock.
    pub fn acquire(path: &Path, command: &str) -> Result<Self, ChBackupError> {
        let info = LockInfo {
            pid: std::process::id(),
            command: command.to_string(),
            timestamp: Utc::now().to_rfc3339(),
        };

        let json = serde_json::to_string_pretty(&info)
            .map_err(|e| ChBackupError::LockError(format!("failed to serialize lock info: {e}")))?;

        // Ensure parent directory exists.
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        // Attempt atomic file creation via O_CREAT|O_EXCL (create_new).
        // This eliminates the TOCTOU race between exists() and write().
        match OpenOptions::new().write(true).create_new(true).open(path) {
            Ok(mut file) => {
                file.write_all(json.as_bytes())?;
                Ok(PidLock {
                    path: path.to_path_buf(),
                })
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                // File exists -- check whether the recorded PID is alive.
                match fs::read_to_string(path) {
                    Ok(contents) => {
                        if let Ok(existing) = serde_json::from_str::<LockInfo>(&contents) {
                            if is_pid_alive(existing.pid) {
                                return Err(ChBackupError::LockError(format!(
                                    "lock held by PID {} (command: {}, since: {})",
                                    existing.pid, existing.command, existing.timestamp,
                                )));
                            }
                            // PID is dead -- stale lock, remove and retry.
                        }
                        // Malformed JSON -- treat as stale, remove and retry.
                    }
                    Err(_) => {
                        // Cannot read file -- treat as stale, remove and retry.
                    }
                }

                // Remove stale lock and retry with create_new for atomicity.
                let _ = fs::remove_file(path);
                let mut file = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(path)
                    .map_err(|e| {
                        ChBackupError::LockError(format!(
                            "failed to acquire lock after removing stale file: {e}"
                        ))
                    })?;
                file.write_all(json.as_bytes())?;
                Ok(PidLock {
                    path: path.to_path_buf(),
                })
            }
            Err(e) => Err(ChBackupError::LockError(format!(
                "failed to create lock file: {e}"
            ))),
        }
    }

    /// Return the path to the lock file.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for PidLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// Return `true` if `path` names an existing PID lock file that is owned by a
/// live process.
///
/// Returns `false` on any I/O or parse error, and on non-Unix platforms.
/// Safe to call concurrently; reads are atomic at the OS level for small files.
pub fn is_lock_file_active(path: &Path) -> bool {
    let content = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(_) => return false,
    };
    let info: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return false,
    };
    let pid = match info.get("pid").and_then(|v| v.as_u64()) {
        Some(p) => p as u32,
        None => return false,
    };
    is_pid_alive(pid)
}

// ---------------------------------------------------------------------------
// Lock scope
// ---------------------------------------------------------------------------

/// Three-tier lock scope from design doc section 2.
///
/// - `Backup(name)` -- per-backup lock (`/tmp/chbackup.{name}.pid`)
/// - `Global` -- global lock (`/tmp/chbackup.global.pid`)
/// - `None` -- no lock required (read-only commands)
///
/// The two locking tiers are mutually exclusive, and [`acquire_scoped`] is the
/// only place that rule is enforced:
///
/// - a `Global` acquisition fails while ANY per-backup lock is live, because
///   destructive admin commands (`clean`, `clean_broken`, `delete`) may delete
///   data another backup is still writing;
/// - a `Backup(name)` acquisition fails while the global lock is live;
/// - a `Backup(name)` acquisition fails while the same name is already held.
///
/// Locks whose recorded PID is dead are stale and never block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LockScope {
    /// Per-backup lock for mutating backup commands.
    Backup(String),
    /// Global lock for destructive admin commands.
    Global,
    /// No lock needed for read-only commands.
    None,
}

/// Determine the lock scope for a given CLI command.
///
/// Mapping per design doc section 2:
/// - Backup-scoped: create, upload, download, restore, create_remote, restore_remote
/// - Global: clean, clean_broken, delete
/// - None: list, tables, default-config, print-config, watch, server
pub fn lock_for_command(command: &str, backup_name: Option<&str>) -> LockScope {
    match command {
        "create" | "upload" | "download" | "restore" | "create_remote" | "restore_remote" => {
            match backup_name {
                Some(name) if !name.is_empty() => LockScope::Backup(name.to_string()),
                _ => LockScope::Global,
            }
        }
        // API routes use "clean_broken_remote" / "clean_broken_local" as the
        // command name; they must also acquire the global lock.
        "clean" | "clean_broken" | "clean_broken_remote" | "clean_broken_local" | "delete" => {
            LockScope::Global
        }
        // list, tables, default-config, print-config, watch, server
        _ => LockScope::None,
    }
}

/// Filename prefix shared by every scope lock file.
const LOCK_PREFIX: &str = "chbackup.";
/// Filename suffix shared by every scope lock file.
const LOCK_SUFFIX: &str = ".pid";
/// Scope-lock filename stem of the global tier.
const GLOBAL_STEM: &str = "global";
/// Filename of the short-lived gate that serialises scope acquisitions.
///
/// Deliberately not a `chbackup.*.pid` name: the scan in
/// [`check_cross_tier_exclusion`] (and `list::active_freeze_prefixes`) would
/// otherwise read the gate as a per-backup lock for a backup named "acquire".
const GATE_LOCK_FILE: &str = "chbackup.acquire.lock";

/// Attempts to take the gate before giving up, and the retry backoff bounds.
/// The gate is held only for a handful of filesystem calls, so contention is
/// resolved in the first retry or two; the cap exists so a pathological case
/// fails with a `LockError` instead of hanging.
const GATE_MAX_ATTEMPTS: u32 = 40;
const GATE_RETRY_DELAY: Duration = Duration::from_millis(5);
const GATE_MAX_RETRY_DELAY: Duration = Duration::from_millis(100);

/// The lock directory used in production; tests pass a `TempDir` instead.
pub fn default_lock_dir() -> &'static Path {
    Path::new("/tmp")
}

/// Resolve a [`LockScope`] to an optional lock file path under `lock_dir`.
///
/// Returns `None` for `LockScope::None`.
pub fn lock_path_for_scope(lock_dir: &Path, scope: &LockScope) -> Option<PathBuf> {
    let stem = match scope {
        LockScope::Backup(name) => name.as_str(),
        LockScope::Global => GLOBAL_STEM,
        LockScope::None => return None,
    };
    Some(lock_dir.join(format!("{LOCK_PREFIX}{stem}{LOCK_SUFFIX}")))
}

/// Acquire the lock for `scope` under `lock_dir`, enforcing mutual exclusion
/// between the global and per-backup tiers (see [`LockScope`]).
///
/// Exclusion cannot be established by scanning for conflicts and then creating
/// the lock file: two processes would both scan, both see nothing, and both
/// acquire. So the scan and the create both happen while holding a gate lock,
/// which makes the pair atomic against every other process using the same
/// `lock_dir`. The gate is released before this function returns, so it never
/// serialises the long-running operations themselves -- only their acquisition.
///
/// Returns [`ChBackupError::LockError`] naming the conflicting holder, which
/// maps to CLI exit code 4 and HTTP 423. Callers must skip `LockScope::None`
/// themselves; it has no lock file.
pub fn acquire_scoped(
    lock_dir: &Path,
    scope: &LockScope,
    command: &str,
) -> Result<PidLock, ChBackupError> {
    let path = lock_path_for_scope(lock_dir, scope).ok_or_else(|| {
        ChBackupError::LockError("LockScope::None has no lock file to acquire".to_string())
    })?;

    let _gate = acquire_gate(lock_dir, command)?;
    check_cross_tier_exclusion(lock_dir, scope)?;
    PidLock::acquire(&path, command)
}

/// Take the gate lock, retrying with exponential backoff while another process
/// holds it.
///
/// The gate is an ordinary [`PidLock`], so a gate left behind by a crashed
/// process is stale-takeover-eligible and cannot wedge the tool.
fn acquire_gate(lock_dir: &Path, command: &str) -> Result<PidLock, ChBackupError> {
    let gate_path = lock_dir.join(GATE_LOCK_FILE);
    let mut delay = GATE_RETRY_DELAY;
    let mut attempt = 1;
    loop {
        match PidLock::acquire(&gate_path, command) {
            Ok(gate) => return Ok(gate),
            Err(e) if attempt >= GATE_MAX_ATTEMPTS => {
                return Err(ChBackupError::LockError(format!(
                    "gave up waiting for the acquisition gate {}: {e}",
                    gate_path.display()
                )));
            }
            Err(_) => {
                std::thread::sleep(delay);
                delay = (delay * 2).min(GATE_MAX_RETRY_DELAY);
                attempt += 1;
            }
        }
    }
}

/// Reject an acquisition that conflicts with a live lock in the other tier.
///
/// The caller must hold the gate; without it this scan is a TOCTOU check.
/// Same-tier conflicts are left to [`PidLock::acquire`], whose `O_EXCL` create
/// is already atomic.
fn check_cross_tier_exclusion(lock_dir: &Path, scope: &LockScope) -> Result<(), ChBackupError> {
    match scope {
        LockScope::Global => {
            let entries = fs::read_dir(lock_dir).map_err(|e| {
                ChBackupError::LockError(format!(
                    "failed to scan lock directory {}: {e}",
                    lock_dir.display()
                ))
            })?;
            for entry in entries.flatten() {
                let file_name = entry.file_name();
                let Some(backup) = file_name.to_str().and_then(backup_name_from_lock_file) else {
                    continue;
                };
                if let Some(holder) = live_lock_holder(&entry.path()) {
                    return Err(ChBackupError::LockError(format!(
                        "global lock blocked by backup '{backup}' held by {holder}"
                    )));
                }
            }
            Ok(())
        }
        LockScope::Backup(name) => {
            let global = lock_dir.join(format!("{LOCK_PREFIX}{GLOBAL_STEM}{LOCK_SUFFIX}"));
            match live_lock_holder(&global) {
                Some(holder) => Err(ChBackupError::LockError(format!(
                    "lock for backup '{name}' blocked by the global lock held by {holder}"
                ))),
                None => Ok(()),
            }
        }
        LockScope::None => Ok(()),
    }
}

/// Extract the backup name from a per-backup lock filename, or `None` if the
/// name belongs to another tier or is not a scope lock at all.
fn backup_name_from_lock_file(file_name: &str) -> Option<&str> {
    let stem = file_name
        .strip_prefix(LOCK_PREFIX)?
        .strip_suffix(LOCK_SUFFIX)?;
    (!stem.is_empty() && stem != GLOBAL_STEM).then_some(stem)
}

/// Describe the live holder of the lock file at `path`, or `None` if the file
/// is missing, unreadable, malformed, or owned by a dead PID.
fn live_lock_holder(path: &Path) -> Option<String> {
    let contents = fs::read_to_string(path).ok()?;
    let info: LockInfo = serde_json::from_str(&contents).ok()?;
    is_pid_alive(info.pid).then(|| {
        format!(
            "PID {} (command: {}, since: {})",
            info.pid, info.command, info.timestamp
        )
    })
}

// ---------------------------------------------------------------------------
// Platform-specific PID liveness check
// ---------------------------------------------------------------------------

/// Check if a process with the given PID is alive.
///
/// Uses `kill(pid, 0)` on Unix.  Returns `false` on any error or on
/// non-Unix platforms.
///
/// POSIX semantics:
/// - `ret == 0`          → process exists and we can signal it → alive
/// - `ret == -1, ESRCH`  → no such process → dead
/// - `ret == -1, EPERM`  → process exists but permission denied → alive
///   (can happen in containers with security contexts or cross-uid scenarios)
pub fn is_pid_alive(pid: u32) -> bool {
    // PID 0 is never a real user process, but `kill(0, 0)` means "signal every process in
    // the caller's process group" and therefore SUCCEEDS -- which would report a zeroed or
    // corrupt PID as alive. Treat it as dead so bad data cannot pin a lock or a deferred
    // freeze indefinitely.
    if pid == 0 {
        return false;
    }

    #[cfg(unix)]
    {
        // SAFETY: signal 0 does not send a signal; it only checks that the
        // process exists and we have permission to signal it.
        let ret = unsafe { libc::kill(pid as libc::pid_t, 0) };
        if ret == 0 {
            return true;
        }
        // EPERM means the process exists but we lack permission to signal it.
        // Treat as alive so we never steal a live process's lock.
        let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
        errno == libc::EPERM
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn lock_path(dir: &TempDir) -> PathBuf {
        dir.path().join("test.pid")
    }

    #[test]
    fn test_acquire_release() {
        let dir = TempDir::new().unwrap();
        let path = lock_path(&dir);

        {
            let lock = PidLock::acquire(&path, "create").unwrap();
            assert!(path.exists(), "lock file should exist after acquire");

            // Verify lock file contents.
            let contents = fs::read_to_string(lock.path()).unwrap();
            let info: LockInfo = serde_json::from_str(&contents).unwrap();
            assert_eq!(info.pid, std::process::id());
            assert_eq!(info.command, "create");
        }
        // PidLock dropped here -- lock file should be removed.
        assert!(!path.exists(), "lock file should be removed after drop");
    }

    #[test]
    fn test_double_acquire_fails() {
        let dir = TempDir::new().unwrap();
        let path = lock_path(&dir);

        let _lock1 = PidLock::acquire(&path, "upload").unwrap();
        let result = PidLock::acquire(&path, "download");

        assert!(result.is_err(), "second acquire on same path should fail");
        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("lock held by PID"),
            "error should mention PID: {msg}"
        );
    }

    #[test]
    fn test_stale_lock_overridden() {
        let dir = TempDir::new().unwrap();
        let path = lock_path(&dir);

        // Write a lock file with a PID that certainly does not exist.
        // PID 4_000_000 is extremely unlikely to be alive on any real system
        // (Linux pid_max default is 32768, macOS typically 99998).
        let stale_info = LockInfo {
            pid: 4_000_000,
            command: "stale".to_string(),
            timestamp: "2020-01-01T00:00:00Z".to_string(),
        };
        fs::write(&path, serde_json::to_string(&stale_info).unwrap()).unwrap();

        // Acquiring should succeed because the old PID is dead.
        let lock = PidLock::acquire(&path, "restore").unwrap();
        let contents = fs::read_to_string(lock.path()).unwrap();
        let info: LockInfo = serde_json::from_str(&contents).unwrap();
        assert_eq!(info.pid, std::process::id());
        assert_eq!(info.command, "restore");
    }

    #[test]
    fn test_acquire_atomic_creation() {
        // Verify that acquire uses atomic file creation (create_new / O_CREAT|O_EXCL).
        // After a successful acquire, the lock file should contain valid JSON with
        // the current PID, proving that the atomic path was used.
        let dir = TempDir::new().unwrap();
        let path = lock_path(&dir);

        // Ensure no file exists before acquire.
        assert!(!path.exists(), "lock file should not exist before acquire");

        let lock = PidLock::acquire(&path, "test_atomic").unwrap();

        // Verify the file was created atomically (contents are valid).
        let contents = fs::read_to_string(lock.path()).unwrap();
        let info: LockInfo = serde_json::from_str(&contents).unwrap();
        assert_eq!(info.pid, std::process::id());
        assert_eq!(info.command, "test_atomic");

        // Verify that a second concurrent acquire attempt is rejected
        // (the atomic creation ensures no window for race conditions).
        let result = PidLock::acquire(&path, "concurrent");
        assert!(
            result.is_err(),
            "concurrent acquire should fail due to atomic lock"
        );
    }

    #[test]
    fn test_lock_for_command_mapping() {
        // Backup-scoped commands
        assert_eq!(
            lock_for_command("create", Some("daily-20250215")),
            LockScope::Backup("daily-20250215".to_string())
        );
        assert_eq!(
            lock_for_command("upload", Some("my-backup")),
            LockScope::Backup("my-backup".to_string())
        );
        assert_eq!(
            lock_for_command("restore", Some("bak")),
            LockScope::Backup("bak".to_string())
        );

        // Backup-scoped without a name falls back to Global
        assert_eq!(lock_for_command("create", None), LockScope::Global);

        // Global commands
        assert_eq!(lock_for_command("clean", None), LockScope::Global);
        assert_eq!(lock_for_command("delete", None), LockScope::Global);
        assert_eq!(lock_for_command("clean_broken", None), LockScope::Global);

        // No-lock commands
        assert_eq!(lock_for_command("list", None), LockScope::None);
        assert_eq!(lock_for_command("tables", None), LockScope::None);
        assert_eq!(lock_for_command("default-config", None), LockScope::None);
        assert_eq!(lock_for_command("print-config", None), LockScope::None);
        assert_eq!(lock_for_command("watch", None), LockScope::None);
        assert_eq!(lock_for_command("server", None), LockScope::None);
    }

    /// A PID that is not alive on any realistic system (Linux `pid_max`
    /// defaults to 32768, macOS to 99998).
    const DEAD_PID: u32 = 4_000_000;

    /// Write a lock file as if another process had created it.
    fn write_foreign_lock(path: &Path, pid: u32) {
        let info = LockInfo {
            pid,
            command: "foreign".to_string(),
            timestamp: "2020-01-01T00:00:00Z".to_string(),
        };
        fs::write(path, serde_json::to_string(&info).unwrap()).unwrap();
    }

    fn backup_scope(name: &str) -> LockScope {
        LockScope::Backup(name.to_string())
    }

    #[test]
    fn lock_cross_tier_global_blocked_by_live_backup() {
        let dir = TempDir::new().unwrap();
        let _backup = acquire_scoped(dir.path(), &backup_scope("daily"), "upload").unwrap();

        let err = acquire_scoped(dir.path(), &LockScope::Global, "clean_broken").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("daily"), "error should name the backup: {msg}");
        assert!(
            !lock_path_for_scope(dir.path(), &LockScope::Global)
                .unwrap()
                .exists(),
            "a rejected global acquisition must not leave a lock file behind"
        );
    }

    #[test]
    fn lock_cross_tier_backup_blocked_by_live_global() {
        let dir = TempDir::new().unwrap();
        let _global = acquire_scoped(dir.path(), &LockScope::Global, "clean_broken").unwrap();

        let err = acquire_scoped(dir.path(), &backup_scope("daily"), "upload").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("global lock"),
            "error should name the global lock: {msg}"
        );
    }

    #[test]
    fn lock_cross_tier_stale_backup_does_not_block_global() {
        let dir = TempDir::new().unwrap();
        write_foreign_lock(
            &lock_path_for_scope(dir.path(), &backup_scope("crashed")).unwrap(),
            DEAD_PID,
        );

        let lock = acquire_scoped(dir.path(), &LockScope::Global, "clean_broken").unwrap();
        assert!(lock.path().exists());
    }

    #[test]
    fn lock_cross_tier_stale_global_does_not_block_backup() {
        let dir = TempDir::new().unwrap();
        write_foreign_lock(
            &lock_path_for_scope(dir.path(), &LockScope::Global).unwrap(),
            DEAD_PID,
        );

        let lock = acquire_scoped(dir.path(), &backup_scope("daily"), "upload").unwrap();
        assert!(lock.path().exists());
    }

    #[test]
    fn lock_cross_tier_same_backup_name_still_blocks() {
        let dir = TempDir::new().unwrap();
        let _first = acquire_scoped(dir.path(), &backup_scope("daily"), "upload").unwrap();

        let err = acquire_scoped(dir.path(), &backup_scope("daily"), "download").unwrap_err();
        assert!(
            err.to_string().contains("lock held by PID"),
            "same-name conflict should still be reported: {err}"
        );
    }

    #[test]
    fn lock_gate_stale_recovery() {
        let dir = TempDir::new().unwrap();
        let gate_path = dir.path().join(GATE_LOCK_FILE);
        // A gate left behind by a crashed acquirer must not wedge the tool.
        write_foreign_lock(&gate_path, DEAD_PID);

        let lock = acquire_scoped(dir.path(), &backup_scope("daily"), "create").unwrap();

        assert!(lock.path().exists());
        assert!(
            !gate_path.exists(),
            "the gate must be released before the operation begins"
        );
    }

    /// Hammer both tiers from many threads and assert the invariant that a
    /// global holder and a per-backup holder never coexist. Per-direction
    /// sequential tests cannot catch an interleaving of scan and create.
    #[test]
    fn lock_cross_tier_mutual_exclusion_concurrent() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        let dir = TempDir::new().unwrap();
        let lock_dir = dir.path().to_path_buf();
        let globals_held = Arc::new(AtomicUsize::new(0));
        let backups_held = Arc::new(AtomicUsize::new(0));
        let violations = Arc::new(AtomicUsize::new(0));

        let threads: Vec<_> = (0..8)
            .map(|t| {
                let lock_dir = lock_dir.clone();
                let globals_held = Arc::clone(&globals_held);
                let backups_held = Arc::clone(&backups_held);
                let violations = Arc::clone(&violations);
                std::thread::spawn(move || {
                    let global = t % 2 == 0;
                    let scope = if global {
                        LockScope::Global
                    } else {
                        backup_scope(&format!("backup-{t}"))
                    };
                    for _ in 0..25 {
                        // A rejected acquisition is a legitimate outcome here;
                        // only a granted one can violate the invariant.
                        let Ok(lock) = acquire_scoped(&lock_dir, &scope, "concurrent") else {
                            continue;
                        };
                        let (mine, theirs) = if global {
                            (&globals_held, &backups_held)
                        } else {
                            (&backups_held, &globals_held)
                        };
                        mine.fetch_add(1, Ordering::SeqCst);
                        // With SeqCst, if two holders overlap at least one of
                        // them observes the other's increment.
                        if theirs.load(Ordering::SeqCst) > 0 {
                            violations.fetch_add(1, Ordering::SeqCst);
                        }
                        std::thread::yield_now();
                        mine.fetch_sub(1, Ordering::SeqCst);
                        drop(lock);
                    }
                })
            })
            .collect();

        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(
            violations.load(Ordering::SeqCst),
            0,
            "a global holder and a per-backup holder must never coexist"
        );
    }
}
