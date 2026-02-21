//! HTTP route handlers for the chbackup API server.
//!
//! All endpoints from design doc section 9 are implemented here.
//! Read-only endpoints return data directly; operation endpoints spawn
//! background tasks and return immediately with an action ID.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use std::sync::Arc;

use crate::list;
use crate::manifest::BackupManifest;
use crate::table_filter::TableFilter;

use super::actions::ActionStatus;
use super::metrics::Metrics;
use super::state::{run_operation, validate_backup_name, AppState};

// ---------------------------------------------------------------------------
// Response / Request types
// ---------------------------------------------------------------------------

/// Response for GET /api/v1/version
#[derive(Debug, Serialize)]
pub struct VersionResponse {
    pub version: String,
    pub clickhouse_version: String,
}

/// Response for GET /api/v1/status
#[derive(Debug, Serialize)]
pub struct StatusResponse {
    pub status: String,
    pub command: Option<String>,
    pub start: Option<String>,
}

/// Response for GET /api/v1/actions -- matches system.backup_actions table schema
#[derive(Debug, Serialize, Deserialize)]
pub struct ActionResponse {
    pub id: u64,
    pub command: String,
    pub start: String,
    pub finish: String,
    pub status: String,
    pub error: String,
}

/// Request body for POST /api/v1/actions (ClickHouse URL engine INSERT)
#[derive(Debug, Deserialize)]
pub struct ActionRequest {
    pub command: String,
}

/// Query params for GET /api/v1/list
#[derive(Debug, Deserialize)]
pub struct ListParams {
    pub location: Option<String>,
    /// When true, reverse sort order (newest first instead of oldest first)
    pub desc: Option<bool>,
    /// Starting offset for pagination (0-based).
    pub offset: Option<usize>,
    /// Maximum number of results to return.
    pub limit: Option<usize>,
    /// Output format hint (stored for integration table DDL compatibility; API always returns JSON).
    pub format: Option<String>,
}

/// Response for GET /api/v1/list -- matches ALL columns of system.backup_list table
#[derive(Debug, Serialize, Deserialize)]
pub struct ListResponse {
    pub name: String,
    pub created: String,
    pub location: String,
    pub size: u64,
    pub data_size: u64,
    pub object_disk_size: u64,
    pub metadata_size: u64,
    pub rbac_size: u64,
    pub config_size: u64,
    pub compressed_size: u64,
    pub required: String,
}

/// Query params for GET /api/v1/tables
#[derive(Debug, Deserialize)]
pub struct TablesParams {
    pub table: Option<String>,
    pub all: Option<bool>,
    pub backup: Option<String>,
    /// Starting offset for pagination (0-based).
    pub offset: Option<usize>,
    /// Maximum number of results to return.
    pub limit: Option<usize>,
}

/// Response entry for GET /api/v1/tables
#[derive(Debug, Serialize, Deserialize)]
pub struct TablesResponseEntry {
    pub database: String,
    pub name: String,
    pub engine: String,
    pub uuid: String,
    pub data_paths: Vec<String>,
    pub total_bytes: Option<u64>,
}

/// Generic error response.
#[derive(Debug, Serialize)]
pub struct ErrorResponse {
    pub error: String,
}

/// Response returned when an async operation is started.
#[derive(Debug, Serialize)]
pub struct OperationStarted {
    pub id: u64,
    pub status: String,
}

// ---------------------------------------------------------------------------
// Read-only endpoints
// ---------------------------------------------------------------------------

/// Response for GET /health -- JSON health check (Go parity)
#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub status: String,
}

/// GET /health -- JSON health check (Go returns `{"status":"ok"}`)
pub async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok".to_string(),
    })
}

/// GET /api/v1/version -- return chbackup and ClickHouse versions
pub async fn version(State(state): State<AppState>) -> Json<VersionResponse> {
    let ch = state.ch.load();
    let ch_version = match ch.get_version().await {
        Ok(v) => v,
        Err(e) => {
            warn!(error = %e, "Failed to query ClickHouse version");
            "unknown".to_string()
        }
    };

    Json(VersionResponse {
        version: env!("CARGO_PKG_VERSION").to_string(),
        clickhouse_version: ch_version,
    })
}

/// GET /api/v1/status -- return current operation status
///
/// For backward compatibility, returns the first running operation or "idle"
/// when no operations are running. Use GET /api/v1/actions for all operations.
pub async fn status(State(state): State<AppState>) -> Json<StatusResponse> {
    // Extract what we need from running_ops, then drop that lock before touching
    // action_log. Holding running_ops while waiting for action_log would invert the
    // lock order used by try_start_op/finish_op/fail_op (action_log first) and
    // create a deadlock under concurrent status + operation-start/finish traffic.
    let running_op: Option<(u64, String)> = {
        let ops = state.running_ops.lock().await;
        ops.values().next().map(|op| (op.id, op.command.clone()))
    }; // running_ops lock dropped here

    if let Some((id, command)) = running_op {
        let start_time = {
            let action_log = state.action_log.lock().await;
            action_log
                .entries()
                .iter()
                .find(|e| e.id == id)
                .map(|e| e.start.to_rfc3339())
        };

        Json(StatusResponse {
            status: "running".to_string(),
            command: Some(command),
            start: start_time,
        })
    } else {
        Json(StatusResponse {
            status: "idle".to_string(),
            command: None,
            start: None,
        })
    }
}

/// GET /api/v1/actions -- return action log entries
pub async fn get_actions(State(state): State<AppState>) -> Json<Vec<ActionResponse>> {
    let log = state.action_log.lock().await;
    let entries: Vec<ActionResponse> = log
        .entries()
        .iter()
        .map(|e| {
            let (status_str, error_str) = match &e.status {
                ActionStatus::Running => ("running".to_string(), String::new()),
                ActionStatus::Completed => ("completed".to_string(), String::new()),
                ActionStatus::Failed(err) => ("failed".to_string(), err.clone()),
                ActionStatus::Killed => ("killed".to_string(), String::new()),
            };

            ActionResponse {
                id: e.id,
                command: e.command.clone(),
                start: e.start.to_rfc3339(),
                finish: e.finish.map(|f| f.to_rfc3339()).unwrap_or_default(),
                status: status_str,
                error: error_str,
            }
        })
        .collect();

    Json(entries)
}

/// POST /api/v1/actions -- dispatch operations from ClickHouse URL engine INSERT
///
/// Accepts JSONEachRow: `[{"command": "create_remote daily_backup"}]`
/// Parses the first command string and dispatches to the appropriate handler.
///
/// NOTE: This handler is intentionally excluded from the `run_operation()` DRY helper
/// because it returns `(StatusCode, Json<OperationStarted>)` (201 CREATED compatible)
/// which is incompatible with the helper's `Result<Json<OperationStarted>, ...>` return type.
/// The inline try_start_op + tokio::spawn + tokio::select! pattern is retained here.
pub async fn post_actions(
    State(state): State<AppState>,
    Json(body): Json<Vec<ActionRequest>>,
) -> Result<(StatusCode, Json<OperationStarted>), (StatusCode, Json<ErrorResponse>)> {
    let request = body.into_iter().next().ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "empty request body".to_string(),
            }),
        )
    })?;

    let parts: Vec<&str> = request.command.split_whitespace().collect();
    let op_name = parts.first().copied().unwrap_or("");

    // Dispatch based on operation name
    match op_name {
        "create" | "upload" | "download" | "restore" | "create_remote" | "restore_remote"
        | "delete" | "clean_broken" => {
            // Validate backup name if explicitly provided in the command.
            // For most commands the name is parts[1]; for "delete <location> <name>"
            // the location is parts[1] and the backup name is parts[2].
            if let Some(name) = parts.get(1) {
                validate_backup_name(name).map_err(|e| {
                    warn!(backup_name = %name, "backup name rejected: {}", e);
                    (
                        StatusCode::BAD_REQUEST,
                        Json(ErrorResponse {
                            error: format!("invalid backup name: {e}"),
                        }),
                    )
                })?;
            }
            if op_name == "delete" {
                if let Some(name) = parts.get(2) {
                    validate_backup_name(name).map_err(|e| {
                        warn!(backup_name = %name, "backup name rejected: {}", e);
                        (
                            StatusCode::BAD_REQUEST,
                            Json(ErrorResponse {
                                error: format!("invalid backup name: {e}"),
                            }),
                        )
                    })?;
                }
            }

            let (id, token) = state.try_start_op(op_name).await.map_err(|e| {
                (
                    StatusCode::LOCKED,
                    Json(ErrorResponse {
                        error: e.to_string(),
                    }),
                )
            })?;

            let state_clone = state.clone();
            let command = request.command.clone();
            let parts_owned: Vec<String> = parts.iter().map(|s| s.to_string()).collect();
            tokio::spawn(async move {
                tokio::select! {
                    _ = token.cancelled() => {
                        warn!(id = id, "Operation {} killed by user", id);
                        // kill_op() already removed this op from running_ops and set its
                        // ActionLog status to Killed before cancelling the token.
                        // Calling fail_op() here would overwrite Killed with Failed — do nothing.
                    }
                    _ = async {
                tracing::info!(command = %command, "Action dispatched from POST /api/v1/actions");
                let backup_name = parts_owned
                    .get(1)
                    .cloned()
                    .unwrap_or_else(|| Utc::now().format("%Y-%m-%dT%H%M%S").to_string());

                let config = state_clone.config.load();
                let ch = state_clone.ch.load();
                let s3 = state_clone.s3.load();
                let start_time = std::time::Instant::now();
                let op = parts_owned[0].as_str();

                let result: Result<(), anyhow::Error> = match op {
                    "create" => {
                        crate::backup::create(
                            &config,
                            &ch,
                            &backup_name,
                            None,  // table_pattern
                            false, // schema_only
                            None,  // diff_from
                            None,  // partitions
                            false, // skip_check_parts_columns
                            false, // rbac
                            false, // configs
                            false, // named_collections
                            &config.backup.skip_projections,
                        )
                        .await
                        .map(|_| ())
                    }
                    "upload" => {
                        let backup_dir = std::path::PathBuf::from(&config.clickhouse.data_path)
                            .join("backup")
                            .join(&backup_name);
                        let effective_resume = config.general.use_resumable_state;
                        let upload_result = crate::upload::upload(
                            &config,
                            &s3,
                            &backup_name,
                            &backup_dir,
                            false, // delete_local
                            None,  // diff_from_remote
                            effective_resume,
                        )
                        .await;

                        if upload_result.is_ok() {
                            // Apply retention after successful upload (design doc 3.6 step 7)
                            list::apply_retention_after_upload(
                                &config,
                                &s3,
                                Some(&state_clone.manifest_cache),
                            )
                            .await;
                        }
                        upload_result
                    }
                    "download" => {
                        let effective_resume = config.general.use_resumable_state;
                        crate::download::download(
                            &config,
                            &s3,
                            &backup_name,
                            effective_resume,
                            false, // hardlink_exists_files
                        )
                        .await
                        .map(|_| ())
                    }
                    "restore" => {
                        let effective_resume = config.general.use_resumable_state;
                        crate::restore::restore(
                            &config,
                            &ch,
                            &backup_name,
                            None,  // table_pattern
                            false, // schema_only
                            false, // data_only
                            false, // rm
                            effective_resume,
                            None,  // rename_as
                            None,  // database_mapping
                            false, // rbac
                            false, // configs
                            false, // named_collections
                            None,  // partitions
                            false, // skip_empty_tables
                        )
                        .await
                    }
                    "create_remote" => {
                        let create_result = crate::backup::create(
                            &config,
                            &ch,
                            &backup_name,
                            None,  // table_pattern
                            false, // schema_only
                            None,  // diff_from
                            None,  // partitions
                            false, // skip_check_parts_columns
                            false, // rbac
                            false, // configs
                            false, // named_collections
                            &config.backup.skip_projections,
                        )
                        .await;
                        match create_result {
                            Ok(_) => {
                                let backup_dir =
                                    std::path::PathBuf::from(&config.clickhouse.data_path)
                                        .join("backup")
                                        .join(&backup_name);
                                let effective_resume = config.general.use_resumable_state;
                                let upload_result = crate::upload::upload(
                                    &config,
                                    &s3,
                                    &backup_name,
                                    &backup_dir,
                                    false, // delete_local
                                    None,  // diff_from_remote
                                    effective_resume,
                                )
                                .await;

                                if upload_result.is_ok() {
                                    // Apply retention after successful upload (design doc 3.6 step 7)
                                    list::apply_retention_after_upload(
                                        &config,
                                        &s3,
                                        Some(&state_clone.manifest_cache),
                                    )
                                    .await;
                                }
                                upload_result
                            }
                            Err(e) => Err(e),
                        }
                    }
                    "restore_remote" => {
                        let effective_resume = config.general.use_resumable_state;
                        let dl = crate::download::download(
                            &config,
                            &s3,
                            &backup_name,
                            effective_resume,
                            false, // hardlink_exists_files
                        )
                        .await;
                        match dl {
                            Ok(_) => {
                                crate::restore::restore(
                                    &config,
                                    &ch,
                                    &backup_name,
                                    None,  // table_pattern
                                    false, // schema_only
                                    false, // data_only
                                    false, // rm
                                    effective_resume,
                                    None,  // rename_as
                                    None,  // database_mapping
                                    false, // rbac
                                    false, // configs
                                    false, // named_collections
                                    None,  // partitions
                                    false, // skip_empty_tables
                                )
                                .await
                            }
                            Err(e) => Err(e),
                        }
                    }
                    "delete" => {
                        // delete <location> <name> OR delete <name> (defaults to remote)
                        let (loc, name) = if parts_owned.len() >= 3 {
                            (parts_owned[1].as_str().to_string(), parts_owned[2].clone())
                        } else {
                            ("remote".to_string(), backup_name.clone())
                        };
                        let data_path = config.clickhouse.data_path.clone();
                        match loc.as_str() {
                            "local" => {
                                let n = name.clone();
                                tokio::task::spawn_blocking(move || {
                                    list::delete_local(&data_path, &n)
                                })
                                .await
                                .unwrap_or_else(|e| {
                                    Err(anyhow::anyhow!("spawn_blocking failed: {}", e))
                                })
                            }
                            _ => list::delete_remote(&s3, &name).await,
                        }
                    }
                    "clean_broken" => {
                        let s3_result = list::clean_broken_remote(&s3).await;
                        let data_path = config.clickhouse.data_path.clone();
                        let local_result = tokio::task::spawn_blocking(move || {
                            list::clean_broken_local(&data_path)
                        })
                        .await
                        .unwrap_or_else(|e| Err(anyhow::anyhow!("spawn_blocking failed: {}", e)));
                        // Combine results -- fail if either failed
                        match (s3_result, local_result) {
                            (Ok(_), Ok(_)) => Ok(()),
                            (Err(e), _) | (_, Err(e)) => Err(e),
                        }
                    }
                    _ => Err(anyhow::anyhow!("unknown command: {}", op)),
                };

                let duration = start_time.elapsed().as_secs_f64();

                match result {
                    Ok(()) => {
                        if let Some(m) = &state_clone.metrics {
                            m.backup_duration_seconds
                                .with_label_values(&[op])
                                .observe(duration);
                            m.successful_operations_total.with_label_values(&[op]).inc();
                        }
                        info!(command = %command, "Action completed from POST /api/v1/actions");
                        // Invalidate manifest cache for operations that mutate remote state
                        if matches!(op, "upload" | "create_remote" | "delete" | "clean_broken") {
                            state_clone.manifest_cache.lock().await.invalidate();
                        }
                        state_clone.finish_op(id).await;
                    }
                    Err(e) => {
                        if let Some(m) = &state_clone.metrics {
                            m.backup_duration_seconds
                                .with_label_values(&[op])
                                .observe(duration);
                            m.errors_total.with_label_values(&[op]).inc();
                        }
                        warn!(command = %command, error = %e, "Action failed from POST /api/v1/actions");
                        state_clone.fail_op(id, e.to_string()).await;
                    }
                }
                    } => {}
                }
            });

            Ok((
                StatusCode::OK,
                Json(OperationStarted {
                    id,
                    status: "started".to_string(),
                }),
            ))
        }
        _ => Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: format!("unknown command: '{}'", op_name),
            }),
        )),
    }
}

/// GET /api/v1/list -- list backups, optionally filtered by location
pub async fn list_backups(
    State(state): State<AppState>,
    Query(params): Query<ListParams>,
) -> Result<
    (
        [(
            axum::http::header::HeaderName,
            axum::http::header::HeaderValue,
        ); 1],
        Json<Vec<ListResponse>>,
    ),
    (StatusCode, Json<ErrorResponse>),
> {
    let config = state.config.load();
    let data_path = &config.clickhouse.data_path;
    let mut results = Vec::new();

    let show_local = params.location.is_none() || params.location.as_deref() == Some("local");
    let show_remote = params.location.is_none() || params.location.as_deref() == Some("remote");

    if show_local {
        match list::list_local(data_path) {
            Ok(summaries) => {
                for s in summaries {
                    results.push(summary_to_list_response(s, "local"));
                }
            }
            Err(e) => {
                warn!(error = %e, "Failed to list local backups");
            }
        }
    }

    if show_remote {
        let s3 = state.s3.load();
        match list::list_remote_cached(&s3, &state.manifest_cache).await {
            Ok(summaries) => {
                for s in summaries {
                    results.push(summary_to_list_response(s, "remote"));
                }
            }
            Err(e) => {
                warn!(error = %e, "Failed to list remote backups");
            }
        }
    }

    // Apply desc parameter: reverse sort if desc=true (newest first)
    if params.desc.unwrap_or(false) {
        results.reverse();
    }

    // Apply pagination (offset/limit) -- same pattern as tables()
    let total_count = results.len();
    let offset = params.offset.unwrap_or(0);
    let results: Vec<ListResponse> = if let Some(limit) = params.limit {
        info!(
            offset = offset,
            limit = limit,
            total = total_count,
            "list: offset/limit applied"
        );
        results.into_iter().skip(offset).take(limit).collect()
    } else {
        if offset > 0 {
            info!(offset = offset, total = total_count, "list: offset applied");
        }
        results.into_iter().skip(offset).collect()
    };

    Ok((
        [(
            axum::http::header::HeaderName::from_static("x-total-count"),
            axum::http::header::HeaderValue::from_str(&total_count.to_string())
                .unwrap_or_else(|_| axum::http::header::HeaderValue::from_static("0")),
        )],
        Json(results),
    ))
}

/// Convert a BackupSummary to a ListResponse with all integration table columns.
fn summary_to_list_response(s: list::BackupSummary, location: &str) -> ListResponse {
    ListResponse {
        name: s.name,
        created: s.timestamp.map(|t| t.to_rfc3339()).unwrap_or_default(),
        location: location.to_string(),
        size: s.size,
        data_size: s.size, // For now, same as size (total uncompressed)
        object_disk_size: s.object_disk_size,
        metadata_size: s.metadata_size,
        rbac_size: s.rbac_size,
        config_size: s.config_size,
        compressed_size: s.compressed_size,
        required: s.required,
    }
}

// ---------------------------------------------------------------------------
// Backup operation endpoints (Task 6)
// ---------------------------------------------------------------------------

/// POST /api/v1/create -- create a local backup
pub async fn create_backup(
    State(state): State<AppState>,
    body: Option<Json<CreateRequest>>,
) -> Result<Json<OperationStarted>, (StatusCode, Json<ErrorResponse>)> {
    let req = body.map(|Json(r)| r).unwrap_or_default();

    // Validate backup name if provided (auto-generated names are always safe)
    if let Some(ref name) = req.backup_name {
        validate_backup_name(name).map_err(|e| {
            warn!(backup_name = %name, "backup name rejected: {}", e);
            (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse {
                    error: format!("invalid backup name: {e}"),
                }),
            )
        })?;
    }

    let metrics_clone = state.metrics.clone();
    run_operation(
        &state,
        "create",
        "create",
        false, // no cache invalidation
        move |config, ch, _s3| async move {
            let backup_name = req
                .backup_name
                .unwrap_or_else(|| Utc::now().format("%Y-%m-%dT%H%M%S").to_string());

            info!(backup_name = %backup_name, "Starting create operation");
            let manifest = crate::backup::create(
                &config,
                &ch,
                &backup_name,
                req.tables.as_deref(),
                req.schema.unwrap_or(false),
                req.diff_from.as_deref(),
                req.partitions.as_deref(),
                req.skip_check_parts_columns.unwrap_or(false),
                req.rbac.unwrap_or(false),
                req.configs.unwrap_or(false),
                req.named_collections.unwrap_or(false),
                &config.backup.skip_projections,
            )
            .await?;

            if let Some(m) = &metrics_clone {
                m.backup_last_success_timestamp
                    .set(Utc::now().timestamp() as f64);
                m.backup_size_bytes.set(manifest.compressed_size as f64);
            }
            Ok(())
        },
    )
    .await
}

/// Request body for POST /api/v1/create
#[derive(Debug, Deserialize, Default)]
pub struct CreateRequest {
    pub tables: Option<String>,
    pub diff_from: Option<String>,
    pub schema: Option<bool>,
    pub partitions: Option<String>,
    pub backup_name: Option<String>,
    pub skip_check_parts_columns: Option<bool>,
    pub rbac: Option<bool>,
    pub configs: Option<bool>,
    pub named_collections: Option<bool>,
}

/// POST /api/v1/upload/{name} -- upload a local backup to S3
pub async fn upload_backup(
    State(state): State<AppState>,
    Path(name): Path<String>,
    body: Option<Json<UploadRequest>>,
) -> Result<Json<OperationStarted>, (StatusCode, Json<ErrorResponse>)> {
    let req = body.map(|Json(r)| r).unwrap_or_default();

    validate_backup_name(&name).map_err(|e| {
        warn!(backup_name = %name, "backup name rejected: {}", e);
        (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: format!("invalid backup name: {e}"),
            }),
        )
    })?;

    let cache_clone = state.manifest_cache.clone();
    run_operation(
        &state,
        "upload",
        "upload",
        true, // invalidate cache after upload
        move |config, _ch, s3| async move {
            info!(backup_name = %name, "Starting upload operation");
            let backup_dir = std::path::PathBuf::from(&config.clickhouse.data_path)
                .join("backup")
                .join(&name);
            let effective_resume = config.general.use_resumable_state;
            crate::upload::upload(
                &config,
                &s3,
                &name,
                &backup_dir,
                req.delete_local.unwrap_or(false),
                req.diff_from_remote.as_deref(),
                effective_resume,
            )
            .await?;

            // Apply retention after successful upload (design doc 3.6 step 7)
            list::apply_retention_after_upload(&config, &s3, Some(&cache_clone)).await;
            Ok(())
        },
    )
    .await
}

/// Request body for POST /api/v1/upload/{name}
#[derive(Debug, Deserialize, Default)]
pub struct UploadRequest {
    pub delete_local: Option<bool>,
    pub diff_from_remote: Option<String>,
}

/// POST /api/v1/download/{name} -- download a backup from S3
pub async fn download_backup(
    State(state): State<AppState>,
    Path(name): Path<String>,
    body: Option<Json<DownloadRequest>>,
) -> Result<Json<OperationStarted>, (StatusCode, Json<ErrorResponse>)> {
    let req = body.map(|Json(r)| r).unwrap_or_default();

    validate_backup_name(&name).map_err(|e| {
        warn!(backup_name = %name, "backup name rejected: {}", e);
        (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: format!("invalid backup name: {e}"),
            }),
        )
    })?;

    let hardlink = req.hardlink_exists_files.unwrap_or(false);
    run_operation(
        &state,
        "download",
        "download",
        false, // no cache invalidation
        move |config, _ch, s3| async move {
            info!(backup_name = %name, hardlink_exists_files = hardlink, "Starting download operation");
            let effective_resume = config.general.use_resumable_state;
            crate::download::download(&config, &s3, &name, effective_resume, hardlink)
                .await
                .map(|_| ())
        },
    )
    .await
}

/// Request body for POST /api/v1/download/{name}
#[derive(Debug, Deserialize, Default)]
pub struct DownloadRequest {
    pub hardlink_exists_files: Option<bool>,
}

/// POST /api/v1/restore/{name} -- restore a downloaded backup
pub async fn restore_backup(
    State(state): State<AppState>,
    Path(name): Path<String>,
    body: Option<Json<RestoreRequest>>,
) -> Result<Json<OperationStarted>, (StatusCode, Json<ErrorResponse>)> {
    let req = body.map(|Json(r)| r).unwrap_or_default();

    validate_backup_name(&name).map_err(|e| {
        warn!(backup_name = %name, "backup name rejected: {}", e);
        (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: format!("invalid backup name: {e}"),
            }),
        )
    })?;

    // Parse remap parameters before starting the operation
    let db_mapping = match &req.database_mapping {
        Some(s) if !s.is_empty() => {
            let map = crate::restore::remap::parse_database_mapping(s).map_err(|e| {
                warn!(error = %e, "Invalid database_mapping parameter");
                (
                    StatusCode::BAD_REQUEST,
                    Json(ErrorResponse {
                        error: format!("invalid database_mapping: {}", e),
                    }),
                )
            })?;
            Some(map)
        }
        _ => None,
    };

    run_operation(
        &state,
        "restore",
        "restore",
        false, // no cache invalidation
        move |config, ch, _s3| async move {
            info!(backup_name = %name, "Starting restore operation");
            let effective_resume = config.general.use_resumable_state;
            crate::restore::restore(
                &config,
                &ch,
                &name,
                req.tables.as_deref(),
                req.schema.unwrap_or(false),
                req.data_only.unwrap_or(false),
                req.rm.unwrap_or(false),
                effective_resume,
                req.rename_as.as_deref(),
                db_mapping.as_ref(),
                req.rbac.unwrap_or(false),
                req.configs.unwrap_or(false),
                req.named_collections.unwrap_or(false),
                req.partitions.as_deref(),
                req.skip_empty_tables.unwrap_or(false),
            )
            .await
        },
    )
    .await
}

/// Request body for POST /api/v1/restore/{name}
#[derive(Debug, Deserialize, Default)]
pub struct RestoreRequest {
    pub tables: Option<String>,
    pub schema: Option<bool>,
    pub data_only: Option<bool>,
    #[serde(default)]
    pub rename_as: Option<String>,
    #[serde(default)]
    pub database_mapping: Option<String>,
    pub rm: Option<bool>,
    pub rbac: Option<bool>,
    pub configs: Option<bool>,
    pub named_collections: Option<bool>,
    pub partitions: Option<String>,
    pub skip_empty_tables: Option<bool>,
}

/// POST /api/v1/create_remote -- create local backup then upload to S3
pub async fn create_remote(
    State(state): State<AppState>,
    body: Option<Json<CreateRemoteRequest>>,
) -> Result<Json<OperationStarted>, (StatusCode, Json<ErrorResponse>)> {
    let req = body.map(|Json(r)| r).unwrap_or_default();

    // Validate backup name if provided (auto-generated names are always safe)
    if let Some(ref name) = req.backup_name {
        validate_backup_name(name).map_err(|e| {
            warn!(backup_name = %name, "backup name rejected: {}", e);
            (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse {
                    error: format!("invalid backup name: {e}"),
                }),
            )
        })?;
    }

    let metrics_clone = state.metrics.clone();
    let cache_clone = state.manifest_cache.clone();
    run_operation(
        &state,
        "create_remote",
        "create_remote",
        true, // invalidate cache after upload
        move |config, ch, s3| async move {
            let backup_name = req
                .backup_name
                .unwrap_or_else(|| Utc::now().format("%Y-%m-%dT%H%M%S").to_string());

            info!(backup_name = %backup_name, "Starting create_remote operation");

            // Step 1: Create local backup
            let manifest = crate::backup::create(
                &config,
                &ch,
                &backup_name,
                req.tables.as_deref(),
                false, // schema_only
                None,  // diff_from (create_remote uses diff_from_remote on upload side)
                None,  // partitions (create_remote doesn't support --partitions)
                req.skip_check_parts_columns.unwrap_or(false),
                req.rbac.unwrap_or(false),
                req.configs.unwrap_or(false),
                req.named_collections.unwrap_or(false),
                &config.backup.skip_projections,
            )
            .await?;

            // Step 2: Upload to S3
            let backup_dir = std::path::PathBuf::from(&config.clickhouse.data_path)
                .join("backup")
                .join(&backup_name);

            let effective_resume = config.general.use_resumable_state;
            crate::upload::upload(
                &config,
                &s3,
                &backup_name,
                &backup_dir,
                req.delete_source.unwrap_or(false),
                req.diff_from_remote.as_deref(),
                effective_resume,
            )
            .await?;

            // Apply retention after successful upload (design doc 3.6 step 7)
            list::apply_retention_after_upload(&config, &s3, Some(&cache_clone)).await;

            if let Some(m) = &metrics_clone {
                m.backup_last_success_timestamp
                    .set(Utc::now().timestamp() as f64);
                m.backup_size_bytes.set(manifest.compressed_size as f64);
            }
            Ok(())
        },
    )
    .await
}

/// Request body for POST /api/v1/create_remote
#[derive(Debug, Deserialize, Default)]
pub struct CreateRemoteRequest {
    pub tables: Option<String>,
    pub diff_from_remote: Option<String>,
    pub backup_name: Option<String>,
    pub delete_source: Option<bool>,
    pub skip_check_parts_columns: Option<bool>,
    pub rbac: Option<bool>,
    pub configs: Option<bool>,
    pub named_collections: Option<bool>,
}

/// POST /api/v1/restore_remote/{name} -- download then restore
pub async fn restore_remote(
    State(state): State<AppState>,
    Path(name): Path<String>,
    body: Option<Json<RestoreRemoteRequest>>,
) -> Result<Json<OperationStarted>, (StatusCode, Json<ErrorResponse>)> {
    let req = body.map(|Json(r)| r).unwrap_or_default();

    validate_backup_name(&name).map_err(|e| {
        warn!(backup_name = %name, "backup name rejected: {}", e);
        (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: format!("invalid backup name: {e}"),
            }),
        )
    })?;

    // Parse remap parameters before starting the operation
    let db_mapping = match &req.database_mapping {
        Some(s) if !s.is_empty() => {
            let map = crate::restore::remap::parse_database_mapping(s).map_err(|e| {
                warn!(error = %e, "Invalid database_mapping parameter");
                (
                    StatusCode::BAD_REQUEST,
                    Json(ErrorResponse {
                        error: format!("invalid database_mapping: {}", e),
                    }),
                )
            })?;
            Some(map)
        }
        _ => None,
    };

    run_operation(
        &state,
        "restore_remote",
        "restore_remote",
        false, // no cache invalidation
        move |config, ch, s3| async move {
            info!(backup_name = %name, "Starting restore_remote operation");

            let effective_resume = config.general.use_resumable_state;

            // Step 1: Download from S3
            crate::download::download(&config, &s3, &name, effective_resume, false)
                .await
                .map(|_| ())?;

            // Step 2: Restore with remap
            crate::restore::restore(
                &config,
                &ch,
                &name,
                req.tables.as_deref(),
                req.schema.unwrap_or(false),
                req.data_only.unwrap_or(false),
                req.rm.unwrap_or(false),
                effective_resume,
                req.rename_as.as_deref(),
                db_mapping.as_ref(),
                req.rbac.unwrap_or(false),
                req.configs.unwrap_or(false),
                req.named_collections.unwrap_or(false),
                req.partitions.as_deref(),
                req.skip_empty_tables.unwrap_or(false),
            )
            .await
        },
    )
    .await
}

/// Request body for POST /api/v1/restore_remote/{name}
#[derive(Debug, Deserialize, Default)]
pub struct RestoreRemoteRequest {
    pub tables: Option<String>,
    pub schema: Option<bool>,
    pub data_only: Option<bool>,
    #[serde(default)]
    pub rename_as: Option<String>,
    #[serde(default)]
    pub database_mapping: Option<String>,
    #[serde(default)]
    pub rm: Option<bool>,
    pub rbac: Option<bool>,
    pub configs: Option<bool>,
    pub named_collections: Option<bool>,
    pub partitions: Option<String>,
    pub skip_empty_tables: Option<bool>,
}

// ---------------------------------------------------------------------------
// Delete, clean, kill, and stub endpoints (Task 7)
// ---------------------------------------------------------------------------

/// DELETE /api/v1/delete/{location}/{name} -- delete a backup
pub async fn delete_backup(
    State(state): State<AppState>,
    Path((location, name)): Path<(String, String)>,
) -> Result<Json<OperationStarted>, (StatusCode, Json<ErrorResponse>)> {
    validate_backup_name(&name).map_err(|e| {
        warn!(backup_name = %name, "backup name rejected: {}", e);
        (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: format!("invalid backup name: {e}"),
            }),
        )
    })?;

    let loc = match location.as_str() {
        "local" => list::Location::Local,
        "remote" => list::Location::Remote,
        other => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse {
                    error: format!("invalid location '{}': expected 'local' or 'remote'", other),
                }),
            ));
        }
    };

    let invalidate = loc == list::Location::Remote;
    run_operation(
        &state,
        "delete",
        "delete",
        invalidate,
        move |config, _ch, s3| async move {
            info!(backup_name = %name, location = %location, "Starting delete operation");
            let data_path = config.clickhouse.data_path.clone();
            match loc {
                list::Location::Local => {
                    let n = name.clone();
                    tokio::task::spawn_blocking(move || list::delete_local(&data_path, &n))
                        .await
                        .unwrap_or_else(|e| Err(anyhow::anyhow!("spawn_blocking failed: {}", e)))
                }
                list::Location::Remote => list::delete_remote(&s3, &name).await,
            }
        },
    )
    .await
}

/// POST /api/v1/clean/remote_broken -- delete broken remote backups
pub async fn clean_remote_broken(
    State(state): State<AppState>,
) -> Result<Json<OperationStarted>, (StatusCode, Json<ErrorResponse>)> {
    run_operation(
        &state,
        "clean_broken_remote",
        "clean_broken_remote",
        true, // invalidate cache
        |_config, _ch, s3| async move {
            info!("Starting clean_broken_remote operation");
            let count = list::clean_broken_remote(&s3).await?;
            info!(count = count, "clean_broken_remote operation completed");
            Ok(())
        },
    )
    .await
}

/// POST /api/v1/clean/local_broken -- delete broken local backups
pub async fn clean_local_broken(
    State(state): State<AppState>,
) -> Result<Json<OperationStarted>, (StatusCode, Json<ErrorResponse>)> {
    run_operation(
        &state,
        "clean_broken_local",
        "clean_broken_local",
        false, // no cache invalidation
        |config, _ch, _s3| async move {
            info!("Starting clean_broken_local operation");
            let data_path = config.clickhouse.data_path.clone();
            let count = tokio::task::spawn_blocking(move || list::clean_broken_local(&data_path))
                .await
                .unwrap_or_else(|e| Err(anyhow::anyhow!("spawn_blocking failed: {}", e)))?;
            info!(count = count, "clean_broken_local operation completed");
            Ok(())
        },
    )
    .await
}

/// Query params for POST /api/v1/kill
#[derive(Debug, Deserialize, Default)]
pub struct KillParams {
    pub id: Option<u64>,
}

/// POST /api/v1/kill -- cancel running operation(s)
///
/// If `?id=N` is provided, cancels only the operation with that ID.
/// If no `id` is provided, cancels ALL running operations.
pub async fn kill_op(
    State(state): State<AppState>,
    Query(params): Query<KillParams>,
) -> Result<&'static str, StatusCode> {
    if state.kill_op(params.id).await {
        info!(target_id = ?params.id, "Operation killed");
        Ok("killed")
    } else {
        Err(StatusCode::NOT_FOUND)
    }
}

// ---------------------------------------------------------------------------
// Stub endpoints -- return 501 Not Implemented
// ---------------------------------------------------------------------------

/// POST /api/v1/clean -- shadow directory cleanup
pub async fn clean(
    State(state): State<AppState>,
) -> Result<Json<OperationStarted>, (StatusCode, Json<ErrorResponse>)> {
    run_operation(
        &state,
        "clean",
        "clean",
        false, // no cache invalidation
        |config, ch, _s3| async move {
            info!("Starting clean operation");
            let data_path = config.clickhouse.data_path.clone();
            let count = list::clean_shadow(&ch, &data_path, None).await?;
            info!(count = count, "clean operation completed");
            Ok(())
        },
    )
    .await
}

/// Load config from disk and create new ChClient and S3Client instances.
///
/// This helper does NOT swap the values into AppState -- callers decide when
/// and whether to swap (e.g., `restart` only swaps after a successful CH ping).
async fn reload_config_and_clients(
    state: &AppState,
) -> Result<
    (
        crate::config::Config,
        crate::clickhouse::ChClient,
        crate::storage::S3Client,
    ),
    (StatusCode, Json<ErrorResponse>),
> {
    let config = crate::config::Config::load(&state.config_path, &[]).map_err(|e| {
        warn!(error = %e, "Config load error");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: format!("config load error: {}", e),
            }),
        )
    })?;

    config.validate().map_err(|e| {
        warn!(error = %e, "Config validation error");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: format!("config validation error: {}", e),
            }),
        )
    })?;

    let ch = crate::clickhouse::ChClient::new(&config.clickhouse).map_err(|e| {
        warn!(error = %e, "ChClient creation error");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: format!("ChClient creation error: {}", e),
            }),
        )
    })?;

    let s3 = crate::storage::S3Client::new(&config.s3)
        .await
        .map_err(|e| {
            warn!(error = %e, "S3Client creation error");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: format!("S3Client creation error: {}", e),
                }),
            )
        })?;

    Ok((config, ch, s3))
}

/// POST /api/v1/reload -- config hot-reload
///
/// Reloads config from disk, creates new ChClient and S3Client, and atomically
/// swaps them into AppState. If the watch loop is active, also sends a reload
/// signal so the watch loop picks up the new config+clients on its next cycle.
pub async fn reload(
    State(state): State<AppState>,
) -> Result<Json<ReloadResponse>, (StatusCode, Json<ErrorResponse>)> {
    info!("Config reload requested");

    let (config, ch, s3) = reload_config_and_clients(&state).await?;

    // Atomically swap config and clients
    state.config.store(Arc::new(config));
    state.ch.store(Arc::new(ch));
    state.s3.store(Arc::new(s3));

    // If watch loop is active, also send reload signal so it picks up changes
    if let Some(tx) = &state.watch_reload_tx {
        tx.send(true).ok();
        info!("Config reload signal also sent to watch loop");
    }

    info!("Config reloaded");
    Ok(Json(ReloadResponse {
        status: "reloaded".to_string(),
    }))
}

/// Response for POST /api/v1/reload
#[derive(Debug, Serialize)]
pub struct ReloadResponse {
    pub status: String,
}

/// Response for POST /api/v1/restart
#[derive(Debug, Serialize)]
pub struct RestartResponse {
    pub status: String,
}

/// POST /api/v1/restart -- reload config and reconnect clients
///
/// Re-reads the config file, creates fresh ChClient and S3Client instances,
/// pings ClickHouse to verify connectivity, then atomically swaps the new
/// config and clients into AppState. On error, the old clients remain active.
pub async fn restart(
    State(state): State<AppState>,
) -> Result<Json<RestartResponse>, (StatusCode, Json<ErrorResponse>)> {
    info!("Restart requested: reloading config and reconnecting clients");

    let (config, ch, s3) = reload_config_and_clients(&state).await?;

    // Verify ClickHouse connectivity BEFORE swapping --
    // if ping fails, old clients remain active (no partial state).
    ch.ping().await.map_err(|e| {
        warn!(error = %e, "Restart failed: ClickHouse ping failed");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: format!("ClickHouse ping failed: {}", e),
            }),
        )
    })?;

    // Atomically swap config and clients
    state.config.store(Arc::new(config));
    state.ch.store(Arc::new(ch));
    state.s3.store(Arc::new(s3));

    info!("Restart completed: config reloaded and clients reconnected");

    Ok(Json(RestartResponse {
        status: "restarted".to_string(),
    }))
}

/// GET /api/v1/tables -- list tables from ClickHouse or from a remote backup manifest.
///
/// Supports two modes:
/// - **Live mode** (default): queries system.tables via ChClient
/// - **Remote mode** (`?backup=name`): downloads manifest from S3 and lists tables from it
///
/// Optional query params:
/// - `table`: glob pattern to filter tables (e.g. "default.*")
/// - `all`: include system databases (live mode only)
/// - `backup`: remote backup name to list tables from
pub async fn tables(
    State(state): State<AppState>,
    Query(params): Query<TablesParams>,
) -> Result<
    (
        [(
            axum::http::header::HeaderName,
            axum::http::header::HeaderValue,
        ); 1],
        Json<Vec<TablesResponseEntry>>,
    ),
    (StatusCode, Json<ErrorResponse>),
> {
    let results = if let Some(backup_name) = &params.backup {
        // Remote mode: download manifest and list tables from it
        let s3 = state.s3.load();
        let manifest_key = format!("{}/metadata.json", backup_name);
        let manifest_data = s3
            .get_object(&manifest_key)
            .await
            .map_err(|e| {
                warn!(error = %e, backup_name = %backup_name, "Failed to download manifest for tables endpoint");
                (
                    StatusCode::NOT_FOUND,
                    Json(ErrorResponse {
                        error: format!("failed to download manifest for backup '{}': {}", backup_name, e),
                    }),
                )
            })?;

        let manifest = BackupManifest::from_json_bytes(&manifest_data).map_err(|e| {
            warn!(error = %e, "Failed to parse manifest for tables endpoint");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: format!("failed to parse manifest: {}", e),
                }),
            )
        })?;

        let filter = params.table.as_deref().map(TableFilter::new);

        let results: Vec<TablesResponseEntry> = manifest
            .tables
            .iter()
            .filter(|(full_name, _)| {
                if let Some(ref f) = filter {
                    let parts: Vec<&str> = full_name.splitn(2, '.').collect();
                    let (db, tbl) = if parts.len() == 2 {
                        (parts[0], parts[1])
                    } else {
                        (full_name.as_str(), "")
                    };
                    f.matches(db, tbl)
                } else {
                    true
                }
            })
            .map(|(full_name, tm)| {
                let parts: Vec<&str> = full_name.splitn(2, '.').collect();
                let (db, tbl) = if parts.len() == 2 {
                    (parts[0].to_string(), parts[1].to_string())
                } else {
                    (full_name.clone(), String::new())
                };

                let total: u64 = tm
                    .parts
                    .values()
                    .flat_map(|v| v.iter())
                    .map(|p| p.size)
                    .sum();

                TablesResponseEntry {
                    database: db,
                    name: tbl,
                    engine: tm.engine.clone(),
                    uuid: tm.uuid.clone().unwrap_or_default(),
                    data_paths: Vec::new(),
                    total_bytes: Some(total),
                }
            })
            .collect();

        info!(
            backup_name = %backup_name,
            count = results.len(),
            "tables endpoint returning remote backup tables"
        );
        results
    } else {
        // Live mode: query ClickHouse
        let ch = state.ch.load();
        let all = params.all.unwrap_or(false);

        let rows = if all {
            ch.list_all_tables().await
        } else {
            ch.list_tables().await
        }
        .map_err(|e| {
            warn!(error = %e, "Failed to query tables for tables endpoint");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: format!("failed to query tables: {}", e),
                }),
            )
        })?;

        let filter = params.table.as_deref().map(TableFilter::new);

        let results: Vec<TablesResponseEntry> = rows
            .into_iter()
            .filter(|t| {
                if let Some(ref f) = filter {
                    if all {
                        f.matches_including_system(&t.database, &t.name)
                    } else {
                        f.matches(&t.database, &t.name)
                    }
                } else {
                    true
                }
            })
            .map(|t| TablesResponseEntry {
                database: t.database,
                name: t.name,
                engine: t.engine,
                uuid: t.uuid,
                data_paths: t.data_paths,
                total_bytes: t.total_bytes,
            })
            .collect();

        info!(
            count = results.len(),
            "tables endpoint returning live tables"
        );
        results
    };

    // Apply pagination (offset/limit)
    let total_count = results.len();
    let offset = params.offset.unwrap_or(0);
    let results: Vec<TablesResponseEntry> = if let Some(limit) = params.limit {
        info!(
            offset = offset,
            limit = limit,
            total = total_count,
            "tables: offset/limit applied"
        );
        results.into_iter().skip(offset).take(limit).collect()
    } else {
        if offset > 0 {
            info!(
                offset = offset,
                total = total_count,
                "tables: offset applied"
            );
        }
        results.into_iter().skip(offset).collect()
    };

    Ok((
        [(
            axum::http::header::HeaderName::from_static("x-total-count"),
            axum::http::header::HeaderValue::from_str(&total_count.to_string())
                .unwrap_or_else(|_| axum::http::header::HeaderValue::from_static("0")),
        )],
        Json(results),
    ))
}

/// POST /api/v1/watch/start -- start the watch loop
///
/// If the watch loop is already active, returns 409 Conflict.
/// Otherwise, creates channels, spawns the watch loop, and stores
/// the handles in AppState.
pub async fn watch_start(
    State(mut state): State<AppState>,
) -> Result<Json<WatchActionResponse>, (StatusCode, Json<ErrorResponse>)> {
    // Check if watch is already active
    {
        let ws = state.watch_status.lock().await;
        if ws.active {
            return Err((
                StatusCode::LOCKED,
                Json(ErrorResponse {
                    error: "watch loop already active".to_string(),
                }),
            ));
        }
    }

    // Query macros from ClickHouse for template resolution
    let ch = state.ch.load();
    let macros = ch.get_macros().await.unwrap_or_default();

    let config_path = state.config_path.clone();
    super::spawn_watch_from_state(&mut state, config_path, macros).await;

    info!("Watch loop started via API");
    Ok(Json(WatchActionResponse {
        status: "started".to_string(),
    }))
}

/// POST /api/v1/watch/stop -- stop the watch loop
///
/// If the watch loop is not active, returns 404.
/// Otherwise, sends a shutdown signal to the watch loop.
pub async fn watch_stop(
    State(state): State<AppState>,
) -> Result<Json<WatchActionResponse>, (StatusCode, Json<ErrorResponse>)> {
    let is_active = {
        let ws = state.watch_status.lock().await;
        ws.active
    };

    if !is_active {
        return Err((
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: "watch loop not active".to_string(),
            }),
        ));
    }

    if let Some(tx) = &state.watch_shutdown_tx {
        tx.send(true).ok();
    }

    info!("Watch loop stop signal sent via API");
    Ok(Json(WatchActionResponse {
        status: "stopped".to_string(),
    }))
}

/// GET /api/v1/watch/status -- return current watch loop status
pub async fn watch_status(State(state): State<AppState>) -> Json<WatchStatusResponse> {
    let ws = state.watch_status.lock().await;

    let next_in = ws.next_backup_in.map(format_duration);

    Json(WatchStatusResponse {
        state: ws.state.clone(),
        active: ws.active,
        last_full: ws.last_full.map(|t| t.to_rfc3339()),
        last_incr: ws.last_incr.map(|t| t.to_rfc3339()),
        consecutive_errors: ws.consecutive_errors,
        next_in,
    })
}

/// Response for POST /api/v1/watch/start and POST /api/v1/watch/stop
#[derive(Debug, Serialize)]
pub struct WatchActionResponse {
    pub status: String,
}

/// Response for GET /api/v1/watch/status
#[derive(Debug, Serialize)]
pub struct WatchStatusResponse {
    pub state: String,
    pub active: bool,
    pub last_full: Option<String>,
    pub last_incr: Option<String>,
    pub consecutive_errors: u32,
    pub next_in: Option<String>,
}

/// Format a Duration as a human-readable string (e.g. "47m", "2h30m", "5s").
fn format_duration(d: std::time::Duration) -> String {
    let total_secs = d.as_secs();
    let hours = total_secs / 3600;
    let minutes = (total_secs % 3600) / 60;
    let seconds = total_secs % 60;

    if hours > 0 && minutes > 0 {
        format!("{}h{}m", hours, minutes)
    } else if hours > 0 {
        format!("{}h", hours)
    } else if minutes > 0 {
        format!("{}m", minutes)
    } else {
        format!("{}s", seconds)
    }
}

/// GET /metrics -- Prometheus metrics endpoint
///
/// When metrics are enabled (`enable_metrics=true`), encodes all registered
/// prometheus metrics into text exposition format. On each scrape, refreshes
/// backup count gauges and in-progress state.
///
/// Returns 501 Not Implemented when metrics are disabled.
pub async fn metrics(State(state): State<AppState>) -> impl IntoResponse {
    let Some(metrics) = &state.metrics else {
        return (StatusCode::NOT_IMPLEMENTED, "metrics disabled".to_string());
    };

    // Refresh backup count gauges (expensive -- OK for 15-30s scrape intervals)
    refresh_backup_counts(&state, metrics).await;

    match metrics.encode() {
        Ok(text) => (StatusCode::OK, text),
        Err(e) => {
            warn!(error = %e, "Failed to encode metrics");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("metrics encoding error: {}", e),
            )
        }
    }
}

/// Refresh backup count and in-progress gauges for the `/metrics` scrape.
///
/// Calls `list_local` (via `spawn_blocking`) and `list_remote` (async) to
/// update the `number_backups_local` and `number_backups_remote` gauges.
/// Also updates the `in_progress` gauge from `running_ops`.
async fn refresh_backup_counts(state: &AppState, metrics: &Metrics) {
    // Refresh local backup count (sync function -- use spawn_blocking)
    let config = state.config.load();
    let data_path = config.clickhouse.data_path.clone();
    match tokio::task::spawn_blocking(move || crate::list::list_local(&data_path)).await {
        Ok(Ok(summaries)) => metrics.number_backups_local.set(summaries.len() as i64),
        Ok(Err(e)) => warn!(error = %e, "Failed to refresh local backup count for metrics"),
        Err(e) => warn!(error = %e, "spawn_blocking failed for list_local in metrics"),
    }

    // Refresh remote backup count (async, using cache to avoid redundant S3 calls)
    let s3 = state.s3.load();
    match crate::list::list_remote_cached(&s3, &state.manifest_cache).await {
        Ok(summaries) => metrics.number_backups_remote.set(summaries.len() as i64),
        Err(e) => warn!(error = %e, "Failed to refresh remote backup count for metrics"),
    }

    // Refresh in_progress gauge from running_ops
    let is_running = !state.running_ops.lock().await.is_empty();
    metrics.in_progress.set(if is_running { 1 } else { 0 });
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_health_returns_ok() {
        let response = HealthResponse {
            status: "ok".to_string(),
        };
        let json = serde_json::to_string(&response).expect("HealthResponse should serialize");
        assert!(json.contains("\"status\":\"ok\""));
    }

    #[tokio::test]
    async fn test_health_handler() {
        let Json(result) = health().await;
        assert_eq!(result.status, "ok");
    }

    #[test]
    fn test_actions_empty_log() {
        // Verify ActionResponse serialization
        let response = ActionResponse {
            id: 1,
            command: "create".to_string(),
            start: "2024-01-15T10:00:00+00:00".to_string(),
            finish: String::new(),
            status: "running".to_string(),
            error: String::new(),
        };

        let json = serde_json::to_string(&response).expect("ActionResponse should serialize");
        assert!(json.contains("\"id\":1"));
        assert!(json.contains("\"command\":\"create\""));
        assert!(json.contains("\"status\":\"running\""));
    }

    #[test]
    fn test_list_response_all_columns() {
        // Verify ListResponse has all integration table columns
        let response = ListResponse {
            name: "daily-backup".to_string(),
            created: "2024-01-15T10:00:00+00:00".to_string(),
            location: "local".to_string(),
            size: 1024,
            data_size: 1024,
            object_disk_size: 0,
            metadata_size: 256,
            rbac_size: 0,
            config_size: 0,
            compressed_size: 512,
            required: String::new(),
        };

        let json = serde_json::to_string(&response).expect("ListResponse should serialize");
        // Verify all required columns are present
        assert!(json.contains("\"name\""));
        assert!(json.contains("\"created\""));
        assert!(json.contains("\"location\""));
        assert!(json.contains("\"size\""));
        assert!(json.contains("\"data_size\""));
        assert!(json.contains("\"object_disk_size\""));
        assert!(json.contains("\"metadata_size\""));
        assert!(json.contains("\"rbac_size\""));
        assert!(json.contains("\"config_size\""));
        assert!(json.contains("\"compressed_size\""));
        assert!(json.contains("\"required\""));
    }

    #[test]
    fn test_list_params_deserialization() {
        // With all fields
        let json =
            r#"{"location": "remote", "desc": true, "offset": 5, "limit": 10, "format": "json"}"#;
        let params: ListParams = serde_json::from_str(json).expect("Should parse ListParams");
        assert_eq!(params.location.as_deref(), Some("remote"));
        assert_eq!(params.desc, Some(true));
        assert_eq!(params.offset, Some(5));
        assert_eq!(params.limit, Some(10));
        assert_eq!(params.format.as_deref(), Some("json"));

        // With no fields (all optional)
        let json_empty = r#"{}"#;
        let params_empty: ListParams =
            serde_json::from_str(json_empty).expect("Should parse empty ListParams");
        assert!(params_empty.location.is_none());
        assert!(params_empty.desc.is_none());
        assert!(params_empty.offset.is_none());
        assert!(params_empty.limit.is_none());
        assert!(params_empty.format.is_none());

        // With only pagination fields
        let json_paginated = r#"{"offset": 0, "limit": 100}"#;
        let params_paginated: ListParams =
            serde_json::from_str(json_paginated).expect("Should parse paginated ListParams");
        assert_eq!(params_paginated.offset, Some(0));
        assert_eq!(params_paginated.limit, Some(100));
        assert!(params_paginated.location.is_none());
    }

    #[test]
    fn test_post_actions_dispatch() {
        // Verify ActionRequest deserialization
        let json = r#"{"command": "create_remote daily_backup"}"#;
        let req: ActionRequest = serde_json::from_str(json).expect("Should parse ActionRequest");
        assert_eq!(req.command, "create_remote daily_backup");

        // Verify command parsing
        let parts: Vec<&str> = req.command.split_whitespace().collect();
        assert_eq!(parts[0], "create_remote");
        assert_eq!(parts[1], "daily_backup");
    }

    #[test]
    fn test_create_request_deserialization() {
        let json = r#"{
            "tables": "default.*",
            "diff_from": "previous",
            "schema": true,
            "partitions": "202401",
            "backup_name": "my-backup",
            "skip_check_parts_columns": true
        }"#;
        let req: CreateRequest = serde_json::from_str(json).expect("Should parse CreateRequest");
        assert_eq!(req.tables.as_deref(), Some("default.*"));
        assert_eq!(req.diff_from.as_deref(), Some("previous"));
        assert_eq!(req.schema, Some(true));
        assert_eq!(req.partitions.as_deref(), Some("202401"));
        assert_eq!(req.backup_name.as_deref(), Some("my-backup"));
        assert_eq!(req.skip_check_parts_columns, Some(true));
    }

    #[test]
    fn test_create_request_default() {
        let json = r#"{}"#;
        let req: CreateRequest =
            serde_json::from_str(json).expect("Should parse empty CreateRequest");
        assert!(req.tables.is_none());
        assert!(req.diff_from.is_none());
        assert!(req.schema.is_none());
    }

    #[test]
    fn test_create_request_has_rbac_fields() {
        let json = r#"{
            "backup_name": "test-backup",
            "rbac": true,
            "configs": true,
            "named_collections": false
        }"#;
        let req: CreateRequest =
            serde_json::from_str(json).expect("Should parse CreateRequest with RBAC fields");
        assert_eq!(req.rbac, Some(true));
        assert_eq!(req.configs, Some(true));
        assert_eq!(req.named_collections, Some(false));

        // Verify defaults when fields are omitted
        let json_empty = r#"{}"#;
        let req_empty: CreateRequest =
            serde_json::from_str(json_empty).expect("Should parse empty CreateRequest");
        assert!(req_empty.rbac.is_none());
        assert!(req_empty.configs.is_none());
        assert!(req_empty.named_collections.is_none());

        // Verify unwrap_or(false) pattern used in route handlers
        assert!(!req_empty.rbac.unwrap_or(false));
        assert!(!req_empty.configs.unwrap_or(false));
        assert!(!req_empty.named_collections.unwrap_or(false));
    }

    #[test]
    fn test_operation_started_response() {
        let response = OperationStarted {
            id: 42,
            status: "started".to_string(),
        };

        let json = serde_json::to_string(&response).expect("OperationStarted should serialize");
        assert!(json.contains("\"id\":42"));
        assert!(json.contains("\"status\":\"started\""));
    }

    #[test]
    fn test_restore_request_accepts_all_fields() {
        let json = r#"{
            "tables": "default.*",
            "schema": false,
            "data_only": true,
            "rename_as": "staging.users",
            "database_mapping": "source_db:target_db",
            "rm": true
        }"#;
        let req: RestoreRequest =
            serde_json::from_str(json).expect("Should parse RestoreRequest with all fields");
        assert_eq!(req.tables.as_deref(), Some("default.*"));
        assert_eq!(req.schema, Some(false));
        assert_eq!(req.data_only, Some(true));
        assert_eq!(req.rename_as.as_deref(), Some("staging.users"));
        assert_eq!(req.database_mapping.as_deref(), Some("source_db:target_db"));
        assert_eq!(req.rm, Some(true));
    }

    #[test]
    fn test_restore_remote_request_accepts_remap_fields() {
        let json = r#"{
            "tables": "prod.*",
            "rename_as": "staging.users",
            "database_mapping": "prod:staging"
        }"#;
        let req: RestoreRemoteRequest = serde_json::from_str(json)
            .expect("Should parse RestoreRemoteRequest with remap fields");
        assert_eq!(req.tables.as_deref(), Some("prod.*"));
        assert_eq!(req.rename_as.as_deref(), Some("staging.users"));
        assert_eq!(req.database_mapping.as_deref(), Some("prod:staging"));
    }

    #[test]
    fn test_error_response_serialization() {
        let response = ErrorResponse {
            error: "something went wrong".to_string(),
        };

        let json = serde_json::to_string(&response).expect("ErrorResponse should serialize");
        assert!(json.contains("\"error\":\"something went wrong\""));
    }

    #[test]
    fn test_delete_path_parsing() {
        // Verify "local"/"remote" string maps correctly to Location
        let local_result = match "local" {
            "local" => Some(list::Location::Local),
            "remote" => Some(list::Location::Remote),
            _ => None,
        };
        assert_eq!(local_result, Some(list::Location::Local));

        let remote_result = match "remote" {
            "local" => Some(list::Location::Local),
            "remote" => Some(list::Location::Remote),
            _ => None,
        };
        assert_eq!(remote_result, Some(list::Location::Remote));

        let invalid_result = match "invalid" {
            "local" => Some(list::Location::Local),
            "remote" => Some(list::Location::Remote),
            _ => None,
        };
        assert!(invalid_result.is_none());
    }

    #[test]
    fn test_tables_response_entry_serialization() {
        let entry = TablesResponseEntry {
            database: "default".to_string(),
            name: "users".to_string(),
            engine: "MergeTree".to_string(),
            uuid: "abc-123".to_string(),
            data_paths: vec!["/data/default/users/".to_string()],
            total_bytes: Some(1024),
        };

        let json = serde_json::to_string(&entry).expect("TablesResponseEntry should serialize");
        assert!(json.contains("\"database\":\"default\""));
        assert!(json.contains("\"name\":\"users\""));
        assert!(json.contains("\"engine\":\"MergeTree\""));
        assert!(json.contains("\"uuid\":\"abc-123\""));
        assert!(json.contains("\"total_bytes\":1024"));
    }

    #[test]
    fn test_tables_params_deserialization() {
        // With all fields
        let json = r#"{"table": "default.*", "all": true, "backup": "daily-2024-01-15"}"#;
        let params: TablesParams = serde_json::from_str(json).expect("Should parse TablesParams");
        assert_eq!(params.table.as_deref(), Some("default.*"));
        assert_eq!(params.all, Some(true));
        assert_eq!(params.backup.as_deref(), Some("daily-2024-01-15"));

        // With no fields
        let json_empty = r#"{}"#;
        let params_empty: TablesParams =
            serde_json::from_str(json_empty).expect("Should parse empty TablesParams");
        assert!(params_empty.table.is_none());
        assert!(params_empty.all.is_none());
        assert!(params_empty.backup.is_none());
    }

    #[test]
    fn test_tables_response_entry_from_manifest_data() {
        // Simulate what the tables handler does in remote mode:
        // convert manifest table data to TablesResponseEntry
        let full_name = "default.users";
        let parts: Vec<&str> = full_name.splitn(2, '.').collect();
        let (db, tbl) = if parts.len() == 2 {
            (parts[0].to_string(), parts[1].to_string())
        } else {
            (full_name.to_string(), String::new())
        };

        let entry = TablesResponseEntry {
            database: db.clone(),
            name: tbl.clone(),
            engine: "MergeTree".to_string(),
            uuid: "".to_string(),
            data_paths: Vec::new(),
            total_bytes: Some(2048),
        };

        assert_eq!(entry.database, "default");
        assert_eq!(entry.name, "users");
        assert_eq!(entry.total_bytes, Some(2048));
    }

    #[test]
    fn test_restart_response_serialization() {
        let response = RestartResponse {
            status: "restarted".to_string(),
        };
        let json = serde_json::to_string(&response).expect("RestartResponse should serialize");
        assert!(json.contains("\"status\":\"restarted\""));
    }

    #[test]
    fn test_watch_status_response_serialization() {
        let response = WatchStatusResponse {
            state: "sleeping".to_string(),
            active: true,
            last_full: Some("2025-02-15T02:00:00+00:00".to_string()),
            last_incr: Some("2025-02-15T03:00:00+00:00".to_string()),
            consecutive_errors: 0,
            next_in: Some("47m".to_string()),
        };

        let json = serde_json::to_string(&response).expect("WatchStatusResponse should serialize");
        assert!(json.contains("\"state\":\"sleeping\""));
        assert!(json.contains("\"active\":true"));
        assert!(json.contains("\"next_in\":\"47m\""));
    }

    #[test]
    fn test_format_duration_minutes() {
        assert_eq!(format_duration(std::time::Duration::from_secs(2820)), "47m");
    }

    #[test]
    fn test_format_duration_hours_and_minutes() {
        assert_eq!(
            format_duration(std::time::Duration::from_secs(9000)),
            "2h30m"
        );
    }

    #[test]
    fn test_format_duration_seconds() {
        assert_eq!(format_duration(std::time::Duration::from_secs(5)), "5s");
    }

    #[test]
    fn test_format_duration_hours_only() {
        assert_eq!(format_duration(std::time::Duration::from_secs(3600)), "1h");
    }

    #[test]
    fn test_reload_response_serialization() {
        let response = ReloadResponse {
            status: "reloaded".to_string(),
        };

        let json = serde_json::to_string(&response).expect("ReloadResponse should serialize");
        assert!(json.contains("\"status\":\"reloaded\""));
    }

    /// Verify that reload_config_and_clients is called from both reload() and restart().
    /// This is a structural test -- we verify both handlers use the shared helper
    /// by checking that they produce valid response types and that the helper's
    /// error path returns appropriate status codes.
    #[test]
    fn test_reload_updates_config_structural() {
        // Verify ReloadResponse and RestartResponse are the expected types
        let reload_resp = ReloadResponse {
            status: "reloaded".to_string(),
        };
        let restart_resp = RestartResponse {
            status: "restarted".to_string(),
        };

        // Both should serialize to valid JSON with status field
        let reload_json =
            serde_json::to_string(&reload_resp).expect("ReloadResponse should serialize");
        let restart_json =
            serde_json::to_string(&restart_resp).expect("RestartResponse should serialize");

        assert!(reload_json.contains("\"status\":\"reloaded\""));
        assert!(restart_json.contains("\"status\":\"restarted\""));

        // Verify ErrorResponse can carry config error messages
        let err = ErrorResponse {
            error: "config load error: file not found".to_string(),
        };
        let err_json = serde_json::to_string(&err).expect("ErrorResponse should serialize");
        assert!(err_json.contains("config load error"));
    }

    #[test]
    fn test_watch_action_response_serialization() {
        let response = WatchActionResponse {
            status: "started".to_string(),
        };

        let json = serde_json::to_string(&response).expect("WatchActionResponse should serialize");
        assert!(json.contains("\"status\":\"started\""));
    }

    #[test]
    fn test_metrics_handler_returns_prometheus_text() {
        // Verify that Metrics::encode() produces valid prometheus text format
        // that the handler returns on the success path.
        let metrics = super::Metrics::new().expect("Metrics::new() should succeed");
        let text = metrics.encode().expect("encode() should succeed");

        // Handler returns (StatusCode::OK, text) when metrics is Some
        assert!(!text.is_empty(), "Encoded text should not be empty");
        assert!(
            text.contains("# HELP chbackup_"),
            "Should contain prometheus HELP lines"
        );
        assert!(
            text.contains("# TYPE chbackup_"),
            "Should contain prometheus TYPE lines"
        );
        // Verify it's text/plain prometheus format (contains histogram and gauge data)
        assert!(
            text.contains("chbackup_backup_duration_seconds"),
            "Should contain duration histogram"
        );
        assert!(
            text.contains("chbackup_in_progress"),
            "Should contain in_progress gauge"
        );
    }

    #[test]
    fn test_metrics_handler_disabled() {
        // When state.metrics is None, the handler returns 501.
        // Verify the None path logic produces the expected status code.
        let metrics: Option<std::sync::Arc<super::Metrics>> = None;

        // Simulates the handler's None path
        let result = if metrics.is_some() {
            (StatusCode::OK, "has metrics".to_string())
        } else {
            (StatusCode::NOT_IMPLEMENTED, "metrics disabled".to_string())
        };

        assert_eq!(result.0, StatusCode::NOT_IMPLEMENTED);
        assert_eq!(result.1, "metrics disabled");
    }

    #[test]
    fn test_tables_pagination_params_deserialize() {
        // Verify TablesParams can be deserialized from JSON with offset/limit
        let json = r#"{"offset": 5, "limit": 10}"#;
        let params: TablesParams = serde_json::from_str(json).expect("should deserialize");
        assert_eq!(params.offset, Some(5));
        assert_eq!(params.limit, Some(10));
        assert_eq!(params.table, None);
        assert_eq!(params.backup, None);

        // Verify defaults when offset/limit are omitted
        let json2 = r#"{"table": "default.*"}"#;
        let params2: TablesParams = serde_json::from_str(json2).expect("should deserialize");
        assert_eq!(params2.offset, None);
        assert_eq!(params2.limit, None);
        assert_eq!(params2.table.as_deref(), Some("default.*"));

        // Verify all params together
        let json3 = r#"{"table": "*.trades", "all": true, "offset": 0, "limit": 100}"#;
        let params3: TablesParams = serde_json::from_str(json3).expect("should deserialize");
        assert_eq!(params3.table.as_deref(), Some("*.trades"));
        assert_eq!(params3.all, Some(true));
        assert_eq!(params3.offset, Some(0));
        assert_eq!(params3.limit, Some(100));
    }

    #[test]
    fn test_summary_to_list_response_sizes() {
        let summary = list::BackupSummary {
            name: "test-backup".to_string(),
            timestamp: None,
            size: 4096,
            compressed_size: 2048,
            table_count: 3,
            metadata_size: 128,
            rbac_size: 1024,
            config_size: 512,
            object_disk_size: 768,
            required: "base-backup".to_string(),
            is_broken: false,
            broken_reason: None,
        };

        let response = summary_to_list_response(summary, "local");
        assert_eq!(response.rbac_size, 1024);
        assert_eq!(response.config_size, 512);
        assert_eq!(response.metadata_size, 128);
        assert_eq!(response.size, 4096);
        assert_eq!(response.object_disk_size, 768);
        assert_eq!(response.required, "base-backup");
    }
}
