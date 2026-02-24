//! Resume state types and serialization helpers for resumable operations.
//!
//! Each operation (upload, download, restore) writes a state file alongside
//! the backup directory. On `--resume`, completed work is loaded from the
//! state file and skipped. State file write failures are non-fatal warnings
//! per design doc section 16.1.
//!
//! State files use atomic write (write to `.tmp`, then rename) to prevent
//! corrupt state on crash. Hash-based invalidation ensures stale state from
//! different parameters is discarded.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use tracing::warn;

/// Resume state for the upload pipeline.
///
/// Tracks which S3 keys have been successfully uploaded so that a resumed
/// upload can skip already-completed parts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadState {
    /// S3 keys that have been successfully uploaded.
    pub completed_keys: HashSet<String>,
    /// Backup name this state belongs to.
    pub backup_name: String,
    /// Hash of operation parameters for invalidation.
    pub params_hash: String,
}

/// Resume state for the download pipeline.
///
/// Tracks which S3 keys have been successfully downloaded and decompressed
/// so that a resumed download can skip already-completed parts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadState {
    /// S3 keys that have been successfully downloaded.
    pub completed_keys: HashSet<String>,
    /// Backup name this state belongs to.
    pub backup_name: String,
    /// Hash of operation parameters for invalidation.
    pub params_hash: String,
    /// Disk name -> disk path mapping from manifest. Persisted so delete_local
    /// can discover per-disk dirs even if download fails before writing metadata.json.
    /// Written unconditionally (not gated by resume mode) for cleanup safety.
    #[serde(default)]
    pub disk_map: HashMap<String, String>,
}

/// Resume state for the restore pipeline.
///
/// Tracks which parts have been successfully attached per table so that a
/// resumed restore can skip already-attached parts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RestoreState {
    /// Map of "db.table" -> list of part names that have been attached.
    pub attached_parts: HashMap<String, Vec<String>>,
    /// Backup name this state belongs to.
    pub backup_name: String,
    /// Hash of operation parameters for invalidation.
    /// Old state files without this field default to "" (empty = always-mismatch sentinel
    /// that triggers a warning but does NOT discard state, for safe rollout).
    #[serde(default)]
    pub params_hash: String,
}

/// Atomically save a state file by writing to a temporary file then renaming.
///
/// The temporary file is `{path}.tmp`. On success, the temp file is renamed
/// to the final path. This prevents corrupt state if the process crashes
/// mid-write.
pub fn save_state_file<T: Serialize>(path: &Path, state: &T) -> anyhow::Result<()> {
    let tmp_path = path.with_extension("json.tmp");

    let json = serde_json::to_string_pretty(state)
        .map_err(|e| anyhow::anyhow!("Failed to serialize state: {}", e))?;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| anyhow::anyhow!("Failed to create state directory: {}", e))?;
    }

    std::fs::write(&tmp_path, json.as_bytes())
        .map_err(|e| anyhow::anyhow!("Failed to write state temp file: {}", e))?;

    std::fs::rename(&tmp_path, path)
        .map_err(|e| anyhow::anyhow!("Failed to rename state temp file: {}", e))?;

    Ok(())
}

/// Load a state file, returning `None` if the file does not exist.
///
/// Returns `Ok(None)` when the file is absent (normal for first run),
/// `Ok(Some(state))` on successful load, or `Err` on parse failure.
pub fn load_state_file<T: DeserializeOwned>(path: &Path) -> anyhow::Result<Option<T>> {
    if !path.exists() {
        return Ok(None);
    }

    let contents = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("Failed to read state file {}: {}", path.display(), e))?;

    let state: T = serde_json::from_str(&contents)
        .map_err(|e| anyhow::anyhow!("Failed to parse state file {}: {}", path.display(), e))?;

    Ok(Some(state))
}

/// Save state file with graceful degradation (non-fatal on failure).
///
/// Per design doc section 16.1: state file write failures are logged as
/// warnings but do not propagate errors. The operation continues but
/// will not be resumable.
pub fn save_state_graceful<T: Serialize>(path: &Path, state: &T) {
    if let Err(e) = save_state_file(path, state) {
        warn!(
            path = %path.display(),
            error = %e,
            "Failed to write resumable state. Operation continues but won't be resumable."
        );
    }
}

/// Compute a deterministic hash of operation parameters for state invalidation.
///
/// When the hash of the current operation's parameters does not match the
/// stored state's hash, the state is considered stale and is discarded.
/// This prevents resuming with incompatible parameters.
pub fn compute_params_hash(params: &[&str]) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for p in params {
        p.hash(&mut hasher);
    }
    format!("{:016x}", hasher.finish())
}

/// Delete a state file after successful operation completion.
///
/// Logs a warning if deletion fails (non-fatal -- leftover state files
/// are harmless since they will be invalidated by `params_hash` on next run).
pub fn delete_state_file(path: &Path) {
    if path.exists() {
        if let Err(e) = std::fs::remove_file(path) {
            warn!(
                path = %path.display(),
                error = %e,
                "Failed to delete state file after successful completion"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_upload_state_roundtrip() {
        let state = UploadState {
            completed_keys: HashSet::from([
                "backup/data/db/table/part1.tar.lz4".to_string(),
                "backup/data/db/table/part2.tar.lz4".to_string(),
            ]),
            backup_name: "daily-2024-01-15".to_string(),
            params_hash: "abc123".to_string(),
        };

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("upload.state.json");

        save_state_file(&path, &state).unwrap();
        let loaded: UploadState = load_state_file(&path).unwrap().unwrap();

        assert_eq!(loaded.completed_keys, state.completed_keys);
        assert_eq!(loaded.backup_name, state.backup_name);
        assert_eq!(loaded.params_hash, state.params_hash);
    }

    #[test]
    fn test_download_state_roundtrip() {
        let state = DownloadState {
            completed_keys: HashSet::from(["backup/data/db/table/part1.tar.lz4".to_string()]),
            backup_name: "daily-2024-01-15".to_string(),
            params_hash: "def456".to_string(),
            disk_map: HashMap::new(),
        };

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("download.state.json");

        save_state_file(&path, &state).unwrap();
        let loaded: DownloadState = load_state_file(&path).unwrap().unwrap();

        assert_eq!(loaded.completed_keys, state.completed_keys);
        assert_eq!(loaded.backup_name, state.backup_name);
        assert_eq!(loaded.params_hash, state.params_hash);
        assert!(loaded.disk_map.is_empty());
    }

    #[test]
    fn test_restore_state_roundtrip() {
        let mut attached_parts = HashMap::new();
        attached_parts.insert(
            "default.trades".to_string(),
            vec!["202401_1_50_3".to_string(), "202402_1_1_0".to_string()],
        );

        let state = RestoreState {
            attached_parts,
            backup_name: "daily-2024-01-15".to_string(),
            params_hash: compute_params_hash(&["daily-2024-01-15", "*.*", "", "", "", ""]),
        };

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("restore.state.json");

        save_state_file(&path, &state).unwrap();
        let loaded: RestoreState = load_state_file(&path).unwrap().unwrap();

        assert_eq!(loaded.attached_parts, state.attached_parts);
        assert_eq!(loaded.backup_name, state.backup_name);
        assert_eq!(loaded.params_hash, state.params_hash);
    }

    #[test]
    fn test_load_state_file_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nonexistent.state.json");

        let result: anyhow::Result<Option<UploadState>> = load_state_file(&path);
        assert!(result.unwrap().is_none());
    }

    #[test]
    fn test_save_state_graceful_writes_file() {
        let state = UploadState {
            completed_keys: HashSet::from(["key1".to_string()]),
            backup_name: "test".to_string(),
            params_hash: "hash".to_string(),
        };

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("graceful.state.json");

        save_state_graceful(&path, &state);

        // File should exist and be loadable
        let loaded: UploadState = load_state_file(&path).unwrap().unwrap();
        assert_eq!(loaded.completed_keys.len(), 1);
    }

    #[test]
    fn test_state_invalidation_on_param_change() {
        let state = UploadState {
            completed_keys: HashSet::from(["key1".to_string()]),
            backup_name: "daily-2024-01-15".to_string(),
            params_hash: compute_params_hash(&["daily-2024-01-15", "*.*", ""]),
        };

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("invalidate.state.json");
        save_state_file(&path, &state).unwrap();

        // Load state
        let loaded: UploadState = load_state_file(&path).unwrap().unwrap();

        // Same params should match
        let same_hash = compute_params_hash(&["daily-2024-01-15", "*.*", ""]);
        assert_eq!(loaded.params_hash, same_hash);

        // Different params should NOT match
        let different_hash = compute_params_hash(&["daily-2024-01-16", "*.*", ""]);
        assert_ne!(loaded.params_hash, different_hash);
    }

    #[test]
    fn test_compute_params_hash_deterministic() {
        let h1 = compute_params_hash(&["backup1", "*.trades", "base"]);
        let h2 = compute_params_hash(&["backup1", "*.trades", "base"]);
        assert_eq!(h1, h2);
    }

    #[test]
    fn test_compute_params_hash_different_params() {
        let h1 = compute_params_hash(&["backup1", "*.trades"]);
        let h2 = compute_params_hash(&["backup2", "*.trades"]);
        assert_ne!(h1, h2);
    }

    #[test]
    fn test_delete_state_file_exists() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("delete_me.state.json");
        std::fs::write(&path, "{}").unwrap();
        assert!(path.exists());

        delete_state_file(&path);
        assert!(!path.exists());
    }

    #[test]
    fn test_delete_state_file_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does_not_exist.state.json");

        // Should not panic or error
        delete_state_file(&path);
    }

    #[test]
    fn test_save_state_atomic_write() {
        // Verify that save_state_file does NOT leave a .tmp file
        let state = UploadState {
            completed_keys: HashSet::new(),
            backup_name: "test".to_string(),
            params_hash: "hash".to_string(),
        };

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("atomic.state.json");
        let tmp_path = dir.path().join("atomic.state.json.tmp");

        save_state_file(&path, &state).unwrap();

        assert!(path.exists());
        assert!(!tmp_path.exists(), ".tmp file should be renamed away");
    }

    #[test]
    fn test_restore_state_has_params_hash() {
        let state = RestoreState {
            attached_parts: Default::default(),
            backup_name: "test".to_string(),
            params_hash: compute_params_hash(&["test", "", "false", "false", "", ""]),
        };
        assert!(!state.params_hash.is_empty());
    }

    #[test]
    fn test_restore_state_old_format_deserializes_with_default_hash() {
        // Old state files without params_hash should deserialize to empty string (serde default)
        let json = r#"{"attached_parts":{},"backup_name":"my-backup"}"#;
        let state: RestoreState = serde_json::from_str(json).unwrap();
        assert_eq!(state.params_hash, "");
    }
}
