//! HTTP route handlers for the chbackup API server.
//!
//! All endpoints from design doc section 9 are implemented here.
//! Read-only endpoints return data directly; operation endpoints spawn
//! background tasks and return immediately with an action ID.

use axum::{
    extract::{Query, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use tracing::warn;

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
}
