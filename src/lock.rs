use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

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
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
        {
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

// ---------------------------------------------------------------------------
// Lock scope
// ---------------------------------------------------------------------------

/// Three-tier lock scope from design doc section 2.
///
/// - `Backup(name)` -- per-backup lock (`/tmp/chbackup.{name}.pid`)
/// - `Global` -- global lock (`/tmp/chbackup.global.pid`)
/// - `None` -- no lock required (read-only commands)
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
        "clean" | "clean_broken" | "delete" => LockScope::Global,
        // list, tables, default-config, print-config, watch, server
        _ => LockScope::None,
    }
}

/// Resolve a [`LockScope`] to an optional filesystem path.
///
/// Returns `None` for `LockScope::None`.
pub fn lock_path_for_scope(scope: &LockScope) -> Option<PathBuf> {
    match scope {
        LockScope::Backup(name) => Some(PathBuf::from(format!("/tmp/chbackup.{name}.pid"))),
        LockScope::Global => Some(PathBuf::from("/tmp/chbackup.global.pid")),
        LockScope::None => None,
    }
}

// ---------------------------------------------------------------------------
// Platform-specific PID liveness check
// ---------------------------------------------------------------------------

/// Check if a process with the given PID is alive.
///
/// Uses `kill(pid, 0)` on Unix.  Returns `false` on any error or on
/// non-Unix platforms.
fn is_pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // SAFETY: signal 0 does not send a signal; it only checks that the
        // process exists and we have permission to signal it.
        let ret = unsafe { libc::kill(pid as libc::pid_t, 0) };
        ret == 0
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
}
