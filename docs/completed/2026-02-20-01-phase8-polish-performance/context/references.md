# Symbol and Reference Analysis

## Phase 1: Symbol Analysis

### BackupManifest (src/manifest.rs:16)

**Current fields (16):**
`manifest_version`, `name`, `timestamp`, `clickhouse_version`, `chbackup_version`, `data_format`, `compressed_size`, `metadata_size`, `disks`, `disk_types`, `disk_remote_paths`, `tables`, `databases`, `functions`, `named_collections`, `rbac`

**Missing fields for Gap 1:** `rbac_size: u64`, `config_size: u64`

**References (78 occurrences across 13 files):**
- `src/manifest.rs` (12) -- struct definition, impls, tests
- `src/list.rs` (14) -- parse_backup_summary, list_remote, gc_collect_referenced_keys
- `src/backup/mod.rs` (7) -- create() builds manifest
- `src/backup/rbac.rs` (5) -- backup_rbac_and_configs sets manifest fields
- `src/backup/diff.rs` (6) -- diff_parts comparison
- `src/upload/mod.rs` (3) -- reads manifest for upload
- `src/download/mod.rs` (3) -- reads manifest for download
- `src/restore/mod.rs` (5) -- reads manifest for restore
- `src/restore/schema.rs` (10) -- reads DDL from manifest
- `src/restore/topo.rs` (6) -- reads dependencies
- `src/restore/rbac.rs` (2) -- reads rbac info
- `src/server/routes.rs` (3) -- tables endpoint, summary_to_list_response
- `src/main.rs` (2) -- CLI commands

**Impact of adding rbac_size/config_size:** Only `backup/mod.rs` (creation), `list.rs` (reading), and `server/routes.rs` (API response) need changes. All other consumers ignore unknown fields (serde default).

### BackupSummary (src/list.rs:41)

**Current fields (8):**
`name`, `timestamp`, `size`, `compressed_size`, `table_count`, `metadata_size`, `is_broken`, `broken_reason`

**Missing fields for Gap 1:** `rbac_size: u64`, `config_size: u64`

**References (52 occurrences across 4 files):**
- `src/lib.rs` (3) -- re-exports
- `src/list.rs` (41) -- construction in list_local, list_remote, parse_backup_summary; sorting, filtering, format_list_output
- `src/server/routes.rs` (2) -- summary_to_list_response, list_backups
- `src/watch/mod.rs` (6) -- watch loop reads list for resume

**Construction sites (all must add new fields):**
1. `list.rs:1051-1060` -- broken backup (metadata not found) -- set to 0
2. `list.rs:1064-1073` -- valid local backup from manifest -- read from manifest
3. `list.rs:1082-1091` -- broken backup (parse error) -- set to 0
4. `list.rs:314-323` -- valid remote backup from manifest -- read from manifest
5. `list.rs:332-342` -- broken remote backup (parse error) -- set to 0
6. `list.rs:351-360` -- broken remote backup (manifest not found) -- set to 0
7. Tests in `list.rs` that construct BackupSummary -- must add new fields

### list_remote (src/list.rs:295)

**Callers (12 call sites):**
- `src/main.rs:707` -- `list::list_remote(s3).await`
- `src/list.rs:94` -- inside `list()` function
- `src/list.rs:114` -- inside `list()` for shortcut resolve
- `src/list.rs:499` -- inside `clean_broken_remote()`
- `src/list.rs:667` -- inside `gc_collect_referenced_keys()`
- `src/list.rs:854` -- inside `retention_remote()`
- `src/server/routes.rs:296` -- inside `list_backups()`
- `src/server/routes.rs:1644` -- inside `refresh_backup_counts()`
- `src/watch/mod.rs:338` -- watch loop resume state

**Impact of manifest caching (Gap 3):** The 4 server/watch call sites would benefit from caching. The 5 list.rs call sites (clean_broken, gc, retention) should invalidate or bypass the cache since they mutate state. The CLI call (main.rs) runs once and exits, no caching benefit.

### summary_to_list_response (src/server/routes.rs:317)

**Callers (2):**
- `routes.rs:285` -- local backups in list_backups
- `routes.rs:299` -- remote backups in list_backups

**Current implementation (lines 317-331):**
```rust
fn summary_to_list_response(s: list::BackupSummary, location: &str) -> ListResponse {
    ListResponse {
        // ... other fields ...
        rbac_size: 0,        // TODO: requires scanning access/ directory sizes
        config_size: 0,      // TODO: requires adding config_size to BackupManifest
        // ...
    }
}
```
Will change to `rbac_size: s.rbac_size` and `config_size: s.config_size` once BackupSummary has these fields.

### dir_size (src/backup/collect.rs:485)

**Current visibility:** Private (`fn dir_size`)
**Signature:** `fn dir_size(path: &Path) -> Result<u64>`
**Callers (0 external):** Only used internally within collect.rs (no callers found by grep).

Wait -- let me verify this. There is one call site within collect.rs.

**Internal callers in collect.rs:**
- Searched for `dir_size` in collect.rs -- found definition at line 485 only.
- Actually NOT called anywhere currently. It exists as a helper but is unused at present.

**Plan:** Make `pub` so `backup::rbac` or `backup::mod` can use it for computing `rbac_size` and `config_size`.

### compress_part (src/upload/stream.rs:42)

**Callers:**
- `src/upload/mod.rs:509` -- in the upload parallel pipeline (the ONLY production call site)
- `src/download/stream.rs:88` -- a separate `compress_part` in download module (different function)
- Multiple test call sites in both upload/stream.rs and download/stream.rs

**Signature:** `pub fn compress_part(part_dir: &Path, archive_name: &str, data_format: &str, compression_level: u32) -> Result<Vec<u8>>`

**Gap 5 impact:** Streaming variant will coexist alongside this. Only `upload/mod.rs:509` call site needs decision logic (use buffered for small parts, streaming for large).

### AppState (src/server/state.rs:55)

**Current fields (11):**
`config`, `ch`, `s3`, `action_log`, `current_op`, `op_semaphore`, `metrics`, `watch_shutdown_tx`, `watch_reload_tx`, `watch_status`, `config_path`

**Missing for Gap 3:** A manifest cache field (e.g., `manifest_cache: Arc<Mutex<ManifestCache>>`)

**Construction:** `AppState::new()` at state.rs:93 -- must add cache initialization.

### TablesParams (src/server/routes.rs:87)

**Current fields (3):** `table`, `all`, `backup`
**Missing for Gap 2:** `offset: Option<usize>`, `limit: Option<usize>`

**Usage:** `tables()` handler at routes.rs:1340 -- destructures `Query(params)`.

### Signal Handlers

**Server SIGHUP (src/server/mod.rs:211-224):**
```rust
#[cfg(unix)]
{
    let reload_tx_clone = reload_tx.clone();
    tokio::spawn(async move {
        use tokio::signal::unix::{signal, SignalKind};
        let mut sighup = signal(SignalKind::hangup())
            .expect("failed to register SIGHUP handler");
        loop {
            sighup.recv().await;
            info!("SIGHUP received, triggering config reload");
            reload_tx_clone.send(true).ok();
        }
    });
}
```

**Standalone SIGHUP (src/main.rs:584-598):** Same pattern.

**Gap 4 pattern:** Follow identical structure but with `SignalKind::quit()` and `Backtrace::capture()` instead of channel send.

## Phase 1.5: LSP Call Hierarchy Analysis

### upload() outgoing calls (src/upload/mod.rs:152)
Key calls in the upload pipeline:
- `BackupManifest::load_from_file()` -- load manifest
- `stream::compress_part()` -- compression (via spawn_blocking)
- `should_use_multipart()` -- threshold check
- `s3.put_object_with_retry()` -- single upload
- `s3.create_multipart_upload()` -> `s3.upload_part_with_retry()` -> `s3.complete_multipart_upload()` -- multipart
- `s3.copy_object_with_retry()` -- S3 disk parts
- `upload_simple_directory()` -- RBAC/config uploads
- `upload_metadata_files()` -- per-disk metadata

### backup::create() outgoing calls (src/backup/mod.rs:79)
Key calls:
- `ch.list_tables()` / `ch.list_all_tables()` -- table discovery
- `ch.freeze_table()` / `ch.freeze_partition()` -- FREEZE
- `collect::collect_parts()` -- shadow walk
- `rbac::backup_rbac_and_configs()` -- RBAC/config backup
- `diff::diff_parts()` -- incremental diff
- `manifest.save_to_file()` -- persist manifest

### list_remote() incoming callers
See list_remote callers section above (12 call sites across 5 files).

### gc_collect_referenced_keys() incoming callers
- `src/list.rs:854` -- inside `retention_remote()` (called for each backup to delete)

This function calls `list_remote()` internally, which re-downloads all manifests. This is the primary target for caching optimization (Gap 3) -- in a server/watch context with multiple retention deletions, this N*M manifest download pattern is the bottleneck.

## Cross-Reference with Discovery Context

### Verified from symbols.md
All types confirmed via LSP `documentSymbol`:
- `BackupManifest` has 16 fields at src/manifest.rs:16 -- matches symbols.md
- `BackupSummary` has 8 fields at src/list.rs:41 -- matches symbols.md
- `ListResponse` has 11 fields including `rbac_size` and `config_size` at routes.rs:71 -- matches
- `TablesParams` has 3 fields at routes.rs:87 -- matches
- `AppState` has 11 fields at state.rs:55 -- matches
- `compress_part` signature confirmed at stream.rs:42 -- matches

### Verified from patterns.md
- SIGHUP handler pattern at server/mod.rs:211-224 -- confirmed
- Buffered upload pattern at upload/mod.rs:490-575 -- confirmed
- `dir_size` helper at collect.rs:485 -- confirmed (private, needs pub)
- Query param pattern (all fields `Option<T>`) -- confirmed in `TablesParams`

### Verified from affected-modules.md
- `backup_rbac_and_configs()` at rbac.rs:45 -- confirmed, takes `&mut BackupManifest`
- Manifest construction at backup/mod.rs:640-657 -- confirmed, no rbac_size/config_size fields
- `summary_to_list_response()` at routes.rs:317 -- confirmed, hardcodes 0

## Design Doc References

| Gap | Design Doc Section | Quote |
|-----|--------------------|-------|
| Gap 1 (rbac_size/config_size) | Implied by integration table schema (client.rs:1414-1415) | `rbac_size UInt64, config_size UInt64` |
| Gap 2 (tables pagination) | CLAUDE.md "Remaining Limitations" | "API tables endpoint has no pagination" |
| Gap 3 (manifest caching) | Section 8.2, line 1759-1760 | "cache manifest key-sets in memory when running in watch/server mode" |
| Gap 3 (list remote cache) | Section 8.4, line 1796 | "cache remote backup metadata locally...with TTL (default 5 minutes)" |
| Gap 4 (SIGQUIT) | Section 11.5, line 2391 | "Dump all goroutine/task stacks to stderr (debugging), then continue" |
| Gap 5 (streaming multipart) | CLAUDE.md "Remaining Limitations" | "No streaming multipart upload" |
