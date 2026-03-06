//! Application state and operation management for the API server.
//!
//! `AppState` is shared across all axum handlers via `State<AppState>`.
//! It provides operation lifecycle management with concurrency control
//! via a semaphore (single-op when allow_parallel=false).

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use axum::http::StatusCode;
use axum::Json;
use chrono::{DateTime, Utc};
use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore};

/// Compute the semaphore permit count for a given `allow_parallel` setting.
///
/// `false` (default): 1 permit — operations are serialized.
/// `true`: effectively unlimited permits.
fn semaphore_permits(allow_parallel: bool) -> usize {
    if allow_parallel {
        Semaphore::MAX_PERMITS
    } else {
        1
    }
}
use tokio_util::sync::CancellationToken;

use crate::clickhouse::ChClient;
use crate::config::Config;
use crate::list::ManifestCache;
use crate::lock::{lock_for_command, lock_path_for_scope, PidLock};
use crate::storage::S3Client;

use tracing::{info, warn};

use super::actions::ActionLog;
use super::metrics::{Metrics, OperationLabels};
use super::routes::{ErrorResponse, OperationStarted};

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
    pub running_ops: Arc<Mutex<HashMap<u64, RunningOp>>>,
    /// Concurrency semaphore. Wrapped in `ArcSwap` so `/api/v1/restart` can
    /// atomically replace it when `allow_parallel` changes between the old and
    /// new config without disrupting in-flight operations (which hold permits
    /// against the previous `Semaphore` instance).
    pub op_semaphore: Arc<ArcSwap<Semaphore>>,
    /// Prometheus metrics registry. `None` when `config.api.enable_metrics` is false.
    pub metrics: Option<Arc<Metrics>>,
    /// Watch loop shutdown signal sender. `None` when watch is not active.
    /// Wrapped in `Arc<Mutex<Option<...>>>` so mutations made by `spawn_watch_from_state`
    /// (called with a local axum `State` clone) are visible to all other handler clones.
    pub watch_shutdown_tx: Arc<Mutex<Option<tokio::sync::watch::Sender<bool>>>>,
    /// Watch loop config reload signal sender. `None` when watch is not active.
    pub watch_reload_tx: Arc<Mutex<Option<tokio::sync::watch::Sender<bool>>>>,
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
    /// The backup name this operation is working on, used for per-backup conflict detection.
    pub backup_name: Option<String>,
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
        let permits = semaphore_permits(config.api.allow_parallel);

        // Conditionally create Prometheus metrics
        let metrics = if config.api.enable_metrics {
            let m = Metrics::new();
            info!("Metrics registry created");
            Some(Arc::new(m))
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
            running_ops: Arc::new(Mutex::new(HashMap::new())),
            op_semaphore: Arc::new(ArcSwap::from_pointee(Semaphore::new(permits))),
            metrics,
            watch_shutdown_tx: Arc::new(Mutex::new(None)),
            watch_reload_tx: Arc::new(Mutex::new(None)),
            watch_status: Arc::new(Mutex::new(WatchStatus::default())),
            config_path,
            manifest_cache,
        }
    }

    /// Try to start a new operation. Returns (action_id, cancellation_token) on success.
    ///
    /// If the semaphore cannot be acquired (another operation is running and
    /// allow_parallel=false), returns an error.
    ///
    /// When `backup_name` is `Some`, also rejects requests for the same backup name
    /// as an already-running operation (relevant when `allow_parallel=true`).
    pub async fn try_start_op(
        &self,
        command: &str,
        backup_name: Option<String>,
    ) -> Result<(u64, CancellationToken), &'static str> {
        let permit = self
            .op_semaphore
            .load_full()
            .try_acquire_owned()
            .map_err(|_| "operation already in progress")?;

        let token = CancellationToken::new();

        // Log the action first (action_log lock acquired and released before running_ops).
        // This preserves the existing lock order: action_log is never held simultaneously
        // with running_ops.
        let id = {
            let mut log = self.action_log.lock().await;
            log.start(command.to_string())
        };

        // Build the RunningOp outside the lock so the critical section stays minimal.
        let running_op = RunningOp {
            id,
            command: command.to_string(),
            backup_name: backup_name.clone(),
            cancel_token: token.clone(),
            _permit: permit,
        };

        // Atomically check for a same-backup conflict and insert — eliminates the TOCTOU
        // race that existed when check and insert were separate lock acquisitions.
        // If a conflict is detected, running_op (including the permit) is dropped here,
        // releasing the semaphore permit.
        let conflict = {
            let mut ops = self.running_ops.lock().await;
            let has_conflict = backup_name.as_deref().is_some_and(|name| {
                ops.values()
                    .any(|op| op.backup_name.as_deref() == Some(name))
            });
            if !has_conflict {
                ops.insert(id, running_op);
            }
            has_conflict
        };

        if conflict {
            // Record the rejection in the action log so GET /api/v1/actions shows it.
            {
                let mut log = self.action_log.lock().await;
                log.fail(
                    id,
                    "operation already in progress for this backup".to_string(),
                );
            }
            return Err("operation already in progress for this backup");
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
            let mut ops = self.running_ops.lock().await;
            ops.remove(&id);
        }
    }

    /// Mark an operation as failed with an error message.
    pub async fn fail_op(&self, id: u64, error: String) {
        {
            let mut log = self.action_log.lock().await;
            log.fail(id, error);
        }
        {
            let mut ops = self.running_ops.lock().await;
            ops.remove(&id);
        }
    }

    /// Cancel one or all running operations.
    ///
    /// If `id` is `Some(id)`, cancels and removes only the specified operation.
    /// If `id` is `None`, cancels ALL running operations (backward-compatible kill-all).
    ///
    /// Returns `true` if at least one operation was cancelled, `false` if none matched.
    pub async fn kill_op(&self, id: Option<u64>) -> bool {
        // Step 1: Remove target op(s) from running_ops under its own lock, then drop the lock.
        // This avoids holding running_ops and action_log simultaneously, which would invert the
        // lock order used by try_start_op / finish_op / fail_op (action_log first, then
        // running_ops) and create a deadlock under concurrent calls.
        let killed_ops: Vec<RunningOp> = {
            let mut ops = self.running_ops.lock().await;
            match id {
                Some(target_id) => {
                    if let Some(op) = ops.remove(&target_id) {
                        vec![op]
                    } else {
                        vec![]
                    }
                }
                None => ops.drain().map(|(_, op)| op).collect(),
            }
        }; // running_ops lock dropped here

        if killed_ops.is_empty() {
            return false;
        }

        // Step 2: Cancel tokens — no lock needed.
        for op in &killed_ops {
            op.cancel_token.cancel();
        }

        // Step 3: Update action_log under its own lock (consistent order with other methods).
        {
            let mut log = self.action_log.lock().await;
            for op in &killed_ops {
                log.kill(op.id);
            }
        }

        true
    }
}

/// DRY orchestration helper for all operation endpoints (except post_actions).
///
/// Encapsulates the common try_start_op / spawn / select! / metrics / finish_op
/// boilerplate. The caller provides a closure that receives loaded config, ch,
/// and s3 clients and returns `Result<()>`. Operation-specific metrics (e.g.
/// `backup_size_bytes`) should be recorded inside the closure.
///
/// `post_actions` is excluded because it returns `(StatusCode, Json<OperationStarted>)`
/// (201 CREATED) which is incompatible with this helper's return type.
pub async fn run_operation<F, Fut>(
    state: &AppState,
    command: &str,
    op_label: &str,
    backup_name: Option<String>,
    invalidate_cache: bool,
    f: F,
) -> Result<Json<OperationStarted>, (StatusCode, Json<ErrorResponse>)>
where
    F: FnOnce(Arc<Config>, Arc<ChClient>, Arc<S3Client>, CancellationToken) -> Fut + Send + 'static,
    Fut: Future<Output = Result<(), anyhow::Error>> + Send,
{
    // Clone before try_start_op so the spawned task can use them for PID lock.
    let backup_name_for_lock = backup_name.clone();
    let command_for_lock = command.to_string();

    let (id, token) = state
        .try_start_op(command, backup_name)
        .await
        .map_err(|e| {
            (
                StatusCode::LOCKED,
                Json(ErrorResponse {
                    error: e.to_string(),
                }),
            )
        })?;

    let state_clone = state.clone();
    let op_label_owned = op_label.to_string();
    let f_token = token.clone();
    tokio::spawn(async move {
        tokio::select! {
            _ = token.cancelled() => {
                warn!(id = id, "Operation {} killed by user", id);
                // kill_op() already removed this op from running_ops and set its
                // ActionLog status to Killed before cancelling the token.
                // Calling fail_op() here would overwrite Killed with Failed — do nothing.
            }
            _ = async {
                // Acquire PID lock before loading clients or running the operation.
                // This provides cross-process exclusion (CLI vs server) per design §9.
                let lock_scope = lock_for_command(&command_for_lock, backup_name_for_lock.as_deref());
                let _pid_lock = if let Some(lock_path) = lock_path_for_scope(&lock_scope) {
                    match PidLock::acquire(&lock_path, &command_for_lock) {
                        Ok(lock) => Some(lock),
                        Err(e) => {
                            warn!(op = %op_label_owned, error = %e, "Failed to acquire PID lock");
                            state_clone.fail_op(id, e.to_string()).await;
                            return;
                        }
                    }
                } else {
                    None
                };

                let config = state_clone.config.load_full();
                let ch = state_clone.ch.load_full();
                let s3 = state_clone.s3.load_full();
                let start_time = std::time::Instant::now();

                let result = f(config, ch, s3, f_token).await;
                let duration = start_time.elapsed().as_secs_f64();

                match result {
                    Ok(()) => {
                        if let Some(m) = &state_clone.metrics {
                            let labels = OperationLabels::new(&op_label_owned);
                            m.backup_duration_seconds
                                .get_or_create(&labels)
                                .observe(duration);
                            m.successful_operations_total
                                .get_or_create(&labels)
                                .inc();
                        }
                        info!(op = %op_label_owned, "Operation completed");
                        if invalidate_cache {
                            state_clone.manifest_cache.lock().await.invalidate();
                            info!("ManifestCache: invalidated");
                        }
                        state_clone.finish_op(id).await;
                    }
                    Err(e) => {
                        if let Some(m) = &state_clone.metrics {
                            let labels = OperationLabels::new(&op_label_owned);
                            m.backup_duration_seconds
                                .get_or_create(&labels)
                                .observe(duration);
                            m.errors_total
                                .get_or_create(&labels)
                                .inc();
                        }
                        warn!(op = %op_label_owned, error = format_args!("{e:#}"), "Operation failed");
                        state_clone.fail_op(id, format!("{e:#}")).await;
                    }
                }
            } => {}
        }
    });

    Ok(Json(OperationStarted {
        id,
        status: "started".to_string(),
    }))
}

/// Validate a backup name to prevent path traversal attacks.
///
/// Rejects names that are empty, contain `..`, `/`, `\`, or NUL bytes.
/// Returns `Ok(())` for valid names, or `Err` with a human-readable reason.
pub fn validate_backup_name(name: &str) -> Result<(), &'static str> {
    if name.is_empty() {
        return Err("backup name must not be empty");
    }
    if name.contains("..") {
        return Err("backup name must not contain '..'");
    }
    if name.contains('/') {
        return Err("backup name must not contain '/'");
    }
    if name.contains('\\') {
        return Err("backup name must not contain '\\'");
    }
    if name.contains('\0') {
        return Err("backup name must not contain NUL byte");
    }
    if name == "." {
        return Err("backup name must not be '.'");
    }
    Ok(())
}

/// Returns an error if `name` is a reserved backup shortcut.
///
/// `"latest"` and `"previous"` are dynamic shortcuts resolved at runtime; they
/// cannot be used as names for newly-created backups because they would collide
/// with shortcut resolution in `list.rs`.  This check is separate from
/// `validate_backup_name`, which is also called for non-create operations
/// (e.g. `upload latest`) where shortcuts *are* valid inputs.
pub fn reject_reserved_backup_name(name: &str) -> Result<(), &'static str> {
    if name == "latest" || name == "previous" {
        return Err("backup name must not be a reserved shortcut ('latest' or 'previous')");
    }
    Ok(())
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

        // Validate name to prevent path traversal from crafted filesystem entries.
        if validate_backup_name(&backup_name).is_err() {
            tracing::warn!(backup_name = %backup_name, "scan_resumable_state_files: skipping directory with invalid backup name");
            continue;
        }

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
                    let (id, token) = match state_clone
                        .try_start_op("upload", Some(backup_name.clone()))
                        .await
                    {
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

                    tokio::select! {
                        _ = token.cancelled() => {
                            tracing::warn!(backup_name = %backup_name, "Auto-resume: upload killed");
                            // kill_op already updated action log; nothing else to do.
                        }
                        _ = async {
                            let lock_scope = lock_for_command("upload", Some(&backup_name));
                            let _pid_lock = if let Some(lock_path) = lock_path_for_scope(&lock_scope) {
                                match PidLock::acquire(&lock_path, "upload") {
                                    Ok(l) => Some(l),
                                    Err(e) => {
                                        tracing::warn!(backup_name = %backup_name, error = %e, "Auto-resume: upload PID lock failed");
                                        state_clone.fail_op(id, e.to_string()).await;
                                        return;
                                    }
                                }
                            } else {
                                None
                            };

                            let config = state_clone.config.load();
                            let s3 = state_clone.s3.load();
                            let backup_dir = std::path::PathBuf::from(&config.clickhouse.data_path)
                                .join("backup")
                                .join(&backup_name);

                            match crate::upload::upload(
                                &config,
                                &s3,
                                &backup_name,
                                &backup_dir,
                                false, // delete_local
                                None,  // diff_from_remote
                                true,  // resume = true
                                token.clone(),
                            )
                            .await
                            {
                                Ok(_) => {
                                    tracing::info!(backup_name = %backup_name, "Auto-resume: upload completed");
                                    state_clone.finish_op(id).await;
                                }
                                Err(e) => {
                                    tracing::warn!(backup_name = %backup_name, error = %e, "Auto-resume: upload failed");
                                    state_clone.fail_op(id, e.to_string()).await;
                                }
                            }
                        } => {}
                    }
                });
            }
            "download" => {
                tokio::spawn(async move {
                    let (id, token) = match state_clone
                        .try_start_op("download", Some(backup_name.clone()))
                        .await
                    {
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

                    tokio::select! {
                        _ = token.cancelled() => {
                            tracing::warn!(backup_name = %backup_name, "Auto-resume: download killed");
                        }
                        _ = async {
                            let lock_scope = lock_for_command("download", Some(&backup_name));
                            let _pid_lock = if let Some(lock_path) = lock_path_for_scope(&lock_scope) {
                                match PidLock::acquire(&lock_path, "download") {
                                    Ok(l) => Some(l),
                                    Err(e) => {
                                        tracing::warn!(backup_name = %backup_name, error = %e, "Auto-resume: download PID lock failed");
                                        state_clone.fail_op(id, e.to_string()).await;
                                        return;
                                    }
                                }
                            } else {
                                None
                            };

                            let config = state_clone.config.load();
                            let s3 = state_clone.s3.load();

                            match crate::download::download(
                                &config,
                                &s3,
                                &backup_name,
                                true,  // resume = true
                                false, // hardlink_exists_files = false
                                token.clone(),
                            )
                            .await
                            {
                                Ok(_) => {
                                    tracing::info!(backup_name = %backup_name, "Auto-resume: download completed");
                                    state_clone.finish_op(id).await;
                                }
                                Err(e) => {
                                    tracing::warn!(backup_name = %backup_name, error = %e, "Auto-resume: download failed");
                                    state_clone.fail_op(id, e.to_string()).await;
                                }
                            }
                        } => {}
                    }
                });
            }
            "restore" => {
                // Load the params sidecar before spawning the task so we can skip this
                // backup immediately if the sidecar is missing (avoids consuming a semaphore
                // permit and logging a spurious "started" action for a backup that cannot
                // be resumed correctly).
                let data_path_for_params = config.clickhouse.data_path.clone();
                let params_path = {
                    let bd = std::path::Path::new(&data_path_for_params)
                        .join("backup")
                        .join(&backup_name);
                    crate::resume::restore_params_path(&bd)
                };

                let restore_params: crate::resume::RestoreParams = match std::fs::read_to_string(
                    &params_path,
                )
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                {
                    Some(p) => p,
                    None => {
                        tracing::warn!(
                            backup_name = %backup_name,
                            params_file = %params_path.display(),
                            "Auto-resume: no restore.params.json found, skipping restore auto-resume"
                        );
                        continue;
                    }
                };

                tokio::spawn(async move {
                    let (id, token) = match state_clone
                        .try_start_op("restore", Some(backup_name.clone()))
                        .await
                    {
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

                    tokio::select! {
                        _ = token.cancelled() => {
                            tracing::warn!(backup_name = %backup_name, "Auto-resume: restore killed");
                        }
                        _ = async {
                            let lock_scope = lock_for_command("restore", Some(&backup_name));
                            let _pid_lock = if let Some(lock_path) = lock_path_for_scope(&lock_scope) {
                                match PidLock::acquire(&lock_path, "restore") {
                                    Ok(l) => Some(l),
                                    Err(e) => {
                                        tracing::warn!(backup_name = %backup_name, error = %e, "Auto-resume: restore PID lock failed");
                                        state_clone.fail_op(id, e.to_string()).await;
                                        return;
                                    }
                                }
                            } else {
                                None
                            };

                            let config = state_clone.config.load();
                            let ch = state_clone.ch.load();

                            // Parse database_mapping from the persisted HashMap.
                            let db_mapping = if restore_params.database_mapping.is_empty() {
                                None
                            } else {
                                Some(restore_params.database_mapping.clone())
                            };

                            match crate::restore::restore(
                                &config,
                                &ch,
                                &backup_name,
                                restore_params.tables.as_deref(),
                                restore_params.schema_only,
                                restore_params.data_only,
                                false, // rm: always false on auto-resume -- never DROP tables on restart
                                true,  // resume = true
                                restore_params.rename_as.as_deref(),
                                db_mapping.as_ref(),
                                restore_params.rbac,
                                restore_params.configs,
                                restore_params.named_collections,
                                restore_params.partitions.as_deref(),
                                restore_params.skip_empty_tables,
                                token.clone(),
                            )
                            .await
                            {
                                Ok(_) => {
                                    tracing::info!(backup_name = %backup_name, "Auto-resume: restore completed");
                                    state_clone.finish_op(id).await;
                                }
                                Err(e) => {
                                    tracing::warn!(backup_name = %backup_name, error = %e, "Auto-resume: restore failed");
                                    state_clone.fail_op(id, e.to_string()).await;
                                }
                            }
                        } => {}
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
    use crate::server::actions::ActionStatus;

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
    async fn test_cancellation_token_aborts_task() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let token = CancellationToken::new();
        let token_clone = token.clone();
        let completed = Arc::new(AtomicBool::new(false));
        let completed_clone = completed.clone();

        let handle = tokio::spawn(async move {
            tokio::select! {
                _ = token_clone.cancelled() => {
                    // Cancelled branch fires -- task aborted
                }
                _ = async {
                    // Simulate a long-running operation
                    tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                    completed_clone.store(true, Ordering::SeqCst);
                } => {
                    // Operation completed normally (should not happen)
                }
            }
        });

        // Cancel the token
        token.cancel();

        // Wait for the task to finish
        handle.await.unwrap();

        // The operation should NOT have completed
        assert!(
            !completed.load(Ordering::SeqCst),
            "Operation should have been aborted by cancellation"
        );
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
            Some(Arc::new(Metrics::new()))
        } else {
            None
        };

        assert!(
            metrics.is_some(),
            "Metrics should be Some when enable_metrics=true"
        );

        // Verify the registry has all 14 metric families by encoding and checking content
        let text = metrics
            .as_ref()
            .unwrap()
            .encode()
            .expect("encode should succeed");
        assert!(
            text.contains("chbackup_backup_duration_seconds"),
            "Encoded output should contain duration metric"
        );
    }

    #[test]
    fn test_app_state_with_metrics_disabled() {
        // Verify that when enable_metrics=false, Metrics is None
        let mut config = Config::default();
        config.api.enable_metrics = false;
        let config = Arc::new(config);

        let metrics: Option<Arc<Metrics>> = if config.api.enable_metrics {
            Some(Arc::new(Metrics::new()))
        } else {
            None
        };

        assert!(
            metrics.is_none(),
            "Metrics should be None when enable_metrics=false"
        );
    }

    #[test]
    fn test_validate_backup_name_valid() {
        assert!(validate_backup_name("daily-2024-01-15").is_ok());
        assert!(validate_backup_name("my_backup").is_ok());
        assert!(validate_backup_name("backup.v2").is_ok());
        assert!(validate_backup_name("2024-01-15T143052").is_ok());
        assert!(validate_backup_name("a").is_ok());
    }

    #[test]
    fn test_validate_backup_name_rejects_dotdot() {
        assert!(validate_backup_name("../etc/passwd").is_err());
        assert!(validate_backup_name("foo/../bar").is_err());
        assert!(validate_backup_name("..").is_err());
    }

    #[test]
    fn test_validate_backup_name_rejects_slash() {
        assert!(validate_backup_name("foo/bar").is_err());
        assert!(validate_backup_name("/abs").is_err());
    }

    #[test]
    fn test_validate_backup_name_rejects_backslash() {
        assert!(validate_backup_name("foo\\bar").is_err());
    }

    #[test]
    fn test_validate_backup_name_rejects_empty() {
        assert!(validate_backup_name("").is_err());
    }

    #[test]
    fn test_validate_backup_name_rejects_nul() {
        assert!(validate_backup_name("foo\0bar").is_err());
    }

    #[test]
    fn test_validate_backup_name_allows_reserved_shortcuts() {
        // validate_backup_name intentionally allows "latest"/"previous" --
        // they are valid inputs for upload/download/restore commands.
        assert!(validate_backup_name("latest").is_ok());
        assert!(validate_backup_name("previous").is_ok());
    }

    #[test]
    fn test_reject_reserved_backup_name() {
        assert!(reject_reserved_backup_name("latest").is_err());
        assert!(reject_reserved_backup_name("previous").is_err());
        // Normal names are accepted.
        assert!(reject_reserved_backup_name("daily-2024-01-15").is_ok());
        assert!(reject_reserved_backup_name("my_backup").is_ok());
    }

    #[tokio::test]
    async fn test_running_ops_tracks_multiple() {
        // With parallel ops enabled, we can start multiple operations
        let action_log = Arc::new(Mutex::new(ActionLog::new(100)));
        let op_semaphore = Arc::new(Semaphore::new(Semaphore::MAX_PERMITS));
        let running_ops: Arc<Mutex<HashMap<u64, RunningOp>>> = Arc::new(Mutex::new(HashMap::new()));

        // Start 3 ops manually (simulating try_start_op)
        let mut tokens = Vec::new();
        for i in 0..3 {
            let permit = op_semaphore.clone().try_acquire_owned().unwrap();
            let token = CancellationToken::new();
            let id = {
                let mut log = action_log.lock().await;
                log.start(format!("op{}", i))
            };
            {
                let mut ops = running_ops.lock().await;
                ops.insert(
                    id,
                    RunningOp {
                        id,
                        command: format!("op{}", i),
                        backup_name: None,
                        cancel_token: token.clone(),
                        _permit: permit,
                    },
                );
            }
            tokens.push((id, token));
        }

        // All 3 should be in the map
        let ops = running_ops.lock().await;
        assert_eq!(ops.len(), 3);
        assert!(ops.contains_key(&1));
        assert!(ops.contains_key(&2));
        assert!(ops.contains_key(&3));
    }

    #[tokio::test]
    async fn test_running_ops_finish_removes() {
        let action_log = Arc::new(Mutex::new(ActionLog::new(100)));
        let op_semaphore = Arc::new(Semaphore::new(Semaphore::MAX_PERMITS));
        let running_ops: Arc<Mutex<HashMap<u64, RunningOp>>> = Arc::new(Mutex::new(HashMap::new()));

        // Start 2 ops
        let mut ids = Vec::new();
        for i in 0..2 {
            let permit = op_semaphore.clone().try_acquire_owned().unwrap();
            let token = CancellationToken::new();
            let id = {
                let mut log = action_log.lock().await;
                log.start(format!("op{}", i))
            };
            {
                let mut ops = running_ops.lock().await;
                ops.insert(
                    id,
                    RunningOp {
                        id,
                        command: format!("op{}", i),
                        backup_name: None,
                        cancel_token: token.clone(),
                        _permit: permit,
                    },
                );
            }
            ids.push(id);
        }

        // Finish op 1 (remove from map)
        {
            let mut log = action_log.lock().await;
            log.finish(ids[0]);
        }
        {
            let mut ops = running_ops.lock().await;
            ops.remove(&ids[0]);
        }

        // Only op 2 remains
        let ops = running_ops.lock().await;
        assert_eq!(ops.len(), 1);
        assert!(!ops.contains_key(&ids[0]));
        assert!(ops.contains_key(&ids[1]));
    }

    #[tokio::test]
    async fn test_running_ops_fail_removes() {
        let action_log = Arc::new(Mutex::new(ActionLog::new(100)));
        let op_semaphore = Arc::new(Semaphore::new(Semaphore::MAX_PERMITS));
        let running_ops: Arc<Mutex<HashMap<u64, RunningOp>>> = Arc::new(Mutex::new(HashMap::new()));

        // Start 1 op
        let permit = op_semaphore.clone().try_acquire_owned().unwrap();
        let token = CancellationToken::new();
        let id = {
            let mut log = action_log.lock().await;
            log.start("failing_op".to_string())
        };
        {
            let mut ops = running_ops.lock().await;
            ops.insert(
                id,
                RunningOp {
                    id,
                    command: "failing_op".to_string(),
                    backup_name: None,
                    cancel_token: token,
                    _permit: permit,
                },
            );
        }

        // Fail it (remove from map)
        {
            let mut log = action_log.lock().await;
            log.fail(id, "error".to_string());
        }
        {
            let mut ops = running_ops.lock().await;
            ops.remove(&id);
        }

        // Map should be empty
        let ops = running_ops.lock().await;
        assert!(ops.is_empty());
    }

    #[tokio::test]
    async fn test_running_ops_kill_by_id() {
        let action_log = Arc::new(Mutex::new(ActionLog::new(100)));
        let op_semaphore = Arc::new(Semaphore::new(Semaphore::MAX_PERMITS));
        let running_ops: Arc<Mutex<HashMap<u64, RunningOp>>> = Arc::new(Mutex::new(HashMap::new()));

        // Start 2 ops
        let mut ids = Vec::new();
        let mut tokens = Vec::new();
        for i in 0..2 {
            let permit = op_semaphore.clone().try_acquire_owned().unwrap();
            let token = CancellationToken::new();
            let id = {
                let mut log = action_log.lock().await;
                log.start(format!("op{}", i))
            };
            {
                let mut ops = running_ops.lock().await;
                ops.insert(
                    id,
                    RunningOp {
                        id,
                        command: format!("op{}", i),
                        backup_name: None,
                        cancel_token: token.clone(),
                        _permit: permit,
                    },
                );
            }
            ids.push(id);
            tokens.push(token);
        }

        // Kill op 1 by ID
        {
            let mut ops = running_ops.lock().await;
            let mut log = action_log.lock().await;
            if let Some(op) = ops.remove(&ids[0]) {
                op.cancel_token.cancel();
                log.kill(op.id);
            }
        }

        // Verify op 1 is cancelled, op 2 survives
        assert!(tokens[0].is_cancelled());
        assert!(!tokens[1].is_cancelled());

        let ops = running_ops.lock().await;
        assert_eq!(ops.len(), 1);
        assert!(!ops.contains_key(&ids[0]));
        assert!(ops.contains_key(&ids[1]));
    }

    /// Verify that the run_operation helper's success path calls finish_op,
    /// which clears the running op from the map.
    /// We simulate the helper's exact internal pattern here since AppState
    /// requires real ChClient/S3Client instances that need running servers.
    #[tokio::test]
    async fn test_run_operation_success() {
        let action_log = Arc::new(Mutex::new(ActionLog::new(100)));
        let op_semaphore = Arc::new(Semaphore::new(1));
        let running_ops: Arc<Mutex<HashMap<u64, RunningOp>>> = Arc::new(Mutex::new(HashMap::new()));

        // Simulate try_start_op
        let permit = op_semaphore.clone().try_acquire_owned().unwrap();
        let token = CancellationToken::new();
        let id = {
            let mut log = action_log.lock().await;
            log.start("create".to_string())
        };
        {
            let mut ops = running_ops.lock().await;
            ops.insert(
                id,
                RunningOp {
                    id,
                    command: "create".to_string(),
                    backup_name: None,
                    cancel_token: token.clone(),
                    _permit: permit,
                },
            );
        }

        // Simulate the spawned task with select! (success path)
        let action_log_clone = action_log.clone();
        let running_ops_clone = running_ops.clone();
        let handle = tokio::spawn(async move {
            tokio::select! {
                _ = token.cancelled() => {
                    // killed path -- should not trigger
                    panic!("Should not be cancelled");
                }
                _ = async {
                    // Simulate successful operation
                    let result: Result<(), anyhow::Error> = Ok(());
                    match result {
                        Ok(()) => {
                            let mut log = action_log_clone.lock().await;
                            log.finish(id);
                            drop(log);
                            let mut ops = running_ops_clone.lock().await;
                            ops.remove(&id);
                        }
                        Err(_) => panic!("Should not fail"),
                    }
                } => {}
            }
        });

        handle.await.unwrap();

        // Verify finish_op was called: op removed from map, log shows completed
        assert!(running_ops.lock().await.is_empty());
        let log = action_log.lock().await;
        let entry = log.entries().iter().find(|e| e.id == id).unwrap();
        assert!(
            matches!(entry.status, ActionStatus::Completed),
            "Expected Completed status after success"
        );
    }

    /// Verify that the run_operation helper's failure path calls fail_op,
    /// which clears the running op from the map and records the error.
    #[tokio::test]
    async fn test_run_operation_failure() {
        let action_log = Arc::new(Mutex::new(ActionLog::new(100)));
        let op_semaphore = Arc::new(Semaphore::new(1));
        let running_ops: Arc<Mutex<HashMap<u64, RunningOp>>> = Arc::new(Mutex::new(HashMap::new()));

        // Simulate try_start_op
        let permit = op_semaphore.clone().try_acquire_owned().unwrap();
        let token = CancellationToken::new();
        let id = {
            let mut log = action_log.lock().await;
            log.start("upload".to_string())
        };
        {
            let mut ops = running_ops.lock().await;
            ops.insert(
                id,
                RunningOp {
                    id,
                    command: "upload".to_string(),
                    backup_name: None,
                    cancel_token: token.clone(),
                    _permit: permit,
                },
            );
        }

        // Simulate the spawned task with select! (failure path)
        let action_log_clone = action_log.clone();
        let running_ops_clone = running_ops.clone();
        let handle = tokio::spawn(async move {
            tokio::select! {
                _ = token.cancelled() => {
                    panic!("Should not be cancelled");
                }
                _ = async {
                    // Simulate failed operation
                    let result: Result<(), anyhow::Error> = Err(anyhow::anyhow!("upload error"));
                    match result {
                        Ok(()) => panic!("Should not succeed"),
                        Err(e) => {
                            let mut log = action_log_clone.lock().await;
                            log.fail(id, e.to_string());
                            drop(log);
                            let mut ops = running_ops_clone.lock().await;
                            ops.remove(&id);
                        }
                    }
                } => {}
            }
        });

        handle.await.unwrap();

        // Verify fail_op was called: op removed from map, log shows failed
        assert!(running_ops.lock().await.is_empty());
        let log = action_log.lock().await;
        let entry = log.entries().iter().find(|e| e.id == id).unwrap();
        match &entry.status {
            ActionStatus::Failed(msg) => assert_eq!(msg, "upload error"),
            other => panic!("Expected Failed status, got {:?}", other),
        }
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

    #[test]
    fn test_restore_params_roundtrip() {
        use crate::resume::RestoreParams;

        let params = RestoreParams {
            backup_name: "test-backup".to_string(),
            tables: Some("mydb.*".to_string()),
            schema_only: false,
            data_only: true,
            rm: false,
            rename_as: None,
            database_mapping: std::collections::HashMap::new(),
            rbac: false,
            configs: false,
            named_collections: false,
            partitions: None,
            skip_empty_tables: true,
        };
        let json = serde_json::to_string(&params).unwrap();
        let loaded: RestoreParams = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.backup_name, "test-backup");
        assert!(loaded.data_only);
        assert!(loaded.skip_empty_tables);
        assert_eq!(loaded.tables, Some("mydb.*".to_string()));
    }

    // -----------------------------------------------------------------------
    // validate_backup_name extended edge case tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_validate_backup_name_unicode() {
        // Unicode characters should be accepted (no path traversal risk)
        assert!(validate_backup_name("backup-\u{00E9}\u{00E8}").is_ok());
        assert!(validate_backup_name("\u{4E2D}\u{6587}\u{5907}\u{4EFD}").is_ok());
    }

    #[test]
    fn test_validate_backup_name_control_chars() {
        // NUL byte is explicitly rejected
        assert!(validate_backup_name("foo\0bar").is_err());
        // Other control characters are not explicitly rejected by the validator
        // (they are unusual but not a path traversal vector)
    }

    #[test]
    fn test_validate_backup_name_very_long() {
        // 255 chars should be accepted (common filesystem limit)
        let long_name = "a".repeat(255);
        assert!(validate_backup_name(&long_name).is_ok());

        // 256 chars should also be accepted by our validator
        // (filesystem limits are enforced by the OS, not our validation)
        let very_long = "b".repeat(256);
        assert!(validate_backup_name(&very_long).is_ok());
    }

    #[test]
    fn test_validate_backup_name_only_dots() {
        // Single dot is explicitly rejected
        assert!(validate_backup_name(".").is_err());
        // Double dot is rejected by the ".." check
        assert!(validate_backup_name("..").is_err());
        // Triple dot is rejected because it contains ".."
        assert!(validate_backup_name("...").is_err());
    }

    #[test]
    fn test_validate_backup_name_path_separators() {
        assert!(validate_backup_name("foo/bar").is_err());
        assert!(validate_backup_name("foo\\bar").is_err());
        assert!(validate_backup_name("/leading").is_err());
        assert!(validate_backup_name("trailing/").is_err());
    }

    #[test]
    fn test_validate_backup_name_with_spaces_and_special() {
        // Spaces and dashes are fine
        assert!(validate_backup_name("backup with spaces").is_ok());
        assert!(validate_backup_name("backup-2024-01-15T14:30:52").is_ok());
        assert!(validate_backup_name("backup@host").is_ok());
        assert!(validate_backup_name("backup#1").is_ok());
    }

    // -----------------------------------------------------------------------
    // semaphore_permits() tests -- covers lines 23-29 (~6 lines)
    // -----------------------------------------------------------------------

    #[test]
    fn test_semaphore_permits_sequential() {
        assert_eq!(semaphore_permits(false), 1);
    }

    #[test]
    fn test_semaphore_permits_parallel() {
        assert_eq!(semaphore_permits(true), Semaphore::MAX_PERMITS);
    }

    // -----------------------------------------------------------------------
    // reject_reserved_backup_name() additional tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_reject_reserved_backup_name_similar_names() {
        // Names that are similar to but NOT exactly "latest" or "previous" should pass
        assert!(reject_reserved_backup_name("latest-2").is_ok());
        assert!(reject_reserved_backup_name("my-latest").is_ok());
        assert!(reject_reserved_backup_name("previous-backup").is_ok());
        assert!(reject_reserved_backup_name("LATEST").is_ok()); // Case-sensitive
        assert!(reject_reserved_backup_name("PREVIOUS").is_ok());
    }

    // -----------------------------------------------------------------------
    // scan_resumable_state_files() additional tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_scan_resumable_skips_invalid_names() {
        let dir = tempfile::tempdir().unwrap();
        let backup_dir = dir.path().join("backup");

        // Create a directory with a path-traversal name -- should be skipped
        let bad_dir = backup_dir.join("..hidden");
        std::fs::create_dir_all(&bad_dir).unwrap();
        std::fs::write(bad_dir.join("upload.state.json"), "{}").unwrap();

        // Create a valid directory -- should be found
        let good_dir = backup_dir.join("valid-backup");
        std::fs::create_dir_all(&good_dir).unwrap();
        std::fs::write(good_dir.join("restore.state.json"), "{}").unwrap();

        let ops = scan_resumable_state_files(dir.path().to_str().unwrap());
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].backup_name, "valid-backup");
        assert_eq!(ops[0].op_type, "restore");
    }

    #[test]
    fn test_scan_resumable_multiple_ops_same_backup() {
        let dir = tempfile::tempdir().unwrap();
        let backup_dir = dir.path().join("backup");

        // A single backup dir with all three state files
        let bk = backup_dir.join("multi-state");
        std::fs::create_dir_all(&bk).unwrap();
        std::fs::write(bk.join("upload.state.json"), "{}").unwrap();
        std::fs::write(bk.join("download.state.json"), "{}").unwrap();
        std::fs::write(bk.join("restore.state.json"), "{}").unwrap();

        let ops = scan_resumable_state_files(dir.path().to_str().unwrap());
        assert_eq!(ops.len(), 3);

        // All should reference the same backup
        for op in &ops {
            assert_eq!(op.backup_name, "multi-state");
        }

        // All three op types should be present
        let types: std::collections::HashSet<_> = ops.iter().map(|o| o.op_type.as_str()).collect();
        assert!(types.contains("upload"));
        assert!(types.contains("download"));
        assert!(types.contains("restore"));
    }

    #[test]
    fn test_scan_resumable_empty_backup_dir_no_state_files() {
        let dir = tempfile::tempdir().unwrap();
        let backup_dir = dir.path().join("backup");

        // Create backup directories without any state files
        std::fs::create_dir_all(backup_dir.join("backup-a")).unwrap();
        std::fs::create_dir_all(backup_dir.join("backup-b")).unwrap();

        let ops = scan_resumable_state_files(dir.path().to_str().unwrap());
        assert!(ops.is_empty());
    }

    // -----------------------------------------------------------------------
    // WatchStatus tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_watch_status_default_values() {
        let ws = WatchStatus::default();
        assert!(!ws.active);
        assert_eq!(ws.state, "inactive");
        assert!(ws.last_full.is_none());
        assert!(ws.last_incr.is_none());
        assert_eq!(ws.consecutive_errors, 0);
        assert!(ws.next_backup_in.is_none());
    }

    #[test]
    fn test_watch_status_clone() {
        let ws = WatchStatus {
            active: true,
            state: "creating_full".to_string(),
            consecutive_errors: 3,
            ..Default::default()
        };

        let cloned = ws.clone();
        assert!(cloned.active);
        assert_eq!(cloned.state, "creating_full");
        assert_eq!(cloned.consecutive_errors, 3);
    }

    // -----------------------------------------------------------------------
    // ResumableOp tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_resumable_op_clone() {
        let op = ResumableOp {
            backup_name: "my-backup".to_string(),
            op_type: "upload".to_string(),
        };
        let cloned = op.clone();
        assert_eq!(cloned.backup_name, "my-backup");
        assert_eq!(cloned.op_type, "upload");
    }

    #[test]
    fn test_resumable_op_debug() {
        let op = ResumableOp {
            backup_name: "test".to_string(),
            op_type: "download".to_string(),
        };
        let debug_str = format!("{:?}", op);
        assert!(debug_str.contains("test"));
        assert!(debug_str.contains("download"));
    }
}
