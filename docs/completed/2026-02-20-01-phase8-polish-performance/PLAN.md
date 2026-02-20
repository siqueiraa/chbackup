# Plan: Phase 8 -- Polish & Performance

## Goal

Close five remaining polish/performance gaps in chbackup: (1) populate `rbac_size`/`config_size` in manifest and list API, (2) add offset/limit pagination to the tables endpoint, (3) cache remote manifests in server/watch mode to avoid redundant S3 downloads, (4) implement SIGQUIT stack dump handler for debugging, and (5) replace the buffered multipart upload with a true streaming pipeline for large parts.

## Architecture Overview

Five independent improvements touching different modules:

1. **rbac_size/config_size (Gap 1):** Add two `u64` fields to `BackupManifest` (with `#[serde(default)]`), compute them from `dir_size()` after RBAC/config backup, propagate through `BackupSummary` to `ListResponse`.
2. **Tables pagination (Gap 2):** Add `offset`/`limit` `Option<usize>` fields to `TablesParams`, apply `.skip(offset).take(limit)` in the `tables()` handler.
3. **Remote manifest caching (Gap 3):** Introduce `ManifestCache` struct with TTL-based expiry (default 5 minutes, per design 8.4), store in `AppState`, use in `list_remote` call sites within the server, invalidate on mutating operations.
4. **SIGQUIT stack dump (Gap 4):** Spawn a `SignalKind::quit()` handler (following existing SIGHUP pattern) in both `server/mod.rs` and `main.rs` (standalone watch). On signal, capture `std::backtrace::Backtrace` and print all tokio task stacks to stderr.
5. **Streaming multipart upload (Gap 5):** Add `compress_part_streaming()` that pipes tar+compress output through a channel to multipart upload chunks, avoiding full-buffer-then-upload for parts exceeding a configurable threshold.

## Architecture Assumptions (VALIDATED)

### Component Ownership
- **BackupManifest**: Created by `backup::create()` in `src/backup/mod.rs`, stored as JSON, read by list/upload/download/restore/server
- **BackupSummary**: Created by `list::list_local()`, `list::list_remote()`, `list::parse_backup_summary()` in `src/list.rs`, consumed by `server::routes::summary_to_list_response()`
- **ListResponse**: Created by `summary_to_list_response()` in `src/server/routes.rs`, already has `rbac_size`/`config_size` fields (hardcoded to 0)
- **AppState**: Created by `start_server()` in `src/server/mod.rs`, shared across all axum handlers
- **dir_size()**: Already exists as private helper in `src/backup/collect.rs:485`, needs to be made `pub`
- **Upload pipeline**: Entry via `upload::upload()` in `src/upload/mod.rs`, compression in `upload::stream::compress_part()`, S3 ops in `src/storage/s3.rs`

### What This Plan CANNOT Do
- Cannot make tables pagination push down to ClickHouse URL engine (client-side pagination only)
- Cannot implement true streaming upload without buffering at least one 5MB S3 multipart chunk (S3 minimum part size)
- Cannot cache manifests across process restarts (in-memory only; design doc does not specify persistent cache)
- Cannot handle SIGQUIT on Windows (Unix-only signal; gated by `#[cfg(unix)]`)
- Cannot dump individual tokio task stacks (Rust has no goroutine-style dump; we use `std::backtrace::Backtrace` for the handler thread plus tokio runtime metrics if available)

## Risk Assessment

| Risk | Level | Mitigation |
|------|-------|------------|
| `#[serde(default)]` backward compat for manifest fields | GREEN | Same pattern as existing `metadata_size`, `compressed_size` fields |
| Manifest cache staleness | YELLOW | TTL-based expiry + explicit invalidation on mutating ops; design 8.4 specifies 5-minute TTL |
| Streaming upload memory correctness | YELLOW | Streaming path only used for large parts (>256MB uncompressed); buffered path unchanged for small parts; integration test required |
| SIGQUIT on non-Unix platforms | GREEN | Gated by `#[cfg(unix)]` same as existing SIGHUP handler |
| Tables pagination breaking existing clients | GREEN | `offset`/`limit` are `Option<usize>` with default `None` (all results) |

## Expected Runtime Logs

| Pattern | Required | Description |
|---------|----------|-------------|
| `rbac_size=` | yes | Logged when backup creates manifest with RBAC size computed |
| `config_size=` | yes | Logged when backup creates manifest with config size computed |
| `ManifestCache: populated` | yes | Logged when server caches manifests from S3 |
| `ManifestCache: invalidated` | yes | Logged when cache is cleared after mutation |
| `SIGQUIT received` | yes | Logged when SIGQUIT handler fires (test via `kill -3`) |
| `Streaming multipart upload` | yes | Logged when streaming path is chosen for large part |
| `tables: offset=` | yes | Logged when pagination params are applied |
| `ERROR:` | no (forbidden) | Should NOT appear in normal operation |

## Known Related Issues (Out of Scope)

| Issue | Reason | Future Plan |
|-------|--------|-------------|
| Persistent manifest cache (file-based) | Design 8.4 mentions `$TMPDIR` file cache but in-memory is simpler first step | Phase 9 if needed |
| `object_disk_size` in ListResponse | Requires manifest disk_types analysis, separate concern | Phase 9 |
| Parallel ATTACH within single table | Deferred since Phase 2a, tables parallel is sufficient | Not planned |
| `rbac_size`/`config_size` for remote backup without manifest cache | CLI runs once and exits, no caching benefit | N/A |

## Dependency Groups

```
Group A (Independent -- manifest fields):
  - Task 1: Add rbac_size/config_size to BackupManifest
  - Task 2: Compute sizes in backup::create() after RBAC backup
  - Task 3: Propagate through BackupSummary and ListResponse (depends on Task 1)

Group B (Independent -- tables pagination):
  - Task 4: Add offset/limit to TablesParams and tables() handler

Group C (Independent -- manifest caching):
  - Task 5: Implement ManifestCache struct with TTL
  - Task 6: Wire cache into AppState and server call sites (depends on Task 5)

Group D (Independent -- SIGQUIT):
  - Task 7: Add SIGQUIT handler to server and standalone watch

Group E (Independent -- streaming upload):
  - Task 8: Implement compress_part_streaming() in upload/stream.rs
  - Task 9: Wire streaming path into upload pipeline (depends on Task 8)

Group F (Final -- documentation):
  - Task 10: Update CLAUDE.md for all modified modules (MANDATORY)
```

## Tasks

### Task 1: Add rbac_size and config_size fields to BackupManifest and BackupSummary

**TDD Steps:**
1. Write failing test `test_manifest_rbac_config_size_fields` in `src/manifest.rs` that constructs a manifest with `rbac_size: 1024` and `config_size: 2048`, serializes to JSON, deserializes, and asserts the values roundtrip.
2. Add `rbac_size: u64` and `config_size: u64` fields to `BackupManifest` in `src/manifest.rs` with `#[serde(default)]`.
3. Write failing test `test_manifest_backward_compat_no_rbac_config_size` that deserializes a JSON string WITHOUT `rbac_size`/`config_size` and asserts both default to 0.
4. Verify both tests pass.
5. Add `rbac_size: u64` and `config_size: u64` fields to `BackupSummary` in `src/list.rs`.
6. Update ALL `BackupSummary` construction sites in `src/list.rs` (7 sites identified in context/references.md): set `rbac_size` and `config_size` from manifest fields (for valid backups) or 0 (for broken backups).
7. Update `sample_manifest()` test helper in `src/manifest.rs` to include the new fields.
8. Run `cargo test` to verify all existing tests still pass with the new fields added.

**Files:** `src/manifest.rs`, `src/list.rs`
**Acceptance:** F001

### Task 2: Compute rbac_size and config_size in backup::create()

**TDD Steps:**
1. Make `dir_size()` in `src/backup/collect.rs` public: change `fn dir_size` to `pub fn dir_size`.
2. Write unit test `test_dir_size_empty_dir` in `src/backup/collect.rs` that creates a tempdir and asserts `dir_size()` returns `Ok(0)`.
3. Write unit test `test_dir_size_with_files` that creates a tempdir with two files of known sizes and asserts `dir_size()` returns the sum.
4. In `src/backup/mod.rs`, after the call to `rbac::backup_rbac_and_configs()` (line ~669), compute sizes:
   - If `manifest.rbac.is_some()`, compute `manifest.rbac_size = collect::dir_size(&backup_dir.join("access"))?;`
   - If `backup_dir.join("configs").exists()`, compute `manifest.config_size = collect::dir_size(&backup_dir.join("configs"))?;`
   - Log both sizes at info level: `info!(rbac_size = manifest.rbac_size, config_size = manifest.config_size, "Computed RBAC and config sizes");`
5. Verify `cargo test` passes.

**Files:** `src/backup/collect.rs`, `src/backup/mod.rs`
**Acceptance:** F002

### Task 3: Wire rbac_size and config_size through to ListResponse

**TDD Steps:**
1. Update `summary_to_list_response()` in `src/server/routes.rs` (line ~326-327): change `rbac_size: 0` to `rbac_size: s.rbac_size` and `config_size: 0` to `config_size: s.config_size`.
2. Remove the TODO comments on those lines.
3. Write unit test `test_summary_to_list_response_sizes` that constructs a `BackupSummary` with `rbac_size: 1024` and `config_size: 512`, calls `summary_to_list_response()`, and asserts the `ListResponse` has the same values.
4. Verify `cargo test` passes.

**Files:** `src/server/routes.rs`
**Acceptance:** F003

### Task 4: Add offset/limit pagination to tables endpoint

**TDD Steps:**
1. Add `offset: Option<usize>` and `limit: Option<usize>` fields to `TablesParams` in `src/server/routes.rs`.
2. In the `tables()` handler (line ~1350), after building the filtered `results: Vec<TablesResponseEntry>`, apply pagination:
   ```rust
   let offset = params.offset.unwrap_or(0);
   let results: Vec<_> = results.into_iter().skip(offset).collect();
   let results = if let Some(limit) = params.limit {
       info!(offset = offset, limit = limit, total = results.len(), "tables: offset/limit applied");
       results.into_iter().take(limit).collect()
   } else {
       if offset > 0 {
           info!(offset = offset, "tables: offset applied");
       }
       results
   };
   ```
3. Write unit test `test_tables_pagination_params_deserialize` that verifies `TablesParams` can be deserialized from `"offset=5&limit=10"` query string (via serde_urlencoded).
4. Verify `cargo test` passes.
5. Update `X-Total-Count` header: add the total count before pagination as a response header for clients that need it.

**Files:** `src/server/routes.rs`
**Acceptance:** F004

### Task 5: Implement ManifestCache struct with TTL

**TDD Steps:**
1. Create `ManifestCache` struct in `src/list.rs` (near the top, after `BackupSummary`):
   ```rust
   use std::time::{Duration, Instant};

   /// In-memory cache for remote backup summaries (design 8.4).
   /// TTL-based expiry, invalidated on mutating operations.
   pub struct ManifestCache {
       summaries: Option<Vec<BackupSummary>>,
       populated_at: Option<Instant>,
       ttl: Duration,
   }
   ```
2. Implement methods:
   - `ManifestCache::new(ttl: Duration) -> Self`
   - `ManifestCache::get(&self) -> Option<&Vec<BackupSummary>>` -- returns `None` if expired or empty
   - `ManifestCache::set(&mut self, summaries: Vec<BackupSummary>)`
   - `ManifestCache::invalidate(&mut self)` -- clears cached data
3. Write unit test `test_manifest_cache_basic` that tests set/get/invalidate.
4. Write unit test `test_manifest_cache_ttl_expiry` that sets TTL to 0 and verifies get returns `None` after set (immediate expiry via `Duration::from_millis(0)` plus a tiny sleep).
5. Write `list_remote_cached(s3: &S3Client, cache: &tokio::sync::Mutex<ManifestCache>) -> Result<Vec<BackupSummary>>` function that checks cache first, then falls back to `list_remote(s3)` and populates cache.
6. Verify `cargo test` passes.

**Files:** `src/list.rs`
**Acceptance:** F005

### Task 6: Wire ManifestCache into AppState and server call sites

**TDD Steps:**
1. Add `manifest_cache: Arc<tokio::sync::Mutex<ManifestCache>>` field to `AppState` in `src/server/state.rs`.
2. Initialize in `AppState::new()`: read TTL from config (add `general.remote_cache_ttl_secs: u64` config field with default 300 per design 8.4), create `Arc::new(Mutex::new(ManifestCache::new(Duration::from_secs(ttl))))`.
3. In `routes::list_backups()` (server routes): replace `list::list_remote(&s3).await?` with `list::list_remote_cached(&s3, &state.manifest_cache).await?`.
4. In `routes::refresh_backup_counts()` (metrics scrape): use cached version.
5. Add cache invalidation calls after mutating operations:
   - After upload completes in `upload_backup` handler: `state.manifest_cache.lock().await.invalidate();`
   - After `delete_backup` remote: `state.manifest_cache.lock().await.invalidate();`
   - After `clean_remote_broken`: `state.manifest_cache.lock().await.invalidate();`
   - After retention_remote in watch loop: invalidate cache.
6. Log `info!("ManifestCache: populated, count={}", summaries.len())` on cache fill and `info!("ManifestCache: invalidated")` on invalidation.
7. Update `AppState` tests to include the new field.
8. Verify `cargo test` passes.

**Files:** `src/server/state.rs`, `src/server/routes.rs`, `src/config.rs`, `src/list.rs`
**Acceptance:** F006

### Task 7: Add SIGQUIT handler for stack dump

**TDD Steps:**
1. In `src/server/mod.rs`, inside the `if watch_enabled` block near the SIGHUP handler spawn (line ~211-224), add a SIGQUIT handler:
   ```rust
   #[cfg(unix)]
   {
       tokio::spawn(async move {
           use tokio::signal::unix::{signal, SignalKind};
           let mut sigquit = signal(SignalKind::quit())
               .expect("failed to register SIGQUIT handler");
           loop {
               sigquit.recv().await;
               info!("SIGQUIT received, dumping stack trace to stderr");
               let bt = std::backtrace::Backtrace::force_capture();
               eprintln!("=== SIGQUIT stack dump ===");
               eprintln!("{bt}");
               eprintln!("=== end stack dump ===");
           }
       });
   }
   ```
2. Also spawn the same SIGQUIT handler OUTSIDE the `if watch_enabled` block so it fires even without watch mode (move it to right before `let router = build_router(state.clone());`).
3. Add the same SIGQUIT handler in `src/main.rs` standalone watch mode (near line ~584-598, after the SIGHUP handler).
4. Write a documentation test or integration note: `kill -QUIT <pid>` should produce a stack dump to stderr and continue running.
5. Verify `cargo check` passes (no compilation errors).

**Files:** `src/server/mod.rs`, `src/main.rs`
**Acceptance:** F007

### Task 8: Implement compress_part_streaming() in upload/stream.rs

**TDD Steps:**
1. Add new function `compress_part_streaming()` in `src/upload/stream.rs`:
   ```rust
   use std::sync::mpsc;
   use std::io::Read;

   /// Minimum S3 multipart chunk size (5 MiB).
   const MIN_MULTIPART_CHUNK: usize = 5 * 1024 * 1024;

   /// Streaming compression: produces chunks suitable for multipart upload.
   ///
   /// Spawns a background thread that tars+compresses `part_dir` and sends
   /// fixed-size chunks (at least 5MB each for S3 multipart) via a channel.
   /// Returns a receiver that yields `Vec<u8>` chunks.
   ///
   /// This function runs synchronously (the receiver is consumed by async code
   /// via spawn_blocking or tokio::sync::mpsc bridging).
   pub fn compress_part_streaming(
       part_dir: &Path,
       archive_name: &str,
       data_format: &str,
       compression_level: u32,
       chunk_size: usize,
   ) -> Result<mpsc::Receiver<Result<Vec<u8>>>> {
       // ...
   }
   ```
2. Implementation: spawn a `std::thread` that creates a tar+compressor writing to a `ChunkedWriter` which buffers bytes and sends `Vec<u8>` chunks of `chunk_size` bytes through the channel when the buffer fills.
3. Write unit test `test_compress_part_streaming_roundtrip` that compresses a temp directory, collects all chunks, concatenates them, and decompresses to verify data integrity matches `compress_part()` output (both should decompress to identical tar archives).
4. Write unit test `test_compress_part_streaming_chunk_sizes` that verifies all chunks except the last are at least `MIN_MULTIPART_CHUNK` bytes.
5. Verify `cargo test` passes.

**Files:** `src/upload/stream.rs`
**Acceptance:** F008

### Task 9: Wire streaming path into upload pipeline

**TDD Steps:**
1. Add config field `backup.streaming_upload_threshold: u64` with default `256 * 1024 * 1024` (256 MiB) -- parts with uncompressed size above this threshold use the streaming path.
2. In `src/upload/mod.rs`, in the upload task (around line ~508), before `compress_part`:
   ```rust
   if item.part.size > streaming_threshold {
       info!(
           table = %item.table_key,
           part = %item.part.name,
           size = item.part.size,
           "Streaming multipart upload for large part"
       );
       // Use streaming path
       // ... (streaming compress + multipart upload)
   } else {
       // Existing buffered path
   }
   ```
3. The streaming path: call `compress_part_streaming()` in a `spawn_blocking`, bridge chunks to async, create multipart upload, upload each chunk as a part, complete multipart.
4. Write unit test `test_should_use_streaming` that verifies threshold logic.
5. Verify `cargo test` and `cargo check` pass.

**Files:** `src/upload/mod.rs`, `src/config.rs`
**Acceptance:** F009

### Task 10: Update CLAUDE.md for all modified modules (MANDATORY)

**Purpose:** Ensure all module documentation reflects code changes from this plan.

**Modules to update:** `src/server`, `src/upload`, `src/backup`

**TDD Steps:**

1. Read `context/affected-modules.json` for module list.
2. For each module (`src/server`, `src/upload`, `src/backup`), regenerate directory tree:
   ```bash
   tree -L 2 "$module" --noreport 2>/dev/null || ls -la "$module"
   ```
3. Detect and add new patterns:
   - `src/server/CLAUDE.md`: Add ManifestCache documentation, SIGQUIT handler pattern, tables pagination
   - `src/upload/CLAUDE.md`: Add streaming multipart upload documentation, `compress_part_streaming()` function
   - `src/backup/CLAUDE.md`: Document `pub fn dir_size()` and rbac_size/config_size computation pattern
4. Update root `CLAUDE.md`:
   - Add Phase 8 to "Current Implementation Status" section
   - Update "Remaining Limitations" to remove addressed items
   - Add new patterns to "Key Implementation Patterns"
5. Validate all CLAUDE.md files have required sections (Parent Context, Directory Structure, Key Patterns, Parent Rules).

**Files:** `src/server/CLAUDE.md`, `src/upload/CLAUDE.md`, `src/backup/CLAUDE.md`, `CLAUDE.md`
**Acceptance:** FDOC

## Notes

### Phase 4.5 Skip Justification
Phase 4.5 (Interface Skeleton Simulation) is **skipped** because:
- All new fields are added to existing structs (no new imports required)
- The only new function (`compress_part_streaming`) follows the exact same module pattern as existing `compress_part`
- `ManifestCache` is a self-contained struct with no external dependencies beyond `std::time`
- All types used (`u64`, `usize`, `Option<T>`, `Duration`, `Instant`, `Vec<u8>`, `BackupSummary`) are standard library or already imported
- The SIGQUIT handler uses `SignalKind::quit()` which is the same API as the verified `SignalKind::hangup()` pattern

### Consistency Validation Results

| Check | Status | Notes |
|-------|--------|-------|
| RC-008 | PASS | Task 1 adds manifest fields before Task 2 uses them; Task 5 creates ManifestCache before Task 6 wires it |
| RC-015 | PASS | All symbols used match knowledge_graph.json: BackupManifest, BackupSummary, ListResponse, TablesParams, AppState, dir_size, compress_part |
| RC-016 | PASS | Test names match implementation across tasks |
| RC-017 | PASS | Acceptance IDs F001-F009+FDOC match task references |
| RC-018 | PASS | Dependencies satisfied: Task 3 depends on Task 1 (fields exist), Task 6 depends on Task 5 (cache struct exists), Task 9 depends on Task 8 (streaming fn exists) |
| RC-019 | PASS | Signal handler pattern copied from existing SIGHUP (server/mod.rs:211-224), manifest field pattern from metadata_size, upload pattern from existing multipart code |
| RC-021 | PASS | All struct locations verified: BackupManifest in src/manifest.rs, BackupSummary in src/list.rs, ListResponse in src/server/routes.rs, TablesParams in src/server/routes.rs, AppState in src/server/state.rs, dir_size in src/backup/collect.rs |
| RC-035 | NOTED | cargo fmt must be run before each commit |
