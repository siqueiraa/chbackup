# Symbol and Reference Analysis

## Symbols Analyzed

### 1. post_actions (src/server/routes.rs:217)

**Definition:** `pub async fn post_actions(State(state): State<AppState>, Json(body): Json<Vec<ActionRequest>>) -> Result<(StatusCode, Json<OperationStarted>), (StatusCode, Json<ErrorResponse>)>`

**Incoming calls:**
- `build_router()` in src/server/mod.rs:50 -- registered as `.post(routes::post_actions)`

**Outgoing calls from spawned task (line 248-254):**
- `tracing::info!` -- logs command
- `state_clone.finish_op(id)` -- marks as completed (stub behavior)

**What it SHOULD call (based on existing route handler patterns):**
- For "create": mirrors `create_backup()` handler logic (routes.rs:342-417)
- For "upload": mirrors `upload_backup()` handler logic (routes.rs:434-507)
- For "download": mirrors `download_backup()` handler logic (routes.rs:517-575)
- For "restore": mirrors `restore_backup()` handler logic (routes.rs:584-672)
- For "create_remote": mirrors `create_remote()` handler logic (routes.rs:693-806)
- For "delete": mirrors `delete_backup()` handler logic (routes.rs:956-1036)
- For "clean_broken": mirrors `clean_remote_broken()` handler logic (routes.rs:1039-1097)

**Command string format:** `"create_remote daily_backup"` -- first word is the command, rest is the argument (backup name).

### 2. list_backups (src/server/routes.rs:274)

**Definition:** `pub async fn list_backups(State(state): State<AppState>, Query(params): Query<ListParams>) -> Result<Json<Vec<ListResponse>>, (StatusCode, Json<ErrorResponse>)>`

**Incoming calls:**
- `build_router()` in src/server/mod.rs:52 -- registered as `get(routes::list_backups)`

**Current return type:** `Result<Json<Vec<ListResponse>>, ...>`
**Needed return type:** `Result<(headers, Json<Vec<ListResponse>>), ...>` for X-Total-Count header

**Pattern to follow:** `tables()` at routes.rs:1353 which returns:
```rust
Result<([(HeaderName, HeaderValue); 1], Json<Vec<TablesResponseEntry>>), ...>
```

### 3. ListParams (src/server/routes.rs:64-69)

**Definition:**
```rust
pub struct ListParams {
    pub location: Option<String>,
    pub desc: Option<bool>,
}
```

**References (deserialization):**
- `list_backups()` at routes.rs:276 -- `Query(params): Query<ListParams>`
- Test `test_tables_params_deserialization` at routes.rs:1941 (tests TablesParams, not ListParams)

**Fields to add:** `offset: Option<usize>`, `limit: Option<usize>`, `format: Option<String>`

### 4. summary_to_list_response (src/server/routes.rs:321)

**Definition:** `fn summary_to_list_response(s: list::BackupSummary, location: &str) -> ListResponse`

**Incoming calls:**
- `list_backups()` at routes.rs:289 -- for local summaries
- `list_backups()` at routes.rs:303 -- for remote summaries
- Test `test_summary_to_list_response_sizes` at routes.rs:2139

**Current hardcoded values:**
- `object_disk_size: 0` (line 328)
- `required: String::new()` (line 333)
- `data_size: s.size` (line 327) -- copies total size, should be `size - object_disk_size`

### 5. BackupSummary (src/list.rs:46-68)

**Definition:** 10 fields (name, timestamp, size, compressed_size, table_count, metadata_size, rbac_size, config_size, is_broken, broken_reason)

**Construction sites that must be updated if fields are added:**

| Location | File:Line | Context |
|----------|-----------|---------|
| list_remote() OK path | list.rs:394 | Remote manifest parsed successfully |
| list_remote() parse error | list.rs:414 | Broken - manifest parse failed |
| list_remote() missing | list.rs:435 | Broken - metadata.json not found |
| parse_backup_summary() missing | list.rs:1189 | Local broken - file missing |
| parse_backup_summary() OK | list.rs:1204 | Local manifest parsed successfully |
| parse_backup_summary() error | list.rs:1224 | Local broken - parse error |
| Tests (8 sites) | list.rs:1482,2106,2161,2186,2206,2229,2251,2288-2357 | Unit tests |
| Test in lib.rs | lib.rs:193 | Integration test |
| Test in routes.rs | routes.rs:2126 | API test |
| watch/mod.rs test | watch/mod.rs:722 | Watch module test |

**Total construction sites:** ~18 (6 production + ~12 test)

### 6. ManifestCache (src/list.rs:72-108)

**References:**
- `list_remote_cached()` at list.rs:113 -- uses cache
- `AppState.manifest_cache` at state.rs:84 -- stored in server state
- Upload, delete, clean handlers -- invalidate cache

**Cache stores:** `Vec<BackupSummary>` -- if BackupSummary gets new fields, cache is automatically updated (no structural change needed).

### 7. is_s3_disk (src/object_disk.rs:280)

**Definition:** `pub fn is_s3_disk(disk_type: &str) -> bool` -- returns true for "s3" or "object_storage"

**References:**
- upload/mod.rs:31,304,1444 -- disk routing in upload pipeline
- Tests in object_disk.rs:512-518

**For MISSING-4:** Will be called during BackupSummary construction to compute object_disk_size.

### 8. collect_incremental_bases (src/list.rs:929)

**Definition:** `async fn collect_incremental_bases(s3: &S3Client, surviving_names: &[&str]) -> HashSet<String>`

**Incoming calls:**
- `retention_remote()` at list.rs:1016

**For MISSING-2:** The `carried:` prefix parsing logic on line 959 is the same logic needed to extract `required` backup names from a single manifest. However, `collect_incremental_bases` is async (needs S3), while the `required` field extraction from a single manifest is synchronous -- it just iterates `manifest.tables.values()` -> `parts.values()` -> check `source.strip_prefix("carried:")`.

### 9. SIGTERM handling gap

**Existing signals in server/mod.rs:**
- Line 302-313 (TLS path): `tokio::signal::ctrl_c()` for shutdown
- Line 333-344 (plain path): `tokio::signal::ctrl_c()` for graceful shutdown
- Line 217-228: `SignalKind::hangup()` for SIGHUP config reload
- Line 239-254: `SignalKind::quit()` for SIGQUIT stack dump

**Existing signals in main.rs (standalone watch):**
- Line 567-571: `tokio::signal::ctrl_c()` for shutdown
- Line 576-586: `SignalKind::hangup()` for SIGHUP config reload
- Line 591-605: `SignalKind::quit()` for SIGQUIT stack dump

**Neither handles SIGTERM.** In K8s, `kubectl delete pod` sends SIGTERM. Both main.rs (standalone watch) and server/mod.rs should handle SIGTERM for graceful shutdown.

### 10. ListFormat enum (src/list.rs:30-42)

**Definition:**
```rust
pub enum ListFormat { Default, Json, Yaml, Csv, Tsv }
```

**References:**
- `format_list_output()` at list.rs:205 -- formats output string
- CLI dispatch in main.rs -- parses `--format` flag
- Not used in API server at all

**For MISSING-1:** If `format` query param is added to the list API endpoint, it needs to support the same formats. However, the API already returns JSON (via axum Json response). The `format` param would only be useful for text/csv/tsv output -- this may be a lower priority change since API consumers typically want JSON.

## Cross-Module Dependencies

```
BUG-1 (post_actions dispatch):
  routes.rs -> backup::create, upload::upload, download::download
  routes.rs -> restore::restore, list::delete_local, list::delete_remote
  routes.rs -> list::clean_broken_remote, list::clean_broken_local
  All existing handler patterns are in routes.rs itself

MISSING-1 (list pagination):
  routes.rs ListParams -> add offset/limit
  routes.rs list_backups() -> change return type to include headers
  No other modules affected

MISSING-2 (required field):
  list.rs BackupSummary -> add required: String field
  list.rs parse_backup_summary() -> extract from manifest.tables
  list.rs list_remote() -> extract from manifest.tables
  routes.rs summary_to_list_response() -> read s.required
  manifest.rs PartInfo.source -> read "carried:..." prefix
  object_disk.rs is_s3_disk() -> not needed for this

MISSING-3 (SIGTERM):
  server/mod.rs -> add SignalKind::terminate() handler
  main.rs -> add SignalKind::terminate() handler for standalone watch
  No other modules affected

MISSING-4 (object_disk_size):
  list.rs BackupSummary -> add object_disk_size: u64 field
  list.rs parse_backup_summary() -> compute from manifest disk_types + parts
  list.rs list_remote() -> compute from manifest disk_types + parts
  routes.rs summary_to_list_response() -> read s.object_disk_size
  object_disk.rs is_s3_disk() -> needed for disk type check
```
