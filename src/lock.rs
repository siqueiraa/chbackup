use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::error::ChBackupError;

/// Contents written to the PID lock file as JSON.
#[derive(Debug, Serialize, Deserialize)]
struct LockInfo {
    pid: u32,
    command: String,
    timestamp: String,
    /// Start-time token of the holding process, pairing with `pid` to form an identity.
    ///
    /// `#[serde(default)]` is required, not cosmetic: without it a lock file written before this
    /// field existed fails to deserialise and [`inspect_lock_file`] reports it as *corrupt*.
    ///
    /// When absent, liveness falls back to PID alone and the lock is treated as **held** while
    /// that PID exists. That is the opposite direction from a deferred-freeze record's absent
    /// token, and deliberately so: a record has the TTL as a backstop and the hazard is holding
    /// forever, whereas a lock has no backstop and the hazard is *stealing* a live operation's
    /// lock and running two writers over one backup. Transitional either way — locks are written
    /// per operation and removed on drop, so one operation cycle after an upgrade every live lock
    /// carries a token.
    #[serde(default)]
    token: Option<String>,
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

/// Times an unparseable lock file is re-read before it is judged corrupt, and the pause
/// between those reads. Bounded on purpose: a genuinely corrupt file must not wedge the tool.
const CORRUPT_LOCK_MAX_READS: u32 = 3;
const CORRUPT_LOCK_RETRY_DELAY: Duration = Duration::from_millis(25);
/// Publish attempts before giving up. Every non-final outcome removes or waits out the
/// blocking file, so exhausting this means one keeps reappearing.
const ACQUIRE_MAX_ATTEMPTS: u32 = 6;

/// Distinguishes serial numbers of concurrently published lock files within this process.
static TEMP_LOCK_SEQ: AtomicU64 = AtomicU64::new(0);

impl PidLock {
    /// Acquire a PID lock at `path` for the given `command`.
    ///
    /// Returns `Ok(PidLock)` on success or `Err(ChBackupError::LockError)` if
    /// another live process already holds the lock.
    pub fn acquire(path: &Path, command: &str) -> Result<Self, ChBackupError> {
        let pid = std::process::id();
        let info = LockInfo {
            pid,
            command: command.to_string(),
            timestamp: Utc::now().to_rfc3339(),
            token: process_start_token(pid),
        };

        let json = serde_json::to_string_pretty(&info)
            .map_err(|e| ChBackupError::LockError(format!("failed to serialize lock info: {e}")))?;

        // Ensure parent directory exists.
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let mut corrupt_reads = 0;
        for _ in 0..ACQUIRE_MAX_ATTEMPTS {
            match publish_lock_file(path, &json) {
                Ok(()) => {
                    return Ok(PidLock {
                        path: path.to_path_buf(),
                    })
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => {
                    return Err(ChBackupError::LockError(format!(
                        "failed to create lock file: {e}"
                    )))
                }
            }

            match inspect_lock_file(path) {
                ExistingLock::Live(existing) => {
                    return Err(ChBackupError::LockError(format!(
                        "lock held by PID {} (command: {}, since: {})",
                        existing.pid, existing.command, existing.timestamp,
                    )))
                }
                // The recorded holder is dead: a stale lock never blocks.
                ExistingLock::Stale => {
                    let _ = fs::remove_file(path);
                }
                // Released while we were looking -- publish again straight away.
                ExistingLock::Gone => {}
                // Publication is atomic, so unparseable content can no longer be a racer's
                // half-written file; it means genuine corruption. Re-read a few times before
                // overriding, because deleting an unparseable lock on sight is exactly what
                // let two acquirers both believe they held the same lock.
                ExistingLock::Unreadable(_) if corrupt_reads < CORRUPT_LOCK_MAX_READS => {
                    corrupt_reads += 1;
                    std::thread::sleep(CORRUPT_LOCK_RETRY_DELAY);
                }
                ExistingLock::Unreadable(reason) => {
                    warn!(
                        path = %path.display(),
                        %reason,
                        "Removing corrupt lock file after repeated unreadable reads"
                    );
                    let _ = fs::remove_file(path);
                }
            }
        }

        Err(ChBackupError::LockError(format!(
            "failed to acquire lock {}: a conflicting lock file keeps reappearing",
            path.display()
        )))
    }

    /// Return the path to the lock file.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// What an already-existing lock file turned out to be.
#[derive(Debug)]
enum ExistingLock {
    /// Owned by a process that is still alive.
    Live(LockInfo),
    /// Owned by a dead process.
    Stale,
    /// Vanished between the failed publish and the read.
    Gone,
    /// Present but not parseable as a [`LockInfo`], with the reason.
    Unreadable(String),
}

/// Classify the lock file at `path`.
fn inspect_lock_file(path: &Path) -> ExistingLock {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return ExistingLock::Gone,
        Err(e) => return ExistingLock::Unreadable(e.to_string()),
    };
    match serde_json::from_str::<LockInfo>(&contents) {
        Ok(info) if holder_is_live(info.pid, info.token.as_deref()) => ExistingLock::Live(info),
        Ok(_) => ExistingLock::Stale,
        Err(e) => ExistingLock::Unreadable(e.to_string()),
    }
}

/// Create the lock file at `path` carrying `json`, failing with
/// [`ErrorKind::AlreadyExists`](std::io::ErrorKind::AlreadyExists) if it is already taken.
///
/// The payload is written to a uniquely named temp file in the same directory first, and only
/// the finished file is linked into place. Creating the lock empty and writing it afterwards
/// left a window in which every reader here — which judges unparseable content as "not a live
/// lock" — would delete a lock another process had just been granted.
///
/// The publish step is `link(2)` rather than `rename(2)`: rename would silently clobber a live
/// holder's lock, while link fails with `EEXIST`, keeping the `O_CREAT|O_EXCL` semantics that
/// make exactly one racer the winner.
fn publish_lock_file(path: &Path, json: &str) -> std::io::Result<()> {
    let tmp = temp_lock_path(path);
    let mut file = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
    let written = file.write_all(json.as_bytes());
    drop(file);
    if let Err(e) = written {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }

    let published = fs::hard_link(&tmp, path);
    let _ = fs::remove_file(&tmp);
    published
}

/// A sibling path for the temp file a lock is staged in.
///
/// Same directory, so the link is never cross-device, and deliberately not a
/// `chbackup.*.pid` name: [`check_cross_tier_exclusion`] would otherwise read it as a
/// per-backup lock.
fn temp_lock_path(path: &Path) -> PathBuf {
    let stem = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("chbackup.lock");
    let unique = format!(
        ".{stem}.{}.{}.tmp",
        std::process::id(),
        TEMP_LOCK_SEQ.fetch_add(1, Ordering::Relaxed)
    );
    match path.parent() {
        Some(dir) => dir.join(unique),
        None => PathBuf::from(unique),
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
    // Read as a raw value rather than `LockInfo` so a malformed-but-pid-bearing file keeps its
    // current "held" answer instead of becoming "not held".
    let token = info.get("token").and_then(|v| v.as_str());
    holder_is_live(pid, token)
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
/// - Backup-scoped: create, upload, download, restore, create_remote, restore_remote,
///   release-deferred
/// - Global: clean, clean_broken, delete
/// - None: list, tables, default-config, print-config, watch, server
///
/// Note the fall-through is `LockScope::None`, so a mutating command omitted from the lists above
/// silently runs with **no lock at all**. Add new mutating commands here deliberately.
pub fn lock_for_command(command: &str, backup_name: Option<&str>) -> LockScope {
    match command {
        "create" | "upload" | "download" | "restore" | "create_remote" | "restore_remote"
        | "release-deferred" => match backup_name {
            Some(name) if !name.is_empty() => LockScope::Backup(name.to_string()),
            _ => LockScope::Global,
        },
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
    holder_is_live(info.pid, info.token.as_deref()).then(|| {
        format!(
            "PID {} (command: {}, since: {})",
            info.pid, info.command, info.timestamp
        )
    })
}

// ---------------------------------------------------------------------------
// Platform-specific PID liveness check
// ---------------------------------------------------------------------------

/// Extract the process start time -- field 22 of `/proc/<pid>/stat` -- from a stat line.
///
/// Takes the line rather than reading it, so the parse is testable on hosts without `/proc`
/// (this project is developed on darwin). Field 2 is `comm`, which may contain spaces *and*
/// parentheses, so the fields after it can only be located by splitting at the **last** `)`.
/// Tokens after that point resume at field 3, putting start time at index 19.
// Only called from the Linux branch of `process_start_token`, but always compiled so the parse
// stays unit-testable on hosts without `/proc` -- which is where this project is developed.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn parse_proc_start_time(stat_line: &str) -> Option<String> {
    let after_comm = stat_line.rsplit_once(')')?.1;
    after_comm
        .split_whitespace()
        .nth(19)
        .map(|field| field.to_string())
}

/// Identity token for a running process: its kernel-reported start time.
///
/// A PID alone is not an identity. In a container the entrypoint is always PID 1, so a PID
/// recorded by a process that has since been replaced still "exists" -- and `kill(pid, 0)`
/// happily confirms it. Pairing the PID with its start time distinguishes *that* process from
/// whatever occupies the number now.
///
/// `None` means "cannot establish identity": no `/proc` (non-Linux), no permission, or the
/// process vanished between calls. Callers must decide the fail direction themselves, because
/// it differs -- see `DeferredFreezeRecord::owner_is_live` (indeterminate ⇒ assume live) versus
/// the lock-file readers (absent token ⇒ fall back to PID-only).
pub fn process_start_token(pid: u32) -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        let content = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        parse_proc_start_time(&content)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        None
    }
}

/// Whether a lock file's recorded holder is still the process that took the lock.
///
/// A PID alone cannot answer this in a container, where the entrypoint is always PID 1: a lock
/// left behind by a hard-killed PID-1 process looks held forever, because the replacement is
/// PID 1 too. Pairing the PID with its start-time token distinguishes them.
///
/// Fail directions, both toward "still held":
/// - `token` absent (legacy lock file, or no `/proc`) ⇒ PID-only, so held while the PID exists.
/// - `token` present but the current one is unreadable ⇒ held, rather than steal a live lock.
///
/// This is deliberately the opposite of a deferred-freeze record's absent-token handling; see
/// [`LockInfo::token`].
fn holder_is_live(pid: u32, token: Option<&str>) -> bool {
    if !is_pid_alive(pid) {
        return false;
    }
    match token {
        None => true,
        Some(recorded) => match process_start_token(pid) {
            Some(current) => current == recorded,
            None => true,
        },
    }
}

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
///
/// **A PID is not an identity.** This answers only "is *something* running under this number",
/// which in a container is permanently true for PID 1. Pair it with [`process_start_token`] --
/// or use [`holder_is_live`] for lock files -- when the question is whether a *specific* process
/// is still running.
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
            token: None,
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
    fn lock_publication_atomic() {
        let dir = TempDir::new().unwrap();
        let path = lock_path(&dir);

        let lock = PidLock::acquire(&path, "publish").unwrap();

        // A published lock is never observable empty: it parses the moment it exists.
        let info: LockInfo = serde_json::from_str(&fs::read_to_string(lock.path()).unwrap())
            .expect("the published lock file must parse");
        assert_eq!(info.pid, std::process::id());

        let strays: Vec<String> = fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter(|name| name != "test.pid")
            .collect();
        assert!(
            strays.is_empty(),
            "publication must leave no temp file behind: {strays:?}"
        );
    }

    #[test]
    fn lock_zero_byte_lock_not_stale() {
        let dir = TempDir::new().unwrap();
        let path = lock_path(&dir);
        // Exactly what the old create-then-write window exposed to a concurrent acquirer.
        fs::write(&path, b"").unwrap();

        let started = std::time::Instant::now();
        let lock = PidLock::acquire(&path, "corrupt").unwrap();
        let elapsed = started.elapsed();

        assert!(
            elapsed >= CORRUPT_LOCK_RETRY_DELAY * CORRUPT_LOCK_MAX_READS,
            "an unparseable lock must be re-read, not taken on sight (took {elapsed:?})"
        );
        // Bounded, though -- a corrupt file must not wedge the tool forever.
        let info: LockInfo = serde_json::from_str(&fs::read_to_string(lock.path()).unwrap())
            .expect("the corrupt lock must eventually be replaced by a valid one");
        assert_eq!(info.pid, std::process::id());
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
    ///
    /// `token: None` mirrors a lock written by a binary predating the field, which is also the
    /// shape a non-Linux host produces.
    fn write_foreign_lock(path: &Path, pid: u32) {
        let info = LockInfo {
            pid,
            command: "foreign".to_string(),
            timestamp: "2020-01-01T00:00:00Z".to_string(),
            token: None,
        };
        fs::write(path, serde_json::to_string(&info).unwrap()).unwrap();
    }

    fn backup_scope(name: &str) -> LockScope {
        LockScope::Backup(name.to_string())
    }

    // -- process identity token --

    #[test]
    fn test_parse_proc_start_time_plain_comm() {
        // Field 22 is start time. Fields after `comm` resume at 3, so it is index 19 there.
        let fields: Vec<String> = (3..=52).map(|n| n.to_string()).collect();
        let line = format!("1 (chbackup) {}", fields.join(" "));
        assert_eq!(parse_proc_start_time(&line).as_deref(), Some("22"));
    }

    #[test]
    fn test_parse_proc_start_time_comm_with_spaces_and_parens() {
        // The reason this cannot be a whitespace split from the left: `comm` is arbitrary bytes
        // and routinely contains both spaces and parentheses. Splitting at the LAST ')' is what
        // makes the field offsets correct.
        let fields: Vec<String> = (3..=52).map(|n| n.to_string()).collect();
        let line = format!("1 (weird (name) with spaces) {}", fields.join(" "));
        assert_eq!(parse_proc_start_time(&line).as_deref(), Some("22"));
    }

    #[test]
    fn test_parse_proc_start_time_rejects_truncated_line() {
        assert!(parse_proc_start_time("1 (chbackup) S 1 2 3").is_none());
        assert!(parse_proc_start_time("no parenthesis here").is_none());
    }

    // -- lock-file holder identity --

    #[test]
    fn test_holder_is_live_absent_token_falls_back_to_pid() {
        // Deliberately the OPPOSITE fail direction from a deferred-freeze record's absent token.
        // A lock has no TTL backstop, so the hazard is stealing a live operation's lock rather
        // than holding forever. Absent token therefore means "held while the PID exists".
        assert!(holder_is_live(std::process::id(), None));
        assert!(!holder_is_live(DEAD_PID, None));
    }

    #[test]
    fn test_holder_is_live_mismatched_token_is_not_held() {
        // The container case: a hard-killed PID-1 holder leaves a lock file, and the replacement
        // is PID 1 too. Without the token that lock would look held forever.
        if process_start_token(std::process::id()).is_some() {
            assert!(!holder_is_live(
                std::process::id(),
                Some("not-this-processes-start-time")
            ));
        }
    }

    #[test]
    fn test_holder_is_live_matching_token_is_held() {
        if let Some(token) = process_start_token(std::process::id()) {
            assert!(holder_is_live(std::process::id(), Some(&token)));
        }
    }

    #[test]
    fn test_legacy_lock_json_without_token_is_not_corrupt() {
        // Without `#[serde(default)]` on the field, a lock file written by an earlier binary
        // fails to deserialise and `inspect_lock_file` reports it as Unreadable -- which would
        // make every pre-upgrade lock look corrupt.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("chbackup.legacy.pid");
        fs::write(
            &path,
            r#"{"pid":4000000,"command":"upload","timestamp":"2020-01-01T00:00:00Z"}"#,
        )
        .unwrap();

        match inspect_lock_file(&path) {
            ExistingLock::Stale => {}
            other => panic!("expected Stale for a legacy dead-PID lock, got {other:?}"),
        }
        assert!(!is_lock_file_active(&path));
    }

    #[test]
    fn test_release_deferred_is_backup_scoped() {
        // The fall-through in `lock_for_command` is `LockScope::None`, so a mutating command
        // omitted from the match silently runs with no lock at all. `release-deferred` must not.
        assert_eq!(
            lock_for_command("release-deferred", Some("bk-1")),
            LockScope::Backup("bk-1".to_string())
        );
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
                    for _ in 0..100 {
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
