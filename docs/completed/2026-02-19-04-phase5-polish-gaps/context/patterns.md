# Pattern Discovery

No global patterns directory exists (`docs/patterns/` not found). Patterns discovered locally from the codebase.

## 1. API Endpoint Pattern (for items 1, 2)

Reference: existing endpoints in `src/server/routes.rs` and `src/server/mod.rs`

### Read-only endpoint pattern (GET, synchronous)

```
// 1. Define query params struct
#[derive(Debug, Deserialize)]
pub struct FooParams {
    pub field: Option<String>,
}

// 2. Handler function (returns data directly, no background spawn)
pub async fn handler(
    State(state): State<AppState>,
    Query(params): Query<FooParams>,
) -> Result<Json<Vec<FooResponse>>, (StatusCode, Json<ErrorResponse>)> {
    // ... synchronous logic ...
    Ok(Json(results))
}

// 3. Wire in build_router() in mod.rs
.route("/api/v1/foo", get(routes::handler))
```

Example: `list_backups` (routes.rs:217), `watch_status` (routes.rs:1265)

### Operation endpoint pattern (POST, background spawn)

```
pub async fn handler(State(state): State<AppState>, ...) -> Result<Json<OperationStarted>, ...> {
    let (id, _token) = state.try_start_op("command")
        .map_err(|e| (StatusCode::CONFLICT, Json(ErrorResponse { error: e.to_string() })))?;
    let state_clone = state.clone();
    tokio::spawn(async move {
        let result = do_operation(&state_clone).await;
        match result {
            Ok(_) => state_clone.finish_op(id).await,
            Err(e) => state_clone.fail_op(id, e.to_string()).await,
        }
    });
    Ok(Json(OperationStarted { id, status: "started".into() }))
}
```

Example: `create_backup` (routes.rs:295), `upload_backup` (routes.rs:380)

## 2. Shadow Walk Part Filtering Pattern (for item 3)

In `collect.rs:236-328`, the shadow walk iterates part directories and applies skip conditions:
- Line 245: `if part_name == "frozen_metadata.txt" { continue; }` -- skip non-part files
- Line 251: `if !checksums_path.exists() { continue; }` -- skip non-parts

Projection filtering would be inserted at approximately line 248, after the part_name is extracted but before checksums.txt verification, since `.proj` subdirectories are directories inside part directories, not separate parts. However, the design says "Skip projection directories matching `--skip-projections` patterns" which refers to skipping `.proj/` subdirectories within parts during the hardlink step.

Actually, re-reading the design more carefully: projections are subdirectories with `.proj` extension INSIDE part directories. The skip-projections flag should cause these subdirectories to be excluded during the hardlink_dir step. The approach would be to pass a filter closure or list to `hardlink_dir()`.

## 3. Error Type Mapping Pattern (for item 6)

Current: `main()` returns `anyhow::Result<()>`. When Ok, exits 0. When Err, anyhow prints the error and exits 1.

The design specifies codes 0/1/2/3/4/130/143. Implementation approach:
- Catch the `Result` in main, inspect error type/message to determine exit code
- Map `ChBackupError` variants and error messages to specific codes
- Use `std::process::exit(code)` after printing the error

## 4. Config Flag Pattern (for item 5)

The `disable_progress_bar` at `config.rs:47` follows the standard bool config pattern with `#[serde(default)]`.

## 5. BackupSummary Extension Pattern (for item 7)

`BackupSummary` (list.rs:28) is a display struct populated from `BackupManifest`. Currently missing `metadata_size`. Adding a field follows the existing pattern:
- Add field to `BackupSummary` struct
- Populate it in `parse_backup_summary()` from `manifest.metadata_size`
- Populate it in the remote listing path from downloaded manifest
- Read it in `summary_to_list_response()` instead of hardcoding 0
