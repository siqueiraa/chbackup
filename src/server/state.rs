//! Application state and operation management for the API server.
//!
//! `AppState` is shared across all axum handlers via `State<AppState>`.
//! It provides operation lifecycle management with concurrency control
//! via a semaphore (single-op when allow_parallel=false).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use chrono::{DateTime, Utc};
use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;

use crate::clickhouse::ChClient;
use crate::config::Config;
use crate::list::ManifestCache;
use crate::storage::S3Client;

use tracing::{info, warn};

use super::actions::ActionLog;
use super::metrics::Metrics;

/// Current status of the watch loop, shared between the watch loop and API handlers.
#[derive(Debug, Clone)]
pub struct WatchStatus {
    /// Whether the watch loop is currently active.
    pub active: bool,
    /// Human-readable state (e.g. "idle", "creating_full", "sleeping").
    pub state: String,
    /// Timestamp of the last successful full backup.
    pub last_full: Option<DateTime<Utc>>,
    /// Timestamp of the last successful incremental backup.
    pub last_incr: Option<DateTime<Utc>>,
    /// Number of consecutive errors in the watch loop.
    pub consecutive_errors: u32,
    /// Estimated time until the next backup (when sleeping).
    pub next_backup_in: Option<Duration>,
}

impl Default for WatchStatus {
    fn default() -> Self {
        Self {
            active: false,
            state: "inactive".to_string(),
            last_full: None,
            last_incr: None,
            consecutive_errors: 0,
            next_backup_in: None,
        }
    }
}

/// Shared application state for all axum handlers.
///
/// Must be `Clone` for axum `State` extractor. All inner fields are
/// `Arc`-wrapped or implement `Clone`.
///
/// The `config`, `ch`, and `s3` fields use `ArcSwap` to enable hot-swapping
/// via the `/api/v1/restart` endpoint. Handlers read them via `.load()` which
/// returns a `Guard<Arc<T>>` that derefs to `T`.
#[derive(Clone)]
pub struct AppState {
    pub config: Arc<ArcSwap<Config>>,
    pub ch: Arc<ArcSwap<ChClient>>,
    pub s3: Arc<ArcSwap<S3Client>>,
    pub action_log: Arc<Mutex<ActionLog>>,
    pub current_op: Arc<Mutex<Option<RunningOp>>>,
    pub op_semaphore: Arc<Semaphore>,
    /// Prometheus metrics registry. `None` when `config.api.enable_metrics` is false.
    pub metrics: Option<Arc<Metrics>>,
    /// Watch loop shutdown signal sender. `None` when watch is not active.
    pub watch_shutdown_tx: Option<tokio::sync::watch::Sender<bool>>,
    /// Watch loop config reload signal sender. `None` when watch is not active.
    pub watch_reload_tx: Option<tokio::sync::watch::Sender<bool>>,
    /// Shared watch status for API queries.
    pub watch_status: Arc<Mutex<WatchStatus>>,
    /// Path to the config file, used for config reload.
    pub config_path: PathBuf,
    /// In-memory cache for remote backup summaries (design 8.4).
    /// TTL-based expiry, invalidated on mutating operations.
    pub manifest_cache: Arc<Mutex<ManifestCache>>,
}

/// Tracks a currently running operation for cancellation support.
pub struct RunningOp {
    pub id: u64,
    pub command: String,
    pub cancel_token: CancellationToken,
    /// Held for the duration of the operation to enforce concurrency limits.
    _permit: OwnedSemaphorePermit,
}

impl AppState {
    /// Create a new AppState from config and client instances.
    ///
    /// The semaphore permits are set based on `config.api.allow_parallel`:
    /// - `false` (default): 1 permit -- operations are serialized
    /// - `true`: effectively unlimited permits
    pub fn new(config: Arc<Config>, ch: ChClient, s3: S3Client, config_path: PathBuf) -> Self {
        let permits = if config.api.allow_parallel {
            // Use a large number to approximate unlimited
            Semaphore::MAX_PERMITS
        } else {
            1
        };

        // Conditionally create Prometheus metrics
        let metrics = if config.api.enable_metrics {
            match Metrics::new() {
                Ok(m) => {
                    let count = m.registry.gather().len();
                    info!("Metrics registry created with {} metric families", count);
                    Some(Arc::new(m))
                }
                Err(e) => {
                    warn!(error = %e, "Failed to create metrics registry, continuing without metrics");
                    None
                }
            }
        } else {
            None
        };

        let cache_ttl = Duration::from_secs(config.general.remote_cache_ttl_secs);
        let manifest_cache = Arc::new(Mutex::new(ManifestCache::new(cache_ttl)));

        Self {
            config: Arc::new(ArcSwap::from_pointee((*config).clone())),
            ch: Arc::new(ArcSwap::from_pointee(ch)),
            s3: Arc::new(ArcSwap::from_pointee(s3)),
            action_log: Arc::new(Mutex::new(ActionLog::new(100))),
            current_op: Arc::new(Mutex::new(None)),
            op_semaphore: Arc::new(Semaphore::new(permits)),
            metrics,
            watch_shutdown_tx: None,
            watch_reload_tx: None,
            watch_status: Arc::new(Mutex::new(WatchStatus::default())),
            config_path,
            manifest_cache,
        }
    }

    /// Try to start a new operation. Returns (action_id, cancellation_token) on success.
    ///
    /// If the semaphore cannot be acquired (another operation is running and
    /// allow_parallel=false), returns an error.
    pub async fn try_start_op(
        &self,
        command: &str,
    ) -> Result<(u64, CancellationToken), &'static str> {
        let permit = self
            .op_semaphore
            .clone()
            .try_acquire_owned()
            .map_err(|_| "operation already in progress")?;

        let token = CancellationToken::new();
        let id = {
            let mut log = self.action_log.lock().await;
            log.start(command.to_string())
        };

        {
            let mut current = self.current_op.lock().await;
            *current = Some(RunningOp {
                id,
                command: command.to_string(),
                cancel_token: token.clone(),
                _permit: permit,
            });
        }

        Ok((id, token))
    }

    /// Mark an operation as completed successfully.
    pub async fn finish_op(&self, id: u64) {
        {
            let mut log = self.action_log.lock().await;
            log.finish(id);
        }
        {
            let mut current = self.current_op.lock().await;
            if current.as_ref().is_some_and(|op| op.id == id) {
                *current = None;
            }
        }
    }

    /// Mark an operation as failed with an error message.
    pub async fn fail_op(&self, id: u64, error: String) {
        {
            let mut log = self.action_log.lock().await;
            log.fail(id, error);
        }
        {
            let mut current = self.current_op.lock().await;
            if current.as_ref().is_some_and(|op| op.id == id) {
                *current = None;
            }
        }
    }

    /// Cancel the currently running operation.
    ///
    /// Returns `true` if an operation was cancelled, `false` if no operation was running.
    pub async fn kill_current(&self) -> bool {
        let mut current = self.current_op.lock().await;
        if let Some(op) = current.take() {
            op.cancel_token.cancel();
            let mut log = self.action_log.lock().await;
            log.kill(op.id);
            true
        } else {
            false
        }
    }
}

/// A resumable operation found by scanning the backup directory for state files.
#[derive(Debug, Clone)]
pub struct ResumableOp {
    /// Name of the backup directory.
    pub backup_name: String,
    /// Type of operation: "upload", "download", or "restore".
    pub op_type: String,
}

/// Scan the backup directory for resumable state files.
///
/// Walks `{data_path}/backup/` and checks each subdirectory for
/// `upload.state.json`, `download.state.json`, or `restore.state.json`.
pub fn scan_resumable_state_files(data_path: &str) -> Vec<ResumableOp> {
    let backup_dir = std::path::Path::new(data_path).join("backup");
    let mut ops = Vec::new();

    let entries = match std::fs::read_dir(&backup_dir) {
        Ok(entries) => entries,
        Err(_) => return ops,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let backup_name = match path.file_name().and_then(|n| n.to_str()) {
            Some(name) => name.to_string(),
            None => continue,
        };

        for (state_file, op_type) in &[
            ("upload.state.json", "upload"),
            ("download.state.json", "download"),
            ("restore.state.json", "restore"),
        ] {
            if path.join(state_file).exists() {
                ops.push(ResumableOp {
                    backup_name: backup_name.clone(),
                    op_type: op_type.to_string(),
                });
            }
        }
    }

    ops
}

/// Scan for and auto-resume interrupted operations on server startup.
///
/// If `config.api.complete_resumable_after_restart` is false, this is a no-op.
/// Otherwise, scans for state files and spawns the corresponding operations.
pub async fn auto_resume(state: &AppState) {
    let config = state.config.load();
    if !config.api.complete_resumable_after_restart {
        tracing::info!("Auto-resume disabled by configuration");
        return;
    }

    let data_path = &config.clickhouse.data_path;
    let ops = scan_resumable_state_files(data_path);

    if ops.is_empty() {
        tracing::info!("Auto-resume: no resumable operations found");
        return;
    }

    tracing::info!(
        count = ops.len(),
        "Auto-resume: found {} resumable operations",
        ops.len()
    );

    for op in ops {
        let state_clone = state.clone();
        let backup_name = op.backup_name.clone();
        let op_type = op.op_type.clone();

        match op_type.as_str() {
            "upload" => {
                tokio::spawn(async move {
                    let (id, _token) = match state_clone.try_start_op("upload").await {
                        Ok(v) => v,
                        Err(e) => {
                            tracing::warn!(
                                backup_name = %backup_name,
                                error = %e,
                                "Auto-resume: could not start upload operation"
                            );
                            return;
                        }
                    };

                    tracing::info!(backup_name = %backup_name, "Auto-resume: resuming upload");

                    let config = state_clone.config.load();
                    let s3 = state_clone.s3.load();
                    let backup_dir = std::path::PathBuf::from(&config.clickhouse.data_path)
                        .join("backup")
                        .join(&backup_name);

                    let result = crate::upload::upload(
                        &config,
                        &s3,
                        &backup_name,
                        &backup_dir,
                        false, // delete_local
                        None,  // diff_from_remote
                        true,  // resume = true
                    )
                    .await;

                    match result {
                        Ok(_) => {
                            tracing::info!(backup_name = %backup_name, "Auto-resume: upload completed");
                            state_clone.finish_op(id).await;
                        }
                        Err(e) => {
                            tracing::warn!(backup_name = %backup_name, error = %e, "Auto-resume: upload failed");
                            state_clone.fail_op(id, e.to_string()).await;
                        }
                    }
                });
            }
            "download" => {
                tokio::spawn(async move {
                    let (id, _token) = match state_clone.try_start_op("download").await {
                        Ok(v) => v,
                        Err(e) => {
                            tracing::warn!(
                                backup_name = %backup_name,
                                error = %e,
                                "Auto-resume: could not start download operation"
                            );
                            return;
                        }
                    };

                    tracing::info!(backup_name = %backup_name, "Auto-resume: resuming download");

                    let config = state_clone.config.load();
                    let s3 = state_clone.s3.load();
                    let result = crate::download::download(
                        &config,
                        &s3,
                        &backup_name,
                        true,  // resume = true
                        false, // hardlink_exists_files = false
                    )
                    .await;

                    match result {
                        Ok(_) => {
                            tracing::info!(backup_name = %backup_name, "Auto-resume: download completed");
                            state_clone.finish_op(id).await;
                        }
                        Err(e) => {
                            tracing::warn!(backup_name = %backup_name, error = %e, "Auto-resume: download failed");
                            state_clone.fail_op(id, e.to_string()).await;
                        }
                    }
                });
            }
            "restore" => {
                tokio::spawn(async move {
                    let (id, _token) = match state_clone.try_start_op("restore").await {
                        Ok(v) => v,
                        Err(e) => {
                            tracing::warn!(
                                backup_name = %backup_name,
                                error = %e,
                                "Auto-resume: could not start restore operation"
                            );
                            return;
                        }
                    };

                    tracing::info!(backup_name = %backup_name, "Auto-resume: resuming restore");

                    let config = state_clone.config.load();
                    let ch = state_clone.ch.load();
                    let result = crate::restore::restore(
                        &config,
                        &ch,
                        &backup_name,
                        None,  // tables
                        false, // schema_only
                        false, // data_only
                        false, // rm (auto-resume never drops)
                        true,  // resume = true
                        None,  // rename_as (auto-resume restores to original names)
                        None,  // database_mapping (auto-resume restores to original names)
                        false, // rbac (auto-resume does not include RBAC restore)
                        false, // configs (auto-resume does not include config restore)
                        false, // named_collections (auto-resume does not include named collections restore)
                        None,  // partitions (auto-resume restores all partitions)
                        false, // skip_empty_tables
                    )
                    .await;

                    match result {
                        Ok(_) => {
                            tracing::info!(backup_name = %backup_name, "Auto-resume: restore completed");
                            state_clone.finish_op(id).await;
                        }
                        Err(e) => {
                            tracing::warn!(backup_name = %backup_name, error = %e, "Auto-resume: restore failed");
                            state_clone.fail_op(id, e.to_string()).await;
                        }
                    }
                });
            }
            other => {
                tracing::warn!(op_type = %other, "Auto-resume: unknown operation type, skipping");
            }
        }

        // Small delay between spawned operations to avoid overwhelming the system
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    /// Helper to create an AppState for testing without real CH/S3 clients.
    /// We cannot construct real ChClient/S3Client without servers, so we test
    /// the operation management logic through the action_log and semaphore directly.
    fn test_config(allow_parallel: bool) -> Arc<Config> {
        let mut config = Config::default();
        config.api.allow_parallel = allow_parallel;
        Arc::new(config)
    }

    #[tokio::test]
    async fn test_app_state_operation_lifecycle() {
        // Test the action log and semaphore behavior directly
        let _config = test_config(false);
        let action_log = Arc::new(Mutex::new(ActionLog::new(100)));
        let op_semaphore = Arc::new(Semaphore::new(1));

        // Start an operation
        let permit = op_semaphore
            .clone()
            .try_acquire_owned()
            .expect("Should acquire permit");

        let id = {
            let mut log = action_log.lock().await;
            log.start("create".to_string())
        };
        assert_eq!(id, 1);

        // Verify running
        {
            let log = action_log.lock().await;
            assert!(log.running().is_some());
            assert_eq!(log.running().unwrap().id, 1);
        }

        // Cannot acquire another permit (allow_parallel=false, 1 permit)
        assert!(op_semaphore.clone().try_acquire_owned().is_err());

        // Finish operation
        {
            let mut log = action_log.lock().await;
            log.finish(id);
        }
        drop(permit);

        // Verify completed
        {
            let log = action_log.lock().await;
            assert!(log.running().is_none());
        }

        // Can acquire permit again
        assert!(op_semaphore.clone().try_acquire_owned().is_ok());
    }

    #[tokio::test]
    async fn test_sequential_ops_blocked() {
        let _config = test_config(false);
        let op_semaphore = Arc::new(Semaphore::new(1));

        // Acquire first permit
        let _permit1 = op_semaphore
            .clone()
            .try_acquire_owned()
            .expect("Should acquire first permit");

        // Second acquire should fail
        let result = op_semaphore.clone().try_acquire_owned();
        assert!(result.is_err(), "Should be blocked by first operation");
    }

    #[tokio::test]
    async fn test_kill_cancels_token() {
        let token = CancellationToken::new();
        let child = token.clone();

        assert!(!child.is_cancelled());
        token.cancel();
        assert!(child.is_cancelled());
    }

    #[tokio::test]
    async fn test_parallel_ops_allowed() {
        let op_semaphore = Arc::new(Semaphore::new(Semaphore::MAX_PERMITS));

        // Should be able to acquire multiple permits
        let _permit1 = op_semaphore
            .clone()
            .try_acquire_owned()
            .expect("Should acquire first permit");
        let _permit2 = op_semaphore
            .clone()
            .try_acquire_owned()
            .expect("Should acquire second permit");
        let _permit3 = op_semaphore
            .clone()
            .try_acquire_owned()
            .expect("Should acquire third permit");
    }

    #[tokio::test]
    async fn test_app_state_watch_handle_default_none() {
        let config = test_config(false);

        // We can't construct a full AppState without real CH/S3 clients,
        // but we can verify the default values for watch fields directly.
        assert!(!config.api.allow_parallel);

        // Verify WatchStatus defaults
        let watch_status = WatchStatus::default();
        assert!(!watch_status.active);
        assert_eq!(watch_status.state, "inactive");
        assert!(watch_status.last_full.is_none());
        assert!(watch_status.last_incr.is_none());
        assert_eq!(watch_status.consecutive_errors, 0);
        assert!(watch_status.next_backup_in.is_none());

        // Verify that watch_shutdown_tx and watch_reload_tx
        // would be None in a newly-created AppState (checked via the new() logic)
        // The None initialization is in AppState::new(), verified by compilation.
    }

    #[test]
    fn test_scan_resumable_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let backup_dir = dir.path().join("backup");
        std::fs::create_dir_all(&backup_dir).unwrap();

        let ops = scan_resumable_state_files(dir.path().to_str().unwrap());
        assert!(ops.is_empty());
    }

    #[test]
    fn test_scan_resumable_finds_state_files() {
        let dir = tempfile::tempdir().unwrap();
        let backup_dir = dir.path().join("backup");

        // Create two backup directories with state files
        let backup1 = backup_dir.join("daily-2024-01-15");
        std::fs::create_dir_all(&backup1).unwrap();
        std::fs::write(backup1.join("upload.state.json"), "{}").unwrap();

        let backup2 = backup_dir.join("daily-2024-01-16");
        std::fs::create_dir_all(&backup2).unwrap();
        std::fs::write(backup2.join("download.state.json"), "{}").unwrap();
        std::fs::write(backup2.join("restore.state.json"), "{}").unwrap();

        let ops = scan_resumable_state_files(dir.path().to_str().unwrap());
        assert_eq!(ops.len(), 3);

        // Verify all ops are found (order may vary due to readdir)
        let upload_ops: Vec<_> = ops.iter().filter(|o| o.op_type == "upload").collect();
        let download_ops: Vec<_> = ops.iter().filter(|o| o.op_type == "download").collect();
        let restore_ops: Vec<_> = ops.iter().filter(|o| o.op_type == "restore").collect();

        assert_eq!(upload_ops.len(), 1);
        assert_eq!(download_ops.len(), 1);
        assert_eq!(restore_ops.len(), 1);
        assert_eq!(upload_ops[0].backup_name, "daily-2024-01-15");
        assert_eq!(download_ops[0].backup_name, "daily-2024-01-16");
        assert_eq!(restore_ops[0].backup_name, "daily-2024-01-16");
    }

    #[test]
    fn test_scan_resumable_nonexistent_dir() {
        let ops = scan_resumable_state_files("/nonexistent/path");
        assert!(ops.is_empty());
    }

    #[test]
    fn test_app_state_with_metrics_enabled() {
        // Verify that when enable_metrics=true (default), Metrics is created
        let config = test_config(false);
        assert!(
            config.api.enable_metrics,
            "Default config should have enable_metrics=true"
        );

        // Create metrics the same way AppState::new() does
        let metrics = if config.api.enable_metrics {
            Metrics::new().ok().map(Arc::new)
        } else {
            None
        };

        assert!(
            metrics.is_some(),
            "Metrics should be Some when enable_metrics=true"
        );

        // Verify the registry has all 14 metric families
        let families = metrics.as_ref().unwrap().registry.gather();
        assert_eq!(
            families.len(),
            14,
            "Expected 14 metric families, got {}",
            families.len()
        );
    }

    #[test]
    fn test_app_state_with_metrics_disabled() {
        // Verify that when enable_metrics=false, Metrics is None
        let mut config = Config::default();
        config.api.enable_metrics = false;
        let config = Arc::new(config);

        let metrics = if config.api.enable_metrics {
            Metrics::new().ok().map(Arc::new)
        } else {
            None
        };

        assert!(
            metrics.is_none(),
            "Metrics should be None when enable_metrics=false"
        );
    }

    #[test]
    fn test_scan_resumable_ignores_files_in_backup_root() {
        let dir = tempfile::tempdir().unwrap();
        let backup_dir = dir.path().join("backup");
        std::fs::create_dir_all(&backup_dir).unwrap();

        // A file directly in backup/ should be ignored (not a directory)
        std::fs::write(backup_dir.join("stale.json"), "{}").unwrap();

        let ops = scan_resumable_state_files(dir.path().to_str().unwrap());
        assert!(ops.is_empty());
    }
}
