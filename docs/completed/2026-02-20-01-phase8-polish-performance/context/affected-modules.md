# Affected Modules Analysis

## Summary

- **Modules to update CLAUDE.md:** 3
- **Modules to create CLAUDE.md:** 0
- **Files modified (no CLAUDE.md):** 2 (single files in src/)
- **Git base:** c11d7794

## Files Being Modified

| File / Module | CLAUDE.md Status | Triggers | Action |
|---------------|-----------------|----------|--------|
| src/manifest.rs | N/A (single file) | new_fields (rbac_size, config_size) | Modify only |
| src/list.rs | N/A (single file) | new_fields, new_function (cache support) | Modify only |
| src/server/ | EXISTS | new_query_params, new_cache_struct | UPDATE CLAUDE.md |
| src/upload/ | EXISTS | new_streaming_function | UPDATE CLAUDE.md |
| src/backup/ | EXISTS | pub_fn_change (dir_size), rbac_size computation | UPDATE CLAUDE.md |
| src/storage/ | EXISTS | (none -- existing APIs used) | No update needed |
| src/main.rs | N/A (single file) | SIGQUIT handler (standalone watch) | Modify only |

## CLAUDE.md Tasks

1. **Update:** `src/server/CLAUDE.md` -- Add ManifestCache documentation, tables pagination, SIGQUIT handler
2. **Update:** `src/upload/CLAUDE.md` -- Add streaming multipart upload documentation
3. **Update:** `src/backup/CLAUDE.md` -- Document `pub fn dir_size()` and rbac_size/config_size computation

## Architecture Assumptions (VALIDATED)

### Component Ownership

- **BackupManifest**: Created by `backup::create()`, stored as JSON file, read by `list`, `upload`, `download`, `restore`, `server/routes`
- **BackupSummary**: Created by `list::list_local()` and `list::list_remote()`, consumed by `server/routes::summary_to_list_response()`
- **ListResponse**: Created by `summary_to_list_response()`, returned by GET /api/v1/list
- **AppState**: Created by `start_server()`, shared across all axum handlers via `State<AppState>`
- **S3Client**: Created in `main.rs` or `start_server()`, shared via Arc/ArcSwap
- **Upload pipeline**: Entry via `upload::upload()`, compression in `upload::stream::compress_part()`, S3 ops in `storage::s3`

### Data Flow for rbac_size/config_size

```
backup::create()
  -> backup::rbac::backup_rbac_and_configs() creates access/ and configs/ dirs
  -> dir_size(access_dir) + dir_size(configs_dir) computed
  -> stored in BackupManifest.rbac_size, .config_size
  -> manifest.save_to_file() persists to JSON

list::parse_backup_summary() / list::list_remote()
  -> reads manifest
  -> copies rbac_size, config_size to BackupSummary

server::routes::summary_to_list_response()
  -> copies from BackupSummary to ListResponse
```

### Data Flow for manifest caching

```
AppState.manifest_cache: Arc<Mutex<ManifestCache>>

list::list_remote(s3) -> Vec<BackupSummary>  (uncached, existing)
list::list_remote_cached(s3, cache) -> Vec<BackupSummary>  (cached, new)

Cache invalidated by:
  - upload::upload() completion
  - list::delete_remote()
  - list::retention_remote()
  - list::clean_broken() (remote)

Cache populated by:
  - list_backups handler (GET /api/v1/list)
  - retention_remote calls within watch loop
  - gc_collect_referenced_keys
```

### What This Plan CANNOT Do

- Cannot make pagination work for ClickHouse URL engine tables (system.backup_list) -- URL engine does not support LIMIT/OFFSET pushdown, ClickHouse handles it client-side
- Cannot implement true streaming upload without buffering at least one multipart chunk (5MB S3 minimum part size)
- Cannot cache manifests across process restarts (in-memory only; design doc does not specify persistent caching)
- Cannot handle SIGQUIT on Windows (Unix-only signal; gated by `#[cfg(unix)]`)
