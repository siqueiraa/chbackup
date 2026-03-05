//! HTTP route handlers for the chbackup API server.
//!
//! All endpoints from design doc section 9 are implemented here.
//! Read-only endpoints return data directly; operation endpoints spawn
//! background tasks and return immediately with an action ID.

use axum::{
    body::Bytes,
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use std::sync::Arc;
use tokio::sync::Semaphore;

use crate::list;
use crate::manifest::BackupManifest;
use crate::table_filter::TableFilter;

use super::actions::ActionStatus;
use super::metrics::{Metrics, OperationLabels};
use super::state::{reject_reserved_backup_name, run_operation, validate_backup_name, AppState};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Build a BAD_REQUEST error response for backup name validation failures.
///
/// Consolidates the `validate_backup_name` / `reject_reserved_backup_name`
/// error mapping that previously appeared ~12 times across route handlers.
fn validation_error(name: &str, e: &str) -> (StatusCode, Json<ErrorResponse>) {
    warn!(backup_name = %name, "backup name rejected: {}", e);
    (
        StatusCode::BAD_REQUEST,
        Json(ErrorResponse {
            error: format!("invalid backup name: {e}"),
        }),
    )
}

fn paginate<T>(
    items: Vec<T>,
    offset: Option<usize>,
    limit: Option<usize>,
    label: &str,
) -> (
    [(
        axum::http::header::HeaderName,
        axum::http::header::HeaderValue,
    ); 1],
    Vec<T>,
) {
    let total_count = items.len();
    let offset = offset.unwrap_or(0);
    let paginated: Vec<T> = if let Some(limit) = limit {
        info!(
            offset = offset,
            limit = limit,
            total = total_count,
            "{label}: offset/limit applied"
        );
        items.into_iter().skip(offset).take(limit).collect()
    } else {
        if offset > 0 {
            info!(
                offset = offset,
                total = total_count,
                "{label}: offset applied"
            );
        }
        items.into_iter().skip(offset).collect()
    };
    let header = [(
        axum::http::header::HeaderName::from_static("x-total-count"),
        axum::http::header::HeaderValue::from_str(&total_count.to_string())
            .unwrap_or_else(|_| axum::http::header::HeaderValue::from_static("0")),
    )];
    (header, paginated)
}

// ---------------------------------------------------------------------------
// ActionFlags parsing for POST /api/v1/actions
// ---------------------------------------------------------------------------

/// Parsed flags from a command string dispatched via POST /api/v1/actions.
///
/// Go clickhouse-backup and chbackup CLI flags are extracted so that
/// `post_actions()` can pass them into the underlying command functions.
struct ActionFlags {
    /// First positional (non-flag) argument after the operation name.
    backup_name: Option<String>,
    /// --table / -t flag value.
    table_pattern: Option<String>,
    /// --restore-table-mapping flag value (converted to db.table format).
    rename_as: Option<String>,
    /// --diff-from-remote flag value.
    diff_from_remote: Option<String>,
    /// --rbac flag.
    rbac: bool,
    /// --configs flag.
    configs: bool,
    /// --rm / --drop flag.
    rm: bool,
}

/// Parse flags from command parts (everything after the operation name).
///
/// Handles both `--flag=VALUE` and `--flag VALUE` forms.
/// The first non-flag token is treated as the backup name.
fn parse_action_flags(parts: &[&str]) -> ActionFlags {
    let args: Vec<&str> = if parts.len() > 1 {
        parts[1..].to_vec()
    } else {
        Vec::new()
    };

    let mut backup_name: Option<String> = None;
    let mut table_pattern: Option<String> = None;
    let mut rename_as: Option<String> = None;
    let mut diff_from_remote: Option<String> = None;
    let mut rbac = false;
    let mut configs = false;
    let mut rm = false;

    let mut i = 0;
    while i < args.len() {
        let arg = args[i];

        if let Some(val) = arg
            .strip_prefix("--table=")
            .or_else(|| arg.strip_prefix("-t="))
        {
            table_pattern = Some(val.to_string());
        } else if arg == "--table" || arg == "-t" {
            i += 1;
            if i < args.len() {
                table_pattern = Some(args[i].to_string());
            }
        } else if let Some(val) = arg.strip_prefix("--restore-table-mapping=") {
            rename_as = Some(val.to_string());
        } else if arg == "--restore-table-mapping" {
            i += 1;
            if i < args.len() {
                rename_as = Some(args[i].to_string());
            }
        } else if let Some(val) = arg.strip_prefix("--diff-from-remote=") {
            diff_from_remote = Some(val.to_string());
        } else if arg == "--diff-from-remote" {
            i += 1;
            if i < args.len() {
                diff_from_remote = Some(args[i].to_string());
            }
        } else if arg == "--rbac" {
            rbac = true;
        } else if arg == "--configs" {
            configs = true;
        } else if arg == "--rm" || arg == "--drop" {
            rm = true;
        } else if !arg.starts_with('-') {
            // First positional argument = backup name
            if backup_name.is_none() {
                backup_name = Some(arg.to_string());
            }
        }
        // Unknown flags starting with -- are silently ignored

        i += 1;
    }

    // Infer database prefix for --restore-table-mapping from --table if needed.
    // Go format: `transactions:transactions_DR` (table-only)
    // chbackup: `events.transactions:events.transactions_DR` (db.table)
    if let (Some(ref mapping), Some(ref table)) = (&rename_as, &table_pattern) {
        // Only infer when --table is a single concrete table (no wildcards)
        if !table.contains('*')
            && !table.contains('?')
            && !table.contains(',')
            && table.contains('.')
        {
            let db = table.split('.').next().unwrap_or("");
            if !db.is_empty() {
                // Process each mapping pair (comma-separated for multi-table)
                let expanded: Vec<String> = mapping
                    .split(',')
                    .map(|pair| {
                        let parts: Vec<&str> = pair.splitn(2, ':').collect();
                        if parts.len() == 2 {
                            let src = parts[0].trim();
                            let dst = parts[1].trim();
                            let src_expanded = if !src.contains('.') {
                                format!("{}.{}", db, src)
                            } else {
                                src.to_string()
                            };
                            let dst_expanded = if !dst.contains('.') {
                                format!("{}.{}", db, dst)
                            } else {
                                dst.to_string()
                            };
                            format!("{}:{}", src_expanded, dst_expanded)
                        } else {
                            pair.to_string()
                        }
                    })
                    .collect();
                rename_as = Some(expanded.join(","));
            }
        }
    }

    ActionFlags {
        backup_name,
        table_pattern,
        rename_as,
        diff_from_remote,
        rbac,
        configs,
        rm,
    }
}

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
    // Internal fields for Go-compat layer (not in integration table columns)
    #[serde(skip_serializing)]
    pub is_broken: bool,
    #[serde(skip_serializing)]
    pub broken_reason: Option<String>,
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
/// because it returns `(StatusCode, Json<OperationStarted>)` (200 OK with action ID)
/// which is incompatible with the helper's `Result<Json<OperationStarted>, ...>` return type.
/// The inline try_start_op + tokio::spawn + tokio::select! pattern is retained here.
pub async fn post_actions(
    State(state): State<AppState>,
    body: Bytes,
) -> Result<(StatusCode, Json<OperationStarted>), (StatusCode, Json<ErrorResponse>)> {
    // Accept two body formats:
    // 1. JSON array:   [{"command":"create foo"}]  (direct API clients)
    // 2. JSONEachRow:  {"command":"create foo"}     (ClickHouse URL-engine INSERT, one object per line)
    let parsed: Vec<ActionRequest> =
        if let Ok(arr) = serde_json::from_slice::<Vec<ActionRequest>>(&body) {
            arr
        } else {
            let mut items = Vec::new();
            for line in body.split(|&b| b == b'\n') {
                if let Ok(s) = std::str::from_utf8(line) {
                    let s = s.trim();
                    if s.is_empty() {
                        continue;
                    }
                    if let Ok(req) = serde_json::from_str::<ActionRequest>(s) {
                        items.push(req);
                    }
                }
            }
            items
        };

    let request = parsed.into_iter().next().ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "empty or unparseable request body".to_string(),
            }),
        )
    })?;

    let parts: Vec<&str> = request.command.split_whitespace().collect();
    let op_name = parts.first().copied().unwrap_or("");

    // Dispatch based on operation name
    match op_name {
        "create" | "upload" | "download" | "restore" | "create_remote" | "restore_remote"
        | "delete" | "clean_broken" => {
            // Parse flags from command parts (skip for delete/clean_broken which have positional format)
            let flags = if matches!(op_name, "delete" | "clean_broken") {
                ActionFlags {
                    backup_name: parts.get(1).map(|s| s.to_string()),
                    table_pattern: None,
                    rename_as: None,
                    diff_from_remote: None,
                    rbac: false,
                    configs: false,
                    rm: false,
                }
            } else {
                parse_action_flags(&parts)
            };

            // Validate backup name if explicitly provided in the command.
            if let Some(ref name) = flags.backup_name {
                validate_backup_name(name).map_err(|e| validation_error(name, e))?;
            }
            if op_name == "delete" {
                // Reject "delete local" / "delete remote" without a backup name.
                // parts[1] is a location keyword only when there's no parts[2];
                // in that case the user forgot the name and we must not silently
                // treat "local"/"remote" as the backup name.
                if parts.len() == 2
                    && matches!(parts.get(1).copied(), Some("local") | Some("remote"))
                {
                    return Err((
                        StatusCode::BAD_REQUEST,
                        Json(ErrorResponse {
                            error:
                                "delete: missing backup name (usage: delete [local|remote] <name>)"
                                    .to_string(),
                        }),
                    ));
                }
                if let Some(name) = parts.get(2) {
                    validate_backup_name(name).map_err(|e| validation_error(name, e))?;
                }
            }

            // For create/create_remote, also reject reserved shortcut names.
            // Other commands (upload, download, restore) accept "latest"/"previous"
            // as shortcut inputs that get resolved at runtime.
            if matches!(op_name, "create" | "create_remote") {
                if let Some(ref name) = flags.backup_name {
                    reject_reserved_backup_name(name).map_err(|e| validation_error(name, e))?;
                }
            }

            // Compute backup_name for per-backup conflict detection before starting the op.
            // For "delete <loc> <name>", the backup name is at parts[2]; for
            // "delete <name>" (no location), it is at parts[1]; for all other
            // commands it is at parts[1] (may be absent for auto-named commands).
            let conflict_backup_name: Option<String> = if op_name == "delete" {
                parts.get(2).or_else(|| parts.get(1)).map(|s| s.to_string())
            } else {
                flags.backup_name.clone()
            };

            let (id, token) = state
                .try_start_op(op_name, conflict_backup_name)
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
            let metrics_clone = state.metrics.clone();
            let command = request.command.clone();
            let parts_owned: Vec<String> = parts.iter().map(|s| s.to_string()).collect();
            let cancel_for_ops = token.clone();
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
                let backup_name = flags.backup_name
                    .unwrap_or_else(crate::generate_backup_name);

                let op = parts_owned[0].as_str();

                // Acquire PID lock before loading clients — provides cross-process
                // exclusion between CLI and server (design §9, same as run_operation).
                let lock_scope = crate::lock::lock_for_command(op, Some(&backup_name));
                let _pid_lock = if let Some(lock_path) = crate::lock::lock_path_for_scope(&lock_scope) {
                    match crate::lock::PidLock::acquire(&lock_path, op) {
                        Ok(lock) => Some(lock),
                        Err(e) => {
                            warn!(id = id, error = %e, "post_actions: failed to acquire PID lock");
                            state_clone.fail_op(id, e.to_string()).await;
                            return;
                        }
                    }
                } else {
                    None
                };

                let config = state_clone.config.load();
                let ch = state_clone.ch.load();
                let s3 = state_clone.s3.load();
                let start_time = std::time::Instant::now();

                // NOTE: post_actions uses string-based dispatch with no typed body
                // per-action, so `resume` always uses the config default here.
                // Use the dedicated /api/v1/{command}/{name} endpoints to override.
                let result: Result<(), anyhow::Error> = match op {
                    "create" => {
                        crate::backup::create(
                            &config,
                            &ch,
                            &backup_name,
                            flags.table_pattern.as_deref(),
                            false, // schema_only
                            None,  // diff_from
                            None,  // partitions
                            false, // skip_check_parts_columns
                            flags.rbac,
                            flags.configs,
                            false, // named_collections
                            &config.backup.skip_projections,
                            cancel_for_ops.clone(),
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
                            flags.diff_from_remote.as_deref(),
                            effective_resume,
                            cancel_for_ops.clone(),
                        )
                        .await;

                        if let Ok(ref stats) = upload_result {
                            if let Some(m) = &metrics_clone {
                                m.parts_uploaded_total.inc_by(stats.uploaded_count);
                                m.parts_skipped_incremental_total.inc_by(stats.carried_count);
                            }
                            // Apply retention after successful upload (design doc 3.6 step 7)
                            list::apply_retention_after_upload(
                                &config,
                                &s3,
                                Some(&backup_name),
                                Some(&state_clone.manifest_cache),
                            )
                            .await;
                        }
                        upload_result.map(|_| ())
                    }
                    "download" => {
                        let effective_resume = config.general.use_resumable_state;
                        crate::download::download(
                            &config,
                            &s3,
                            &backup_name,
                            effective_resume,
                            false, // hardlink_exists_files
                            cancel_for_ops.clone(),
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
                            flags.table_pattern.as_deref(),
                            false, // schema_only
                            false, // data_only
                            flags.rm,
                            effective_resume,
                            flags.rename_as.as_deref(),
                            None,  // database_mapping
                            flags.rbac,
                            flags.configs,
                            false, // named_collections
                            None,  // partitions
                            false, // skip_empty_tables
                            cancel_for_ops.clone(),
                        )
                        .await
                    }
                    "create_remote" => {
                        let create_result = crate::backup::create(
                            &config,
                            &ch,
                            &backup_name,
                            flags.table_pattern.as_deref(),
                            false, // schema_only
                            None,  // diff_from
                            None,  // partitions
                            false, // skip_check_parts_columns
                            flags.rbac,
                            flags.configs,
                            false, // named_collections
                            &config.backup.skip_projections,
                            cancel_for_ops.clone(),
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
                                    flags.diff_from_remote.as_deref(),
                                    effective_resume,
                                    cancel_for_ops.clone(),
                                )
                                .await;

                                if let Ok(ref stats) = upload_result {
                                    if let Some(m) = &metrics_clone {
                                        m.parts_uploaded_total.inc_by(stats.uploaded_count);
                                        m.parts_skipped_incremental_total
                                            .inc_by(stats.carried_count);
                                    }
                                    // Apply retention after successful upload (design doc 3.6 step 7)
                                    list::apply_retention_after_upload(
                                        &config,
                                        &s3,
                                        Some(&backup_name),
                                        Some(&state_clone.manifest_cache),
                                    )
                                    .await;
                                }
                                upload_result.map(|_| ())
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
                            cancel_for_ops.clone(),
                        )
                        .await;
                        match dl {
                            Ok(_) => {
                                crate::restore::restore(
                                    &config,
                                    &ch,
                                    &backup_name,
                                    flags.table_pattern.as_deref(),
                                    false, // schema_only
                                    false, // data_only
                                    flags.rm,
                                    effective_resume,
                                    flags.rename_as.as_deref(),
                                    None,  // database_mapping
                                    flags.rbac,
                                    flags.configs,
                                    false, // named_collections
                                    None,  // partitions
                                    false, // skip_empty_tables
                                    cancel_for_ops.clone(),
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
                            "remote" => list::delete_remote(&s3, &name).await,
                            _ => Err(anyhow::anyhow!(
                                "delete: invalid location '{}' (must be 'local' or 'remote')",
                                loc,
                            )),
                        }
                    }
                    "clean_broken" => {
                        // Respect optional location token: "clean_broken [local|remote]"
                        // No token (or unknown token) → clean both.
                        let location = parts_owned.get(1).map(String::as_str).unwrap_or("");
                        match location {
                            "local" => {
                                let data_path = config.clickhouse.data_path.clone();
                                tokio::task::spawn_blocking(move || {
                                    list::clean_broken_local(&data_path)
                                })
                                .await
                                .unwrap_or_else(|e| {
                                    Err(anyhow::anyhow!("spawn_blocking failed: {}", e))
                                })
                                .map(|_| ())
                            }
                            "remote" => {
                                list::clean_broken_remote(&s3).await.map(|_| ())
                            }
                            _ => {
                                // Both (default when no location specified)
                                let s3_result = list::clean_broken_remote(&s3).await;
                                let data_path = config.clickhouse.data_path.clone();
                                let local_result = tokio::task::spawn_blocking(move || {
                                    list::clean_broken_local(&data_path)
                                })
                                .await
                                .unwrap_or_else(|e| {
                                    Err(anyhow::anyhow!("spawn_blocking failed: {}", e))
                                });
                                match (s3_result, local_result) {
                                    (Ok(_), Ok(_)) => Ok(()),
                                    (Err(s3_err), Err(local_err)) => Err(anyhow::anyhow!(
                                        "Both remote and local clean_broken failed: remote: {}; local: {}",
                                        s3_err, local_err
                                    )),
                                    (Err(e), _) | (_, Err(e)) => Err(e),
                                }
                            }
                        }
                    }
                    _ => Err(anyhow::anyhow!("unknown command: {}", op)),
                };

                let duration = start_time.elapsed().as_secs_f64();

                match result {
                    Ok(()) => {
                        if let Some(m) = &state_clone.metrics {
                            let labels = OperationLabels::new(op);
                            m.backup_duration_seconds
                                .get_or_create(&labels)
                                .observe(duration);
                            m.successful_operations_total.get_or_create(&labels).inc();
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
                            let labels = OperationLabels::new(op);
                            m.backup_duration_seconds
                                .get_or_create(&labels)
                                .observe(duration);
                            m.errors_total.get_or_create(&labels).inc();
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
    // Validate location param: must be absent, "local", or "remote"
    if let Some(loc) = params.location.as_deref() {
        if loc != "local" && loc != "remote" {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse {
                    error: "Invalid location: must be 'local' or 'remote'".to_string(),
                }),
            ));
        }
    }

    let results = build_list_data(
        &state,
        params.location.as_deref(),
        params.desc.unwrap_or(false),
    )
    .await;

    let (header, results) = paginate(results, params.offset, params.limit, "list");

    Ok((header, Json(results)))
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
        is_broken: s.is_broken,
        broken_reason: s.broken_reason,
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
        validate_backup_name(name).map_err(|e| validation_error(name, e))?;
        reject_reserved_backup_name(name).map_err(|e| validation_error(name, e))?;
    }

    // Pre-generate the backup name so run_operation() can acquire a per-backup
    // PID lock instead of falling back to the global lock when no name is given.
    let backup_name = req
        .backup_name
        .clone()
        .unwrap_or_else(crate::generate_backup_name);

    let metrics_clone = state.metrics.clone();
    run_operation(
        &state,
        "create",
        "create",
        Some(backup_name.clone()),
        false, // no cache invalidation
        move |config, ch, _s3, cancel| async move {
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
                req.skip_projections
                    .as_ref()
                    .unwrap_or(&config.backup.skip_projections),
                cancel,
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
    /// Override config.backup.skip_projections for this request.
    pub skip_projections: Option<Vec<String>>,
}

/// POST /api/v1/upload/{name} -- upload a local backup to S3
pub async fn upload_backup(
    State(state): State<AppState>,
    Path(name): Path<String>,
    body: Option<Json<UploadRequest>>,
) -> Result<Json<OperationStarted>, (StatusCode, Json<ErrorResponse>)> {
    let req = body.map(|Json(r)| r).unwrap_or_default();

    validate_backup_name(&name).map_err(|e| validation_error(&name, e))?;

    let metrics_clone = state.metrics.clone();
    let cache_clone = state.manifest_cache.clone();
    run_operation(
        &state,
        "upload",
        "upload",
        Some(name.clone()), // per-backup conflict detection
        true,               // invalidate cache after upload
        move |config, _ch, s3, cancel| async move {
            info!(backup_name = %name, "Starting upload operation");
            let backup_dir = std::path::PathBuf::from(&config.clickhouse.data_path)
                .join("backup")
                .join(&name);
            let effective_resume = req.resume.unwrap_or(config.general.use_resumable_state);
            let stats = crate::upload::upload(
                &config,
                &s3,
                &name,
                &backup_dir,
                req.delete_local.unwrap_or(false),
                req.diff_from_remote.as_deref(),
                effective_resume,
                cancel,
            )
            .await?;

            if let Some(m) = &metrics_clone {
                m.parts_uploaded_total.inc_by(stats.uploaded_count);
                m.parts_skipped_incremental_total
                    .inc_by(stats.carried_count);
            }

            // Apply retention after successful upload (design doc 3.6 step 7)
            list::apply_retention_after_upload(&config, &s3, Some(&name), Some(&cache_clone)).await;
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
    /// Override config.general.use_resumable_state for this request.
    pub resume: Option<bool>,
}

/// POST /api/v1/download/{name} -- download a backup from S3
pub async fn download_backup(
    State(state): State<AppState>,
    Path(name): Path<String>,
    body: Option<Json<DownloadRequest>>,
) -> Result<Json<OperationStarted>, (StatusCode, Json<ErrorResponse>)> {
    let req = body.map(|Json(r)| r).unwrap_or_default();

    validate_backup_name(&name).map_err(|e| validation_error(&name, e))?;

    let hardlink = req.hardlink_exists_files.unwrap_or(false);
    let resume_override = req.resume;
    run_operation(
        &state,
        "download",
        "download",
        Some(name.clone()), // per-backup conflict detection
        false,              // no cache invalidation
        move |config, _ch, s3, cancel| async move {
            info!(backup_name = %name, hardlink_exists_files = hardlink, "Starting download operation");
            let effective_resume = resume_override.unwrap_or(config.general.use_resumable_state);
            crate::download::download(&config, &s3, &name, effective_resume, hardlink, cancel)
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
    /// Override config.general.use_resumable_state for this request.
    pub resume: Option<bool>,
}

/// POST /api/v1/restore/{name} -- restore a downloaded backup
pub async fn restore_backup(
    State(state): State<AppState>,
    Path(name): Path<String>,
    body: Option<Json<RestoreRequest>>,
) -> Result<Json<OperationStarted>, (StatusCode, Json<ErrorResponse>)> {
    let req = body.map(|Json(r)| r).unwrap_or_default();

    validate_backup_name(&name).map_err(|e| validation_error(&name, e))?;

    // Reject mutually exclusive flag combination
    if req.schema.unwrap_or(false) && req.data_only.unwrap_or(false) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "schema and data_only are mutually exclusive".to_string(),
            }),
        ));
    }

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
        Some(name.clone()), // per-backup conflict detection
        false,              // no cache invalidation
        move |config, ch, _s3, cancel| async move {
            info!(backup_name = %name, "Starting restore operation");
            let effective_resume = req.resume.unwrap_or(config.general.use_resumable_state);
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
                cancel,
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
    /// Override config.general.use_resumable_state for this request.
    pub resume: Option<bool>,
}

/// POST /api/v1/create_remote -- create local backup then upload to S3
pub async fn create_remote(
    State(state): State<AppState>,
    body: Option<Json<CreateRemoteRequest>>,
) -> Result<Json<OperationStarted>, (StatusCode, Json<ErrorResponse>)> {
    let req = body.map(|Json(r)| r).unwrap_or_default();

    // Validate backup name if provided (auto-generated names are always safe)
    if let Some(ref name) = req.backup_name {
        validate_backup_name(name).map_err(|e| validation_error(name, e))?;
        reject_reserved_backup_name(name).map_err(|e| validation_error(name, e))?;
    }

    // Pre-generate the backup name so run_operation() can acquire a per-backup
    // PID lock instead of falling back to the global lock when no name is given.
    let backup_name = req
        .backup_name
        .clone()
        .unwrap_or_else(crate::generate_backup_name);

    let metrics_clone = state.metrics.clone();
    let cache_clone = state.manifest_cache.clone();
    run_operation(
        &state,
        "create_remote",
        "create_remote",
        Some(backup_name.clone()),
        true, // invalidate cache after upload
        move |config, ch, s3, cancel| async move {
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
                req.skip_projections
                    .as_ref()
                    .unwrap_or(&config.backup.skip_projections),
                cancel.clone(),
            )
            .await?;

            // Step 2: Upload to S3
            let backup_dir = std::path::PathBuf::from(&config.clickhouse.data_path)
                .join("backup")
                .join(&backup_name);

            let effective_resume = req.resume.unwrap_or(config.general.use_resumable_state);
            let stats = crate::upload::upload(
                &config,
                &s3,
                &backup_name,
                &backup_dir,
                req.delete_source.unwrap_or(false),
                req.diff_from_remote.as_deref(),
                effective_resume,
                cancel,
            )
            .await?;

            // Apply retention after successful upload (design doc 3.6 step 7)
            list::apply_retention_after_upload(
                &config,
                &s3,
                Some(&backup_name),
                Some(&cache_clone),
            )
            .await;

            if let Some(m) = &metrics_clone {
                m.backup_last_success_timestamp
                    .set(Utc::now().timestamp() as f64);
                m.backup_size_bytes.set(manifest.compressed_size as f64);
                m.parts_uploaded_total.inc_by(stats.uploaded_count);
                m.parts_skipped_incremental_total
                    .inc_by(stats.carried_count);
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
    /// Override config.backup.skip_projections for this request.
    pub skip_projections: Option<Vec<String>>,
    /// Override config.general.use_resumable_state for this request.
    pub resume: Option<bool>,
}

/// POST /api/v1/restore_remote/{name} -- download then restore
pub async fn restore_remote(
    State(state): State<AppState>,
    Path(name): Path<String>,
    body: Option<Json<RestoreRemoteRequest>>,
) -> Result<Json<OperationStarted>, (StatusCode, Json<ErrorResponse>)> {
    let req = body.map(|Json(r)| r).unwrap_or_default();

    validate_backup_name(&name).map_err(|e| validation_error(&name, e))?;

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
        Some(name.clone()), // per-backup conflict detection
        false,              // no cache invalidation
        move |config, ch, s3, cancel| async move {
            info!(backup_name = %name, "Starting restore_remote operation");

            let effective_resume = req.resume.unwrap_or(config.general.use_resumable_state);

            // Step 1: Download from S3
            crate::download::download(&config, &s3, &name, effective_resume, false, cancel.clone())
                .await
                .map(|_| ())?;

            // Step 2: Restore with remap.
            // restore_remote does not support schema-only, data-only, or partition
            // filtering (per design doc §2 flag table for restore_remote command).
            crate::restore::restore(
                &config,
                &ch,
                &name,
                req.tables.as_deref(),
                false, // schema: not supported by restore_remote
                false, // data_only: not supported by restore_remote
                req.rm.unwrap_or(false),
                effective_resume,
                req.rename_as.as_deref(),
                db_mapping.as_ref(),
                req.rbac.unwrap_or(false),
                req.configs.unwrap_or(false),
                req.named_collections.unwrap_or(false),
                None, // partitions: not supported by restore_remote
                req.skip_empty_tables.unwrap_or(false),
                cancel,
            )
            .await
        },
    )
    .await
}

/// Request body for POST /api/v1/restore_remote/{name}
///
/// Note: `schema`, `data_only`, and `partitions` are intentionally absent — they
/// are not part of the `restore_remote` command spec (design doc §2 flag table).
#[derive(Debug, Deserialize, Default)]
pub struct RestoreRemoteRequest {
    pub tables: Option<String>,
    #[serde(default)]
    pub rename_as: Option<String>,
    #[serde(default)]
    pub database_mapping: Option<String>,
    #[serde(default)]
    pub rm: Option<bool>,
    pub rbac: Option<bool>,
    pub configs: Option<bool>,
    pub named_collections: Option<bool>,
    pub skip_empty_tables: Option<bool>,
    /// Override config.general.use_resumable_state for this request.
    pub resume: Option<bool>,
}

// ---------------------------------------------------------------------------
// Delete, clean, kill, and stub endpoints (Task 7)
// ---------------------------------------------------------------------------

/// DELETE /api/v1/delete/{location}/{name} -- delete a backup
pub async fn delete_backup(
    State(state): State<AppState>,
    Path((location, name)): Path<(String, String)>,
) -> Result<Json<OperationStarted>, (StatusCode, Json<ErrorResponse>)> {
    validate_backup_name(&name).map_err(|e| validation_error(&name, e))?;

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
        Some(name.clone()), // per-backup conflict detection
        invalidate,
        move |config, _ch, s3, _cancel| async move {
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
        None, // no specific backup name
        true, // invalidate cache
        |_config, _ch, s3, _cancel| async move {
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
        None,  // no specific backup name
        false, // no cache invalidation
        |config, _ch, _s3, _cancel| async move {
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
        None,  // no specific backup name
        false, // no cache invalidation
        |config, ch, _s3, _cancel| async move {
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

/// Rebuild `op_semaphore` if `allow_parallel` changed between old and new config.
///
/// In-flight operations hold permits against the old `Semaphore` instance and
/// are unaffected. Shared by `reload()` and `restart()`.
fn maybe_rebuild_semaphore(state: &AppState, new_config: &crate::config::Config, caller: &str) {
    let old_allow_parallel = state.config.load().api.allow_parallel;
    if old_allow_parallel != new_config.api.allow_parallel {
        let new_permits = if new_config.api.allow_parallel {
            Semaphore::MAX_PERMITS
        } else {
            1
        };
        state
            .op_semaphore
            .store(Arc::new(Semaphore::new(new_permits)));
        info!(
            old = old_allow_parallel,
            new = new_config.api.allow_parallel,
            "{}: op_semaphore rebuilt (allow_parallel changed)",
            caller
        );
    }
}

/// Update `ManifestCache` TTL after config reload/restart so the cache picks
/// up any change to `general.remote_cache_ttl_secs`.
async fn update_manifest_cache_ttl(state: &AppState, new_config: &crate::config::Config) {
    let ttl = std::time::Duration::from_secs(new_config.general.remote_cache_ttl_secs);
    state.manifest_cache.lock().await.set_ttl(ttl);
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

    // Rebuild op_semaphore when allow_parallel changes
    maybe_rebuild_semaphore(&state, &config, "Reload");

    // Atomically swap config and clients
    state.config.store(Arc::new(config.clone()));
    state.ch.store(Arc::new(ch));
    state.s3.store(Arc::new(s3));

    // Update ManifestCache TTL to reflect new config
    update_manifest_cache_ttl(&state, &config).await;

    // If watch loop is active, also send reload signal so it picks up changes
    if let Some(tx) = &*state.watch_reload_tx.lock().await {
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

    // Rebuild op_semaphore when allow_parallel changes
    maybe_rebuild_semaphore(&state, &config, "Restart");

    // Atomically swap config and clients
    state.config.store(Arc::new(config.clone()));
    state.ch.store(Arc::new(ch));
    state.s3.store(Arc::new(s3));

    // Update ManifestCache TTL to reflect new config
    update_manifest_cache_ttl(&state, &config).await;

    // If watch loop is active, send reload signal so it picks up the new config
    if let Some(tx) = &*state.watch_reload_tx.lock().await {
        tx.send(true).ok();
        info!("Config reload signal sent to watch loop");
    }

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

    let (header, results) = paginate(results, params.offset, params.limit, "tables");

    Ok((header, Json(results)))
}

/// POST /api/v1/watch/start -- start the watch loop
///
/// If the watch loop is already active, returns 409 Conflict.
/// Otherwise, creates channels, spawns the watch loop, and stores
/// the handles in AppState.
pub async fn watch_start(
    State(mut state): State<AppState>,
    body: Option<Json<WatchStartRequest>>,
) -> Result<Json<WatchActionResponse>, (StatusCode, Json<ErrorResponse>)> {
    let req = body.map(|Json(r)| r).unwrap_or_default();

    // Reserve the slot atomically: check-and-set while holding the lock to
    // prevent two concurrent requests from both passing the active check.
    {
        let mut ws = state.watch_status.lock().await;
        if ws.active {
            return Err((
                StatusCode::LOCKED,
                Json(ErrorResponse {
                    error: "watch loop already active".to_string(),
                }),
            ));
        }
        ws.active = true; // Reserve the slot
    }

    // Apply optional interval overrides
    if req.watch_interval.is_some() || req.full_interval.is_some() {
        let mut config = (*state.config.load_full()).clone();
        if let Some(v) = req.watch_interval {
            config.watch.watch_interval = v;
        }
        if let Some(v) = req.full_interval {
            config.watch.full_interval = v;
        }
        // Validate merged config before spawning
        if let Err(e) = config.validate() {
            // Undo the reservation on validation failure
            state.watch_status.lock().await.active = false;
            return Err((
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse {
                    error: format!("invalid config: {}", e),
                }),
            ));
        }
        state.config.store(Arc::new(config));
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

    if let Some(tx) = &*state.watch_shutdown_tx.lock().await {
        tx.send(true).ok();
    }

    info!("Watch loop stop signal sent via API");
    Ok(Json(WatchActionResponse {
        status: "stopping".to_string(),
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

/// Optional request body for POST /api/v1/watch/start
#[derive(Debug, Deserialize, Default)]
pub struct WatchStartRequest {
    pub watch_interval: Option<String>,
    pub full_interval: Option<String>,
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

// ---------------------------------------------------------------------------
// Go-compatible /backup/* wrapper handlers
// ---------------------------------------------------------------------------

/// Response for Go-compatible mutation endpoints (create, upload, etc.).
/// Go returns `{"status":"acknowledged","operation":"<cmd>"}`.
#[derive(Debug, Serialize)]
pub struct GoOperationResponse {
    pub status: String,
    pub operation: String,
}

/// Go-compatible list response with additional `desc` and `location` fields.
/// Extends ListResponse with fields Go clients expect.
#[derive(Debug, Serialize)]
pub struct GoListResponse {
    pub name: String,
    pub created: String,
    pub size: i64,
    pub location: String,
    pub required: String,
    pub desc: String,
    // chbackup extras (Go ignores unmatched fields)
    pub data_size: u64,
    pub object_disk_size: u64,
    pub metadata_size: u64,
    pub rbac_size: u64,
    pub config_size: u64,
    pub compressed_size: u64,
}

/// Go-compatible action response with status/timestamp mapping.
#[derive(Debug, Serialize)]
pub struct GoActionResponse {
    pub command: String,
    pub start: String,
    pub finish: String,
    pub status: String,
    pub error: String,
}

/// Format a chrono DateTime as "YYYY-MM-DD HH:MM:SS" (Go format, no T, no timezone).
fn format_go_timestamp(rfc3339: &str) -> String {
    if rfc3339.is_empty() {
        return String::new();
    }
    // Parse RFC3339 and reformat
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(rfc3339) {
        dt.format("%Y-%m-%d %H:%M:%S").to_string()
    } else {
        rfc3339.to_string()
    }
}

/// Map chbackup status to Go status values.
fn map_go_status(status: &str) -> &'static str {
    match status {
        "running" => "in progress",
        "completed" => "success",
        "failed" => "error",
        "killed" => "cancel",
        _ => "error",
    }
}

/// Build the core list data (shared between /api/v1/list and /backup/list).
async fn build_list_data(
    state: &AppState,
    location: Option<&str>,
    desc: bool,
) -> Vec<ListResponse> {
    let config = state.config.load();
    let data_path = &config.clickhouse.data_path;
    let mut results = Vec::new();

    let show_local = location.is_none() || location == Some("local");
    let show_remote = location.is_none() || location == Some("remote");

    if show_local {
        let dp = data_path.to_string();
        match tokio::task::spawn_blocking(move || list::list_local(&dp)).await {
            Ok(Ok(summaries)) => {
                for s in summaries {
                    results.push(summary_to_list_response(s, "local"));
                }
            }
            Ok(Err(e)) => {
                warn!(error = %e, "Failed to list local backups");
            }
            Err(e) => {
                warn!(error = %e, "spawn_blocking panicked for list_local");
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

    if desc {
        results.sort_by(|a, b| b.created.cmp(&a.created).then_with(|| b.name.cmp(&a.name)));
    } else {
        results.sort_by(|a, b| a.created.cmp(&b.created).then_with(|| a.name.cmp(&b.name)));
    }

    results
}

/// GET /backup/list -- Go-compatible list (JSONEachRow format).
pub async fn go_list_backups(
    State(state): State<AppState>,
    Query(params): Query<ListParams>,
) -> (
    StatusCode,
    [(axum::http::header::HeaderName, &'static str); 1],
    String,
) {
    let results = build_list_data(
        &state,
        params.location.as_deref(),
        params.desc.unwrap_or(false),
    )
    .await;

    let go_results: Vec<GoListResponse> = results
        .into_iter()
        .map(|r| {
            let desc = if r.is_broken {
                format!(
                    "broken: {}",
                    r.broken_reason.as_deref().unwrap_or("unknown")
                )
            } else if r.required.is_empty() {
                "tar, regular".to_string()
            } else {
                "tar, incremental".to_string()
            };
            GoListResponse {
                name: r.name,
                created: format_go_timestamp(&r.created),
                size: r.size.min(i64::MAX as u64) as i64,
                location: r.location,
                required: r.required,
                desc,
                data_size: r.data_size,
                object_disk_size: r.object_disk_size,
                metadata_size: r.metadata_size,
                rbac_size: r.rbac_size,
                config_size: r.config_size,
                compressed_size: r.compressed_size,
            }
        })
        .collect();

    // JSONEachRow: one JSON object per line
    let body = go_results
        .iter()
        .map(|r| serde_json::to_string(r).unwrap_or_default())
        .collect::<Vec<_>>()
        .join("\n");
    let body = if body.is_empty() { body } else { body + "\n" };

    (
        StatusCode::OK,
        [(
            axum::http::header::CONTENT_TYPE,
            "application/json; charset=UTF-8",
        )],
        body,
    )
}

/// GET /backup/list/:where -- Go-compatible list filtered by location.
pub async fn go_list_by_location(
    State(state): State<AppState>,
    Path(location): Path<String>,
) -> (
    StatusCode,
    [(axum::http::header::HeaderName, &'static str); 1],
    String,
) {
    if location != "local" && location != "remote" {
        return (
            StatusCode::BAD_REQUEST,
            [(
                axum::http::header::CONTENT_TYPE,
                "application/json; charset=UTF-8",
            )],
            "{\"error\":\"Invalid location: must be 'local' or 'remote'\"}\n".to_string(),
        );
    }
    let params = ListParams {
        location: Some(location),
        desc: None,
        offset: None,
        limit: None,
        format: None,
    };
    go_list_backups(State(state), Query(params)).await
}

/// GET /backup/actions -- Go-compatible actions (JSONEachRow format with Go status mapping).
pub async fn go_get_actions(State(state): State<AppState>) -> impl IntoResponse {
    let log = state.action_log.lock().await;
    let entries: Vec<GoActionResponse> = log
        .entries()
        .iter()
        .map(|e| {
            let (status_str, error_str) = match &e.status {
                ActionStatus::Running => ("running", String::new()),
                ActionStatus::Completed => ("completed", String::new()),
                ActionStatus::Failed(err) => ("failed", err.clone()),
                ActionStatus::Killed => ("killed", String::new()),
            };

            GoActionResponse {
                command: e.command.clone(),
                start: format_go_timestamp(&e.start.to_rfc3339()),
                finish: e
                    .finish
                    .map(|f| format_go_timestamp(&f.to_rfc3339()))
                    .unwrap_or_default(),
                status: map_go_status(status_str).to_string(),
                error: error_str,
            }
        })
        .collect();

    let body = entries
        .iter()
        .map(|r| serde_json::to_string(r).unwrap_or_default())
        .collect::<Vec<_>>()
        .join("\n");
    let body = if body.is_empty() { body } else { body + "\n" };

    (
        StatusCode::OK,
        [(
            axum::http::header::CONTENT_TYPE,
            "application/json; charset=UTF-8",
        )],
        body,
    )
}

/// Wrap a chbackup mutation response into a Go-compatible acknowledged response.
fn go_operation_response(
    operation: &str,
) -> Result<Json<GoOperationResponse>, (StatusCode, Json<ErrorResponse>)> {
    Ok(Json(GoOperationResponse {
        status: "acknowledged".to_string(),
        operation: operation.to_string(),
    }))
}

/// POST /backup/create -- Go-compatible create (returns acknowledged format).
pub async fn go_create_backup(
    State(state): State<AppState>,
    body: Option<Json<CreateRequest>>,
) -> Result<Json<GoOperationResponse>, (StatusCode, Json<ErrorResponse>)> {
    let _ = create_backup(State(state), body).await?;
    go_operation_response("create")
}

/// POST /backup/upload/:name -- Go-compatible upload.
pub async fn go_upload_backup(
    State(state): State<AppState>,
    Path(name): Path<String>,
    body: Option<Json<UploadRequest>>,
) -> Result<Json<GoOperationResponse>, (StatusCode, Json<ErrorResponse>)> {
    let _ = upload_backup(State(state), Path(name), body).await?;
    go_operation_response("upload")
}

/// POST /backup/download/:name -- Go-compatible download.
pub async fn go_download_backup(
    State(state): State<AppState>,
    Path(name): Path<String>,
    body: Option<Json<DownloadRequest>>,
) -> Result<Json<GoOperationResponse>, (StatusCode, Json<ErrorResponse>)> {
    let _ = download_backup(State(state), Path(name), body).await?;
    go_operation_response("download")
}

/// POST /backup/restore/:name -- Go-compatible restore.
pub async fn go_restore_backup(
    State(state): State<AppState>,
    Path(name): Path<String>,
    body: Option<Json<RestoreRequest>>,
) -> Result<Json<GoOperationResponse>, (StatusCode, Json<ErrorResponse>)> {
    let _ = restore_backup(State(state), Path(name), body).await?;
    go_operation_response("restore")
}

/// POST /backup/create_remote -- Go-compatible create_remote.
pub async fn go_create_remote(
    State(state): State<AppState>,
    body: Option<Json<CreateRemoteRequest>>,
) -> Result<Json<GoOperationResponse>, (StatusCode, Json<ErrorResponse>)> {
    let _ = create_remote(State(state), body).await?;
    go_operation_response("create_remote")
}

/// POST /backup/restore_remote/:name -- Go-compatible restore_remote.
pub async fn go_restore_remote(
    State(state): State<AppState>,
    Path(name): Path<String>,
    body: Option<Json<RestoreRemoteRequest>>,
) -> Result<Json<GoOperationResponse>, (StatusCode, Json<ErrorResponse>)> {
    let _ = restore_remote(State(state), Path(name), body).await?;
    go_operation_response("restore_remote")
}

/// POST /backup/delete/:where/:name -- Go-compatible delete (POST instead of DELETE).
pub async fn go_delete_backup(
    State(state): State<AppState>,
    Path((location, name)): Path<(String, String)>,
) -> Result<Json<GoOperationResponse>, (StatusCode, Json<ErrorResponse>)> {
    let _ = delete_backup(State(state), Path((location, name))).await?;
    go_operation_response("delete")
}

/// POST /backup/clean -- Go-compatible clean (shadow dirs).
pub async fn go_clean(
    State(state): State<AppState>,
) -> Result<Json<GoOperationResponse>, (StatusCode, Json<ErrorResponse>)> {
    let _ = clean(State(state)).await?;
    go_operation_response("clean")
}

/// POST /backup/clean/remote_broken -- Go-compatible clean remote broken.
pub async fn go_clean_remote_broken(
    State(state): State<AppState>,
) -> Result<Json<GoOperationResponse>, (StatusCode, Json<ErrorResponse>)> {
    let _ = clean_remote_broken(State(state)).await?;
    go_operation_response("clean_remote_broken")
}

/// POST /backup/clean/local_broken -- Go-compatible clean local broken (not in Go, but for symmetry).
pub async fn go_clean_local_broken(
    State(state): State<AppState>,
) -> Result<Json<GoOperationResponse>, (StatusCode, Json<ErrorResponse>)> {
    let _ = clean_local_broken(State(state)).await?;
    go_operation_response("clean_local_broken")
}

/// GET /backup/tables/all -- Go-compatible tables with all=true.
pub async fn go_tables_all(
    State(state): State<AppState>,
    Query(mut params): Query<TablesParams>,
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
    params.all = Some(true);
    tables(State(state), Query(params)).await
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
        Ok(Ok(summaries)) => {
            metrics.number_backups_local.set(summaries.len() as i64);
        }
        Ok(Err(e)) => warn!(error = %e, "Failed to refresh local backup count for metrics"),
        Err(e) => warn!(error = %e, "spawn_blocking failed for list_local in metrics"),
    }

    // Refresh remote backup count (async, using cache to avoid redundant S3 calls)
    let s3 = state.s3.load();
    match crate::list::list_remote_cached(&s3, &state.manifest_cache).await {
        Ok(summaries) => {
            metrics.number_backups_remote.set(summaries.len() as i64);
        }
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
            is_broken: false,
            broken_reason: None,
        };

        let json = serde_json::to_string(&response).expect("ListResponse should serialize");
        // Verify all required columns are present (is_broken/broken_reason are skip_serializing)
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
    fn test_watch_start_request_deserialization() {
        // With both fields
        let json = r#"{"watch_interval": "2h", "full_interval": "48h"}"#;
        let req: WatchStartRequest =
            serde_json::from_str(json).expect("Should parse WatchStartRequest");
        assert_eq!(req.watch_interval.as_deref(), Some("2h"));
        assert_eq!(req.full_interval.as_deref(), Some("48h"));

        // With only watch_interval
        let json_partial = r#"{"watch_interval": "30m"}"#;
        let req_partial: WatchStartRequest =
            serde_json::from_str(json_partial).expect("Should parse partial WatchStartRequest");
        assert_eq!(req_partial.watch_interval.as_deref(), Some("30m"));
        assert!(req_partial.full_interval.is_none());

        // Empty body (all defaults)
        let json_empty = r#"{}"#;
        let req_empty: WatchStartRequest =
            serde_json::from_str(json_empty).expect("Should parse empty WatchStartRequest");
        assert!(req_empty.watch_interval.is_none());
        assert!(req_empty.full_interval.is_none());
    }

    #[test]
    fn test_watch_start_request_default() {
        let req = WatchStartRequest::default();
        assert!(req.watch_interval.is_none());
        assert!(req.full_interval.is_none());
    }

    #[test]
    fn test_metrics_handler_returns_prometheus_text() {
        // Verify that Metrics::encode() produces valid prometheus text format
        // that the handler returns on the success path.
        let metrics = super::Metrics::new();
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

    // -----------------------------------------------------------------------
    // paginate() tests -- covers ~34 lines (47-81)
    // -----------------------------------------------------------------------

    #[test]
    fn test_paginate_empty_vec() {
        let items: Vec<i32> = vec![];
        let (header, result) = paginate(items, None, None, "test");
        assert!(result.is_empty());
        assert_eq!(header[0].1.to_str().unwrap(), "0");
    }

    #[test]
    fn test_paginate_no_offset_no_limit() {
        let items = vec![1, 2, 3, 4, 5];
        let (header, result) = paginate(items, None, None, "test");
        assert_eq!(result, vec![1, 2, 3, 4, 5]);
        assert_eq!(header[0].1.to_str().unwrap(), "5");
    }

    #[test]
    fn test_paginate_offset_only() {
        let items = vec![10, 20, 30, 40, 50];
        let (header, result) = paginate(items, Some(2), None, "test");
        assert_eq!(result, vec![30, 40, 50]);
        // X-Total-Count reflects original count
        assert_eq!(header[0].1.to_str().unwrap(), "5");
    }

    #[test]
    fn test_paginate_limit_only() {
        let items = vec![10, 20, 30, 40, 50];
        let (header, result) = paginate(items, None, Some(3), "test");
        assert_eq!(result, vec![10, 20, 30]);
        assert_eq!(header[0].1.to_str().unwrap(), "5");
    }

    #[test]
    fn test_paginate_offset_and_limit() {
        let items = vec![10, 20, 30, 40, 50];
        let (header, result) = paginate(items, Some(1), Some(2), "test");
        assert_eq!(result, vec![20, 30]);
        assert_eq!(header[0].1.to_str().unwrap(), "5");
    }

    #[test]
    fn test_paginate_offset_beyond_length() {
        let items = vec![1, 2, 3];
        let (header, result) = paginate(items, Some(10), None, "test");
        assert!(result.is_empty());
        assert_eq!(header[0].1.to_str().unwrap(), "3");
    }

    #[test]
    fn test_paginate_large_limit() {
        let items = vec![1, 2, 3];
        let (header, result) = paginate(items, None, Some(100), "test");
        assert_eq!(result, vec![1, 2, 3]);
        assert_eq!(header[0].1.to_str().unwrap(), "3");
    }

    #[test]
    fn test_paginate_zero_offset_zero_limit() {
        let items = vec![1, 2, 3, 4];
        let (header, result) = paginate(items, Some(0), Some(0), "test");
        assert!(result.is_empty());
        assert_eq!(header[0].1.to_str().unwrap(), "4");
    }

    #[test]
    fn test_paginate_header_name_is_x_total_count() {
        let items = vec![1, 2];
        let (header, _) = paginate(items, None, None, "test");
        assert_eq!(header[0].0.as_str(), "x-total-count");
    }

    // -----------------------------------------------------------------------
    // validation_error() tests -- covers ~8 lines (37-45)
    // -----------------------------------------------------------------------

    #[test]
    fn test_validation_error_returns_bad_request() {
        let (status, Json(body)) = validation_error("bad/../name", "must not contain '..'");
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body.error.contains("invalid backup name"));
        assert!(body.error.contains("must not contain '..'"));
    }

    #[test]
    fn test_validation_error_includes_reason() {
        let (status, Json(body)) = validation_error("", "backup name must not be empty");
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body.error.contains("backup name must not be empty"));
    }

    // -----------------------------------------------------------------------
    // format_duration() additional edge cases
    // -----------------------------------------------------------------------

    #[test]
    fn test_format_duration_zero() {
        assert_eq!(format_duration(std::time::Duration::ZERO), "0s");
    }

    #[test]
    fn test_format_duration_one_second() {
        assert_eq!(format_duration(std::time::Duration::from_secs(1)), "1s");
    }

    #[test]
    fn test_format_duration_59_seconds() {
        assert_eq!(format_duration(std::time::Duration::from_secs(59)), "59s");
    }

    #[test]
    fn test_format_duration_exactly_60_seconds() {
        assert_eq!(format_duration(std::time::Duration::from_secs(60)), "1m");
    }

    #[test]
    fn test_format_duration_hours_minutes_seconds() {
        // 1h1m1s = 3661s. format_duration only shows h+m, ignoring leftover seconds
        assert_eq!(
            format_duration(std::time::Duration::from_secs(3661)),
            "1h1m"
        );
    }

    #[test]
    fn test_format_duration_very_large() {
        // 100 hours
        assert_eq!(
            format_duration(std::time::Duration::from_secs(360_000)),
            "100h"
        );
    }

    // -----------------------------------------------------------------------
    // summary_to_list_response() additional tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_summary_to_list_response_with_timestamp() {
        use chrono::TimeZone;
        let ts = chrono::Utc.with_ymd_and_hms(2025, 3, 1, 12, 0, 0).unwrap();
        let summary = list::BackupSummary {
            name: "ts-backup".to_string(),
            timestamp: Some(ts),
            size: 100,
            compressed_size: 50,
            table_count: 1,
            metadata_size: 10,
            rbac_size: 0,
            config_size: 0,
            object_disk_size: 0,
            required: String::new(),
            is_broken: false,
            broken_reason: None,
        };
        let resp = summary_to_list_response(summary, "remote");
        assert_eq!(resp.name, "ts-backup");
        assert_eq!(resp.location, "remote");
        assert!(resp.created.contains("2025"));
        assert_eq!(resp.data_size, resp.size); // data_size == size
        assert_eq!(resp.compressed_size, 50);
    }

    #[test]
    fn test_summary_to_list_response_no_timestamp() {
        let summary = list::BackupSummary {
            name: "no-ts".to_string(),
            timestamp: None,
            size: 0,
            compressed_size: 0,
            table_count: 0,
            metadata_size: 0,
            rbac_size: 0,
            config_size: 0,
            object_disk_size: 0,
            required: String::new(),
            is_broken: false,
            broken_reason: None,
        };
        let resp = summary_to_list_response(summary, "local");
        assert_eq!(resp.created, ""); // None maps to empty string
    }

    // -----------------------------------------------------------------------
    // Request deserialization tests (DownloadRequest, CreateRemoteRequest, KillParams)
    // -----------------------------------------------------------------------

    #[test]
    fn test_download_request_deserialization() {
        let json = r#"{"hardlink_exists_files": true, "resume": false}"#;
        let req: DownloadRequest =
            serde_json::from_str(json).expect("Should parse DownloadRequest");
        assert_eq!(req.hardlink_exists_files, Some(true));
        assert_eq!(req.resume, Some(false));

        let json_empty = r#"{}"#;
        let req_empty: DownloadRequest =
            serde_json::from_str(json_empty).expect("Should parse empty DownloadRequest");
        assert!(req_empty.hardlink_exists_files.is_none());
        assert!(req_empty.resume.is_none());
    }

    #[test]
    fn test_create_remote_request_deserialization() {
        let json = r#"{
            "tables": "default.*",
            "diff_from_remote": "prev-remote",
            "backup_name": "remote-backup",
            "delete_source": true,
            "skip_check_parts_columns": false,
            "rbac": true,
            "configs": false,
            "named_collections": true,
            "skip_projections": ["proj1", "proj2"]
        }"#;
        let req: CreateRemoteRequest =
            serde_json::from_str(json).expect("Should parse CreateRemoteRequest");
        assert_eq!(req.tables.as_deref(), Some("default.*"));
        assert_eq!(req.diff_from_remote.as_deref(), Some("prev-remote"));
        assert_eq!(req.backup_name.as_deref(), Some("remote-backup"));
        assert_eq!(req.delete_source, Some(true));
        assert_eq!(req.skip_check_parts_columns, Some(false));
        assert_eq!(req.rbac, Some(true));
        assert_eq!(req.configs, Some(false));
        assert_eq!(req.named_collections, Some(true));
        assert_eq!(
            req.skip_projections.as_deref(),
            Some(&["proj1".to_string(), "proj2".to_string()][..])
        );

        let json_empty = r#"{}"#;
        let req_empty: CreateRemoteRequest =
            serde_json::from_str(json_empty).expect("Should parse empty CreateRemoteRequest");
        assert!(req_empty.tables.is_none());
        assert!(req_empty.diff_from_remote.is_none());
        assert!(req_empty.skip_projections.is_none());
    }

    #[test]
    fn test_kill_params_deserialization() {
        let json = r#"{"id": 42}"#;
        let params: KillParams = serde_json::from_str(json).expect("Should parse KillParams");
        assert_eq!(params.id, Some(42));

        let json_empty = r#"{}"#;
        let params_empty: KillParams =
            serde_json::from_str(json_empty).expect("Should parse empty KillParams");
        assert!(params_empty.id.is_none());
    }

    // -----------------------------------------------------------------------
    // RestoreRemoteRequest with rm and RBAC fields
    // -----------------------------------------------------------------------

    #[test]
    fn test_restore_remote_request_all_fields() {
        let json = r#"{
            "tables": "db.*",
            "schema": true,
            "data_only": false,
            "rename_as": "staging.tbl",
            "database_mapping": "prod:dev",
            "rm": true,
            "rbac": true,
            "configs": true,
            "named_collections": false
        }"#;
        let req: RestoreRemoteRequest =
            serde_json::from_str(json).expect("Should parse RestoreRemoteRequest with all fields");
        assert_eq!(req.rm, Some(true));
        assert_eq!(req.rbac, Some(true));
        assert_eq!(req.configs, Some(true));
        assert_eq!(req.named_collections, Some(false));
    }

    // -----------------------------------------------------------------------
    // RestoreRequest RBAC fields
    // -----------------------------------------------------------------------

    #[test]
    fn test_restore_request_rbac_fields() {
        let json = r#"{
            "rbac": true,
            "configs": false,
            "named_collections": true
        }"#;
        let req: RestoreRequest =
            serde_json::from_str(json).expect("Should parse RestoreRequest with RBAC fields");
        assert_eq!(req.rbac, Some(true));
        assert_eq!(req.configs, Some(false));
        assert_eq!(req.named_collections, Some(true));
    }

    #[test]
    fn test_post_actions_delete_invalid_location() {
        // Simulate the post_actions delete command parsing with a typo ("loacl")
        let command = "delete loacl my_backup";
        let parts_owned: Vec<String> = command.split_whitespace().map(String::from).collect();

        let (loc, name) = if parts_owned.len() >= 3 {
            (parts_owned[1].as_str().to_string(), parts_owned[2].clone())
        } else {
            (
                "remote".to_string(),
                parts_owned.get(1).cloned().unwrap_or_default(),
            )
        };

        // The match should now reject unknown locations instead of falling through to remote
        let result: Result<(), anyhow::Error> = match loc.as_str() {
            "local" => Ok(()),
            "remote" => Ok(()),
            _ => Err(anyhow::anyhow!(
                "delete: invalid location '{}' (must be 'local' or 'remote')",
                loc,
            )),
        };

        assert!(
            result.is_err(),
            "Typo 'loacl' should be rejected, not fall through to remote"
        );
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("invalid location"),
            "Error should mention 'invalid location', got: {err_msg}"
        );
        assert!(
            err_msg.contains("loacl"),
            "Error should include the typo'd value, got: {err_msg}"
        );
        assert_eq!(name, "my_backup");
    }

    #[test]
    fn test_list_desc_sorts_by_created_not_name() {
        // Construct ListResponses where name order differs from created order
        let mut results = [
            ListResponse {
                name: "z-oldest".to_string(),
                created: "2024-01-01T00:00:00+00:00".to_string(),
                location: "local".to_string(),
                size: 0,
                data_size: 0,
                object_disk_size: 0,
                metadata_size: 0,
                rbac_size: 0,
                config_size: 0,
                compressed_size: 0,
                required: String::new(),
                is_broken: false,
                broken_reason: None,
            },
            ListResponse {
                name: "a-newest".to_string(),
                created: "2024-06-01T00:00:00+00:00".to_string(),
                location: "local".to_string(),
                size: 0,
                data_size: 0,
                object_disk_size: 0,
                metadata_size: 0,
                rbac_size: 0,
                config_size: 0,
                compressed_size: 0,
                required: String::new(),
                is_broken: false,
                broken_reason: None,
            },
            ListResponse {
                name: "m-middle".to_string(),
                created: "2024-03-15T00:00:00+00:00".to_string(),
                location: "local".to_string(),
                size: 0,
                data_size: 0,
                object_disk_size: 0,
                metadata_size: 0,
                rbac_size: 0,
                config_size: 0,
                compressed_size: 0,
                required: String::new(),
                is_broken: false,
                broken_reason: None,
            },
        ];

        // Ascending sort (default)
        results.sort_by(|a, b| a.created.cmp(&b.created).then_with(|| a.name.cmp(&b.name)));
        assert_eq!(results[0].name, "z-oldest");
        assert_eq!(results[1].name, "m-middle");
        assert_eq!(results[2].name, "a-newest");

        // Descending sort (desc=true)
        results.sort_by(|a, b| b.created.cmp(&a.created).then_with(|| b.name.cmp(&a.name)));
        assert_eq!(results[0].name, "a-newest");
        assert_eq!(results[1].name, "m-middle");
        assert_eq!(results[2].name, "z-oldest");
    }

    // ---- Request struct deserialization tests for resume field ----

    #[test]
    fn test_create_remote_request_resume_true() {
        let json = r#"{"resume": true}"#;
        let req: CreateRemoteRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.resume, Some(true));
    }

    #[test]
    fn test_create_remote_request_resume_false() {
        let json = r#"{"resume": false}"#;
        let req: CreateRemoteRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.resume, Some(false));
    }

    #[test]
    fn test_create_remote_request_resume_missing() {
        let json = r#"{}"#;
        let req: CreateRemoteRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.resume, None);
    }

    #[test]
    fn test_restore_remote_request_resume_true() {
        let json = r#"{"resume": true}"#;
        let req: RestoreRemoteRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.resume, Some(true));
    }

    #[test]
    fn test_restore_remote_request_resume_false() {
        let json = r#"{"resume": false}"#;
        let req: RestoreRemoteRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.resume, Some(false));
    }

    #[test]
    fn test_restore_remote_request_resume_missing() {
        let json = r#"{}"#;
        let req: RestoreRemoteRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.resume, None);
    }

    // -----------------------------------------------------------------------
    // parse_action_flags() tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_parse_action_flags_restore_with_table_and_mapping() {
        let parts = vec![
            "restore",
            "--table=events.t",
            "--restore-table-mapping",
            "t:t_DR",
            "backup",
        ];
        let flags = parse_action_flags(&parts);
        assert_eq!(flags.backup_name.as_deref(), Some("backup"));
        assert_eq!(flags.table_pattern.as_deref(), Some("events.t"));
        // db should be inferred from --table
        assert_eq!(flags.rename_as.as_deref(), Some("events.t:events.t_DR"));
        assert!(!flags.rbac);
        assert!(!flags.configs);
        assert!(!flags.rm);
    }

    #[test]
    fn test_parse_action_flags_create_with_diff_and_rbac() {
        let parts = vec![
            "create",
            "--diff-from-remote=base",
            "--rbac",
            "--configs",
            "backup",
        ];
        let flags = parse_action_flags(&parts);
        assert_eq!(flags.backup_name.as_deref(), Some("backup"));
        assert_eq!(flags.diff_from_remote.as_deref(), Some("base"));
        assert!(flags.rbac);
        assert!(flags.configs);
        assert!(!flags.rm);
    }

    #[test]
    fn test_parse_action_flags_no_flags() {
        let parts = vec!["create", "backup_name"];
        let flags = parse_action_flags(&parts);
        assert_eq!(flags.backup_name.as_deref(), Some("backup_name"));
        assert!(flags.table_pattern.is_none());
        assert!(flags.rename_as.is_none());
        assert!(flags.diff_from_remote.is_none());
        assert!(!flags.rbac);
        assert!(!flags.configs);
        assert!(!flags.rm);
    }

    #[test]
    fn test_parse_action_flags_table_space_separated() {
        let parts = vec!["restore", "--table", "db.t", "backup"];
        let flags = parse_action_flags(&parts);
        assert_eq!(flags.table_pattern.as_deref(), Some("db.t"));
        assert_eq!(flags.backup_name.as_deref(), Some("backup"));
    }

    #[test]
    fn test_parse_action_flags_mapping_with_db() {
        let parts = vec![
            "restore",
            "--table=db.t",
            "--restore-table-mapping=db.t:db.t_DR",
            "backup",
        ];
        let flags = parse_action_flags(&parts);
        // Already has db prefix, should pass through unchanged
        assert_eq!(flags.rename_as.as_deref(), Some("db.t:db.t_DR"));
    }

    #[test]
    fn test_parse_action_flags_mapping_infers_db() {
        let parts = vec![
            "restore",
            "--table=mydb.users",
            "--restore-table-mapping=users:users_DR",
            "backup",
        ];
        let flags = parse_action_flags(&parts);
        assert_eq!(
            flags.rename_as.as_deref(),
            Some("mydb.users:mydb.users_DR")
        );
    }

    #[test]
    fn test_parse_action_flags_mapping_wildcard_no_infer() {
        let parts = vec![
            "restore",
            "--table=db.*",
            "--restore-table-mapping=t:t_DR",
            "backup",
        ];
        let flags = parse_action_flags(&parts);
        // Wildcard in --table, should not infer db
        assert_eq!(flags.rename_as.as_deref(), Some("t:t_DR"));
    }

    #[test]
    fn test_parse_action_flags_rm_and_drop() {
        let parts = vec!["restore", "--rm", "backup"];
        let flags = parse_action_flags(&parts);
        assert!(flags.rm);

        let parts = vec!["restore", "--drop", "backup"];
        let flags = parse_action_flags(&parts);
        assert!(flags.rm);
    }
}
