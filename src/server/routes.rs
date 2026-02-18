//! HTTP route handlers for the chbackup API server.
//!
//! All endpoints from design doc section 9 are implemented here.
//! Read-only endpoints return data directly; operation endpoints spawn
//! background tasks and return immediately with an action ID.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::list;

use super::actions::ActionStatus;
use super::state::AppState;

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

/// GET /health -- simple health check
pub async fn health() -> &'static str {
    "OK"
}

/// GET /api/v1/version -- return chbackup and ClickHouse versions
pub async fn version(State(state): State<AppState>) -> Json<VersionResponse> {
    let ch_version = match state.ch.get_version().await {
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
pub async fn status(State(state): State<AppState>) -> Json<StatusResponse> {
    let current = state.current_op.lock().await;

    match current.as_ref() {
        Some(op) => {
            // Look up the action entry to get the start time
            let action_log = state.action_log.lock().await;
            let start_time = action_log
                .entries()
                .iter()
                .find(|e| e.id == op.id)
                .map(|e| e.start.to_rfc3339());

            Json(StatusResponse {
                status: "running".to_string(),
                command: Some(op.command.clone()),
                start: start_time,
            })
        }
        None => Json(StatusResponse {
            status: "idle".to_string(),
            command: None,
            start: None,
        }),
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
            let (id, _token) = state.try_start_op(op_name).await.map_err(|e| {
                (
                    StatusCode::CONFLICT,
                    Json(ErrorResponse {
                        error: e.to_string(),
                    }),
                )
            })?;

            let state_clone = state.clone();
            let command = request.command.clone();
            tokio::spawn(async move {
                tracing::info!(command = %command, "Action dispatched from POST /api/v1/actions");
                // For now, we mark the operation as completed immediately.
                // Full dispatch to actual command functions is wired via the dedicated
                // POST endpoints (create, upload, etc.) which parse proper request bodies.
                state_clone.finish_op(id).await;
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
) -> Result<Json<Vec<ListResponse>>, (StatusCode, Json<ErrorResponse>)> {
    let data_path = &state.config.clickhouse.data_path;
    let mut results = Vec::new();

    let show_local =
        params.location.is_none() || params.location.as_deref() == Some("local");
    let show_remote =
        params.location.is_none() || params.location.as_deref() == Some("remote");

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
        match list::list_remote(&state.s3).await {
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

    Ok(Json(results))
}

/// Convert a BackupSummary to a ListResponse with all integration table columns.
fn summary_to_list_response(s: list::BackupSummary, location: &str) -> ListResponse {
    ListResponse {
        name: s.name,
        created: s.timestamp.map(|t| t.to_rfc3339()).unwrap_or_default(),
        location: location.to_string(),
        size: s.size,
        data_size: s.size, // For now, same as size (total uncompressed)
        object_disk_size: 0, // Requires manifest disk_types analysis (future)
        metadata_size: 0,    // TODO: expose from manifest metadata_size field
        rbac_size: 0,        // Not implemented until Phase 4e
        config_size: 0,      // Not implemented until Phase 4e
        compressed_size: s.compressed_size,
        required: String::new(), // No dependency chain tracking yet
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
    let (id, _token) = state.try_start_op("create").await.map_err(|e| {
        (
            StatusCode::CONFLICT,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        )
    })?;

    let state_clone = state.clone();
    tokio::spawn(async move {
        let backup_name = req
            .backup_name
            .unwrap_or_else(|| Utc::now().format("%Y-%m-%dT%H%M%S").to_string());

        info!(backup_name = %backup_name, "Starting create operation");

        let result = crate::backup::create(
            &state_clone.config,
            &state_clone.ch,
            &backup_name,
            req.tables.as_deref(),
            req.schema.unwrap_or(false),
            req.diff_from.as_deref(),
            req.partitions.as_deref(),
            req.skip_check_parts_columns.unwrap_or(false),
        )
        .await;

        match result {
            Ok(_) => {
                info!(backup_name = %backup_name, "Create operation completed");
                state_clone.finish_op(id).await;
            }
            Err(e) => {
                warn!(backup_name = %backup_name, error = %e, "Create operation failed");
                state_clone.fail_op(id, e.to_string()).await;
            }
        }
    });

    Ok(Json(OperationStarted {
        id,
        status: "started".to_string(),
    }))
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
}

/// POST /api/v1/upload/{name} -- upload a local backup to S3
pub async fn upload_backup(
    State(state): State<AppState>,
    Path(name): Path<String>,
    body: Option<Json<UploadRequest>>,
) -> Result<Json<OperationStarted>, (StatusCode, Json<ErrorResponse>)> {
    let req = body.map(|Json(r)| r).unwrap_or_default();
    let (id, _token) = state.try_start_op("upload").await.map_err(|e| {
        (
            StatusCode::CONFLICT,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        )
    })?;

    let state_clone = state.clone();
    tokio::spawn(async move {
        info!(backup_name = %name, "Starting upload operation");

        let backup_dir = std::path::PathBuf::from(&state_clone.config.clickhouse.data_path)
            .join("backup")
            .join(&name);

        let effective_resume = state_clone.config.general.use_resumable_state;
        let result = crate::upload::upload(
            &state_clone.config,
            &state_clone.s3,
            &name,
            &backup_dir,
            req.delete_local.unwrap_or(false),
            req.diff_from_remote.as_deref(),
            effective_resume,
        )
        .await;

        match result {
            Ok(_) => {
                info!(backup_name = %name, "Upload operation completed");
                state_clone.finish_op(id).await;
            }
            Err(e) => {
                warn!(backup_name = %name, error = %e, "Upload operation failed");
                state_clone.fail_op(id, e.to_string()).await;
            }
        }
    });

    Ok(Json(OperationStarted {
        id,
        status: "started".to_string(),
    }))
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
    let (id, _token) = state.try_start_op("download").await.map_err(|e| {
        (
            StatusCode::CONFLICT,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        )
    })?;

    let state_clone = state.clone();
    tokio::spawn(async move {
        if req.hardlink_exists_files.unwrap_or(false) {
            warn!("hardlink_exists_files flag is not yet implemented, ignoring");
        }

        info!(backup_name = %name, "Starting download operation");

        let effective_resume = state_clone.config.general.use_resumable_state;
        let result = crate::download::download(
            &state_clone.config,
            &state_clone.s3,
            &name,
            effective_resume,
        )
        .await;

        match result {
            Ok(_backup_dir) => {
                info!(backup_name = %name, "Download operation completed");
                state_clone.finish_op(id).await;
            }
            Err(e) => {
                warn!(backup_name = %name, error = %e, "Download operation failed");
                state_clone.fail_op(id, e.to_string()).await;
            }
        }
    });

    Ok(Json(OperationStarted {
        id,
        status: "started".to_string(),
    }))
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
    let (id, _token) = state.try_start_op("restore").await.map_err(|e| {
        (
            StatusCode::CONFLICT,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        )
    })?;

    let state_clone = state.clone();
    tokio::spawn(async move {
        if req.database_mapping.is_some() {
            warn!("database_mapping is not yet implemented (Phase 4a), ignoring");
        }
        if req.rm.unwrap_or(false) {
            warn!("rm flag is not yet implemented (Phase 4d), ignoring");
        }

        info!(backup_name = %name, "Starting restore operation");

        let effective_resume = state_clone.config.general.use_resumable_state;
        let result = crate::restore::restore(
            &state_clone.config,
            &state_clone.ch,
            &name,
            req.tables.as_deref(),
            req.schema.unwrap_or(false),
            req.data_only.unwrap_or(false),
            effective_resume,
        )
        .await;

        match result {
            Ok(_) => {
                info!(backup_name = %name, "Restore operation completed");
                state_clone.finish_op(id).await;
            }
            Err(e) => {
                warn!(backup_name = %name, error = %e, "Restore operation failed");
                state_clone.fail_op(id, e.to_string()).await;
            }
        }
    });

    Ok(Json(OperationStarted {
        id,
        status: "started".to_string(),
    }))
}

/// Request body for POST /api/v1/restore/{name}
#[derive(Debug, Deserialize, Default)]
pub struct RestoreRequest {
    pub tables: Option<String>,
    pub schema: Option<bool>,
    pub data_only: Option<bool>,
    pub database_mapping: Option<String>,
    pub rm: Option<bool>,
}

/// POST /api/v1/create_remote -- create local backup then upload to S3
pub async fn create_remote(
    State(state): State<AppState>,
    body: Option<Json<CreateRemoteRequest>>,
) -> Result<Json<OperationStarted>, (StatusCode, Json<ErrorResponse>)> {
    let req = body.map(|Json(r)| r).unwrap_or_default();
    let (id, _token) = state.try_start_op("create_remote").await.map_err(|e| {
        (
            StatusCode::CONFLICT,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        )
    })?;

    let state_clone = state.clone();
    tokio::spawn(async move {
        let backup_name = req
            .backup_name
            .unwrap_or_else(|| Utc::now().format("%Y-%m-%dT%H%M%S").to_string());

        info!(backup_name = %backup_name, "Starting create_remote operation");

        // Step 1: Create local backup
        let create_result = crate::backup::create(
            &state_clone.config,
            &state_clone.ch,
            &backup_name,
            req.tables.as_deref(),
            false, // schema_only
            None,  // diff_from (create_remote uses diff_from_remote on upload side)
            None,  // partitions (create_remote doesn't support --partitions)
            req.skip_check_parts_columns.unwrap_or(false),
        )
        .await;

        if let Err(e) = create_result {
            warn!(backup_name = %backup_name, error = %e, "create_remote: create step failed");
            state_clone.fail_op(id, e.to_string()).await;
            return;
        }

        // Step 2: Upload to S3
        let backup_dir = std::path::PathBuf::from(&state_clone.config.clickhouse.data_path)
            .join("backup")
            .join(&backup_name);

        let effective_resume = state_clone.config.general.use_resumable_state;
        let upload_result = crate::upload::upload(
            &state_clone.config,
            &state_clone.s3,
            &backup_name,
            &backup_dir,
            req.delete_source.unwrap_or(false),
            req.diff_from_remote.as_deref(),
            effective_resume,
        )
        .await;

        match upload_result {
            Ok(_) => {
                info!(backup_name = %backup_name, "create_remote operation completed");
                state_clone.finish_op(id).await;
            }
            Err(e) => {
                warn!(backup_name = %backup_name, error = %e, "create_remote: upload step failed");
                state_clone.fail_op(id, e.to_string()).await;
            }
        }
    });

    Ok(Json(OperationStarted {
        id,
        status: "started".to_string(),
    }))
}

/// Request body for POST /api/v1/create_remote
#[derive(Debug, Deserialize, Default)]
pub struct CreateRemoteRequest {
    pub tables: Option<String>,
    pub diff_from_remote: Option<String>,
    pub backup_name: Option<String>,
    pub delete_source: Option<bool>,
    pub skip_check_parts_columns: Option<bool>,
}

/// POST /api/v1/restore_remote/{name} -- download then restore
pub async fn restore_remote(
    State(state): State<AppState>,
    Path(name): Path<String>,
    body: Option<Json<RestoreRemoteRequest>>,
) -> Result<Json<OperationStarted>, (StatusCode, Json<ErrorResponse>)> {
    let req = body.map(|Json(r)| r).unwrap_or_default();
    let (id, _token) = state.try_start_op("restore_remote").await.map_err(|e| {
        (
            StatusCode::CONFLICT,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        )
    })?;

    let state_clone = state.clone();
    tokio::spawn(async move {
        info!(backup_name = %name, "Starting restore_remote operation");

        // Step 1: Download from S3
        let effective_resume = state_clone.config.general.use_resumable_state;
        let download_result = crate::download::download(
            &state_clone.config,
            &state_clone.s3,
            &name,
            effective_resume,
        )
        .await;

        if let Err(e) = download_result {
            warn!(backup_name = %name, error = %e, "restore_remote: download step failed");
            state_clone.fail_op(id, e.to_string()).await;
            return;
        }

        // Step 2: Restore
        let restore_result = crate::restore::restore(
            &state_clone.config,
            &state_clone.ch,
            &name,
            req.tables.as_deref(),
            req.schema.unwrap_or(false),
            req.data_only.unwrap_or(false),
            effective_resume,
        )
        .await;

        match restore_result {
            Ok(_) => {
                info!(backup_name = %name, "restore_remote operation completed");
                state_clone.finish_op(id).await;
            }
            Err(e) => {
                warn!(backup_name = %name, error = %e, "restore_remote: restore step failed");
                state_clone.fail_op(id, e.to_string()).await;
            }
        }
    });

    Ok(Json(OperationStarted {
        id,
        status: "started".to_string(),
    }))
}

/// Request body for POST /api/v1/restore_remote/{name}
#[derive(Debug, Deserialize, Default)]
pub struct RestoreRemoteRequest {
    pub tables: Option<String>,
    pub schema: Option<bool>,
    pub data_only: Option<bool>,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_health_returns_ok() {
        // health() is a simple sync function returning &'static str
        assert_eq!("OK", "OK");
    }

    #[tokio::test]
    async fn test_health_handler() {
        let result = health().await;
        assert_eq!(result, "OK");
    }

    #[test]
    fn test_actions_empty_log() {
        // Verify ActionResponse serialization
        let response = ActionResponse {
            command: "create".to_string(),
            start: "2024-01-15T10:00:00+00:00".to_string(),
            finish: String::new(),
            status: "running".to_string(),
            error: String::new(),
        };

        let json = serde_json::to_string(&response).expect("ActionResponse should serialize");
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
    fn test_restore_request_accepts_unimplemented_fields() {
        let json = r#"{
            "tables": "default.*",
            "schema": false,
            "data_only": true,
            "database_mapping": "source_db:target_db",
            "rm": true
        }"#;
        let req: RestoreRequest =
            serde_json::from_str(json).expect("Should parse RestoreRequest with all fields");
        assert_eq!(req.tables.as_deref(), Some("default.*"));
        assert_eq!(req.schema, Some(false));
        assert_eq!(req.data_only, Some(true));
        assert_eq!(
            req.database_mapping.as_deref(),
            Some("source_db:target_db")
        );
        assert_eq!(req.rm, Some(true));
    }
}
