# Plan: Phase 5 -- Polish Gaps

## Goal

Implement all seven remaining polish gaps identified after roadmap completion: API stubs (tables, restart), CLI flag implementations (skip-projections, hardlink-exists-files), progress bar, structured exit codes, and list response metadata sizes.

## Architecture Overview

Seven independent changes across multiple modules:
1. **API GET /api/v1/tables** -- Replace 501 stub with real endpoint querying ChClient
2. **API POST /api/v1/restart** -- Replace 501 stub with process re-exec
3. **--skip-projections** -- Filter `.proj` directories during shadow walk in collect.rs
4. **--hardlink-exists-files** -- Deduplicate downloaded parts via hardlinks to existing local backups
5. **Progress bar** -- Add indicatif-based progress tracking to upload/download pipelines
6. **Structured exit codes** -- Map error categories to design 11.6 codes (0/1/2/3/4/130/143)
7. **API list response sizes** -- Thread metadata_size through BackupSummary; expose rbac_size/config_size

## Architecture Assumptions (VALIDATED)

### Component Ownership
- **API endpoints** (items 1, 2): Defined in `src/server/routes.rs`, registered in `src/server/mod.rs:build_router()`, state from `AppState` in `src/server/state.rs`
- **Shadow walk** (item 3): `collect_parts()` in `src/backup/collect.rs` iterates part directories; projection filter must be injected there
- **Download dedup** (item 4): `download()` in `src/download/mod.rs` manages the work queue; dedup check added per-part before download
- **Progress bar** (item 5): New `ProgressTracker` struct; integrated into upload `spawn` tasks and download `spawn` tasks
- **Exit codes** (item 6): `main()` in `src/main.rs` currently returns `anyhow::Result<()>`; must be changed to catch errors and map to exit codes
- **List sizes** (item 7): `BackupSummary` in `src/list.rs` -> `summary_to_list_response()` in `src/server/routes.rs`

### What This Plan CANNOT Do
- Cannot add RBAC/config backup (Phase 4e already done; `rbac_size` and `config_size` values depend on data presence in manifest)
- Cannot implement streaming multipart download (progress bar wraps existing buffered approach)
- Cannot add tests that require real ClickHouse/S3 (items 1, 3, 4 need integration tests that are out of unit test scope)

## Risk Assessment

| Risk | Level | Mitigation |
|------|-------|------------|
| Item 5 (progress bar) adds `indicatif` dependency | GREEN | Well-maintained, widely used crate; no feature conflicts |
| Item 6 (exit codes) changes `main()` return type | GREEN | Straightforward refactor; clap already handles code 2 |
| Item 3 (skip-projections) modifies hot path in collect.rs | YELLOW | Must not regress performance for non-projection backups |
| Item 4 (hardlink-exists-files) scans local backups | YELLOW | Need to handle missing/corrupt backups gracefully |
| Item 2 (restart) refactors AppState to ArcSwap | YELLOW | Mechanical refactor touching all handlers; must verify all tests pass after |

## Expected Runtime Logs

| Pattern | Required | Description |
|---------|----------|-------------|
| `tables endpoint returning` | yes | Tables endpoint serves real data |
| `Restart requested` | yes | Restart endpoint triggers re-exec |
| `Skipping projection directory` | yes | skip-projections filter active |
| `Hardlink dedup` | yes | hardlink-exists-files found existing part |
| `Progress:` | yes | Progress tracker reports upload/download status |
| `Exiting with code` | yes | Structured exit code applied |
| `metadata_size=` | yes | metadata_size threaded through list |
| `ERROR:` | no (forbidden) | Should NOT appear during normal operation |

## Known Related Issues (Out of Scope)

| Issue | Reason | Future Plan |
|-------|--------|-------------|
| Streaming multipart upload | Orthogonal to progress bar; item 5 wraps existing approach | Phase 6 |
| Full RBAC size computation | Requires scanning RBAC file sizes; partial (manifest has field) | N/A -- addressed by item 7 |
| Partition-level restore | CLI flag parsed but not implemented | Separate plan |
| skip-empty-tables restore flag | CLI flag parsed but not implemented | Separate plan |

## Dependency Groups

```
Group A (Independent, no cross-dependencies):
  - Task 3: --skip-projections implementation
  - Task 4: --hardlink-exists-files implementation
  - Task 6: Structured exit codes
  - Task 7: API list response metadata_size/rbac_size/config_size

Group B (ArcSwap refactor → API endpoints):
  - Task 2: API POST /api/v1/restart (adds arc-swap dep, refactors AppState — MUST run before Task 1)
  - Task 1: API GET /api/v1/tables endpoint (depends on Task 2 since Task 2 refactors AppState/routes)

Group C (Progress bar):
  - Task 5a: Add indicatif dependency and ProgressTracker struct
  - Task 5b: Wire progress bar into upload and download pipelines (depends on 5a)

Group D (Final, depends on all above):
  - Task 8: Update CLAUDE.md for all modified modules (MANDATORY)
```

## Tasks

### Task 1: API GET /api/v1/tables endpoint

**Description:** Replace `tables_stub()` (returns 501) with a real `tables()` handler that supports two modes: (a) live ClickHouse table listing via `ChClient::list_tables()` / `ChClient::list_all_tables()`, and (b) remote backup table listing via manifest download from S3. Mirrors the CLI `tables` command logic in `main.rs:384-480`.

**TDD Steps:**
1. Write failing test: `test_tables_endpoint_returns_data` -- calls `tables()` handler (currently 501), asserts 200 status and JSON body with table entries
2. Define `TablesParams` struct with query params: `table: Option<String>`, `all: Option<bool>`, `backup: Option<String>` (remote backup name)
3. Define `TablesResponseEntry` struct matching actual `TableRow` fields: `database: String`, `name: String`, `engine: String`, `uuid: String`, `data_paths: Vec<String>`, `total_bytes: Option<u64>` — no fields that don't exist in `TableRow`
4. Implement `tables()` handler:
   - Extract `State(state)` and `Query(params)`
   - **If `params.backup` is Some** (remote mode):
     - Download manifest from S3 via `state.s3.get_object(&format!("{}/metadata.json", backup_name))`
     - Parse `BackupManifest::from_json_bytes()`
     - Iterate `manifest.tables`, apply `TableFilter` if `params.table` is Some
     - Map each `(full_name, TableManifest)` to `TablesResponseEntry` (engine from manifest, total_bytes from sum of part sizes)
   - **Else** (live mode):
     - If `params.all` is true, call `state.ch.list_all_tables()`; else call `state.ch.list_tables()`
     - Apply `TableFilter` if `params.table` is Some (use `matches_including_system` when all=true)
     - Map `Vec<TableRow>` to `Vec<TablesResponseEntry>`
   - Return `Json(results)`
5. Update route in `build_router()`: change `.route("/api/v1/tables", get(routes::tables_stub))` to `.route("/api/v1/tables", get(routes::tables))`
6. Remove `tables_stub()` function
7. Remove test `test_remaining_stub_endpoints_return_501` (both stubs replaced by Tasks 1 and 2)
8. Write test: `test_tables_endpoint_remote_mode` -- verify backup query param triggers manifest-based listing
9. Verify all tests pass

**Files:**
- `src/server/routes.rs` -- Add TablesParams, TablesResponseEntry, tables() handler (both live and remote modes); remove tables_stub()
- `src/server/mod.rs` -- Update route registration

**Acceptance:** F001

---

### Task 2: API POST /api/v1/restart endpoint

**Description:** Replace `restart_stub()` (returns 501) with a `restart()` handler. Per design doc §9: "close connections, re-bind socket, reload config."

**Architecture constraint:** `AppState.ch` (`ChClient`) and `AppState.s3` (`S3Client`) are plain `Clone` fields — NOT behind `Arc<RwLock>` or `ArcSwap`. They cannot be hot-swapped in the shared `AppState` without a structural refactor that would affect all handlers.

**Implementation approach:** Use `ArcSwap` for the config, CH client, and S3 client fields in `AppState` to enable atomic replacement. This is the minimal change that enables true restart semantics:
1. Add `arc-swap = "1"` dependency to Cargo.toml
2. Change `AppState.config` from `Arc<Config>` to `ArcSwap<Config>`
3. Change `AppState.ch` from `ChClient` to `ArcSwap<ChClient>`
4. Change `AppState.s3` from `S3Client` to `ArcSwap<S3Client>`
5. Update all handlers that read these fields to use `.load()` (returns `arc_swap::Guard<Arc<T>>` which derefs to `T`)
6. The restart handler atomically swaps new clients in via `.store()`

**TDD Steps:**
1. Add `arc-swap = "1"` to Cargo.toml
2. Write failing test: `test_restart_endpoint_returns_200` -- currently returns 501
3. Refactor `AppState` fields: `config: ArcSwap<Config>`, `ch: ArcSwap<ChClient>`, `s3: ArcSwap<S3Client>`
4. Update all handler call sites (list_backups, create_backup, upload, download, restore, etc.) to use `state.ch.load()`, `state.s3.load()`, `state.config.load()` — mechanical find-and-replace
5. Implement `restart()` handler:
   - Load config from `state.config_path` via `Config::load()` + `validate()`
   - Create new `ChClient::new(&config.clickhouse)?`
   - Create new `S3Client::new(&config.s3).await?`
   - Ping ClickHouse to verify: `ch.ping().await?`
   - Atomically swap: `state.config.store(Arc::new(config))`, `state.ch.store(Arc::new(ch))`, `state.s3.store(Arc::new(s3))`
   - Return `Json(RestartResponse { status: "restarted" })`
   - On error: return 500 with error message (old clients remain active)
6. Define `RestartResponse { status: String }`
7. Update route: change `restart_stub` to `restart`
8. Remove `restart_stub()` function
9. Remove `test_remaining_stub_endpoints_return_501` test (both stubs replaced)
10. Verify all existing tests still pass after ArcSwap refactor
11. Verify restart test passes

**Files:**
- `Cargo.toml` -- Add arc-swap dependency
- `src/server/state.rs` -- Refactor AppState fields to use ArcSwap
- `src/server/routes.rs` -- Add RestartResponse, restart() handler; remove restart_stub(); update all handlers to use `.load()`
- `src/server/mod.rs` -- Update route registration; update AppState construction to use ArcSwap

**Acceptance:** F002

---

### Task 3: --skip-projections implementation

**Description:** Implement projection directory filtering during shadow walk in `collect_parts()`. Projections are `.proj` subdirectories inside part directories. When `--skip-projections` is provided (either CLI flag or `config.backup.skip_projections` list), matching `.proj` directories are excluded from hardlinking.

Per design doc §3.4: "Projections are pre-computed materialized indexes stored as `.proj/` subdirectories inside parts. Pattern format: `db.table:proj_name` with glob support. During shadow walk, skip directories matching `*.proj` patterns."

**Pattern matching semantics:**
- CLI flag `--skip-projections=pattern` accepts a comma-separated list of patterns
- Config `backup.skip_projections: [...]` is a YAML list of patterns
- Pattern format: `proj_name` (plain name or glob, applied to the `.proj` directory name)
- A `.proj` directory name is like `my_projection.proj` — the stem before `.proj` is the projection name
- Patterns are matched against the stem: e.g., pattern `my_*` matches `my_projection.proj`
- Special value `*` (or empty list with flag present) skips ALL projections
- Context: `hardlink_dir()` (collect.rs:398) walks source dirs recursively via `WalkDir`. Projection dirs are subdirectories of part directories (e.g., `{part_name}/my_projection.proj/`).

**TDD Steps:**
1. Write failing test: `test_hardlink_dir_skips_projections` -- create a temp dir with part files and a `my_proj.proj` subdirectory containing files, call `hardlink_dir()` with `skip_proj_patterns: &["*"]`, assert `.proj` dir is NOT present in destination
2. Modify `hardlink_dir()` signature: `fn hardlink_dir(src_dir: &Path, dst_dir: &Path, skip_proj_patterns: &[String]) -> Result<()>`; in the WalkDir loop, when encountering a directory entry whose name ends with `.proj` and `skip_proj_patterns` is non-empty: extract the stem (name without `.proj`), check if any pattern in `skip_proj_patterns` matches the stem via `glob::Pattern`, and if so call `it.skip_current_dir()` (WalkDir's `filter_entry` or manual skip) and log at info level
3. Update all call sites of `hardlink_dir()` to pass the new parameter (currently called within `collect_parts` for local disk parts)
4. Update `collect_parts()` signature to accept `skip_projections: &[String]`, pass through to `hardlink_dir()` calls
5. Update `backup::create()` signature to accept `skip_projections: &[String]`, pass through to `collect_parts()`
6. Update `main.rs` create command: remove the "not yet implemented" warning, merge CLI flag with `config.backup.skip_projections`, pass to `backup::create()`
7. Update `main.rs` create_remote command: same treatment
8. Wire through server routes: `create_backup` and `create_remote` handlers pass `config.backup.skip_projections` to `backup::create()`
9. Write test: `test_skip_projections_empty_list_keeps_all` -- with empty skip list, `.proj` dirs are preserved
10. Write test: `test_skip_projections_glob_pattern` -- pattern `my_*` skips `my_agg.proj` but keeps `other.proj`
11. Verify all tests pass

**Files:**
- `src/backup/collect.rs` -- Modify `hardlink_dir()` to accept and apply projection filter; update `collect_parts()` signature
- `src/backup/mod.rs` -- Update `create()` signature to accept and pass through `skip_projections`
- `src/main.rs` -- Wire CLI flag / config into `backup::create()` for both create and create_remote; remove warnings
- `src/server/routes.rs` -- Wire skip_projections through create_backup and create_remote API handlers

**Acceptance:** F003

---

### Task 4: --hardlink-exists-files implementation

**Description:** After downloading a part from S3, check if an identical part (matching CRC64) already exists in another local backup. If so, hardlink instead of keeping the decompressed copy. Saves disk space when multiple local backups share unchanged parts.

Per design doc 11.4: "Checksum dedup optimization (--hardlink-exists-files): if local backup has part with same name AND matching CRC64: hardlink to existing part -> skip download -> release permit"

**TDD Steps:**
1. Write failing test: `test_hardlink_dedup_finds_existing_part` -- create two backup dirs, one with a part and checksums.txt, call dedup function with matching CRC64, assert hardlink created
2. Implement `find_existing_part(data_path: &str, backup_name: &str, table_key: &str, part_name: &str, expected_crc: u64) -> Option<PathBuf>`:
   - Scan `{data_path}/backup/*/shadow/{table_key}/{part_name}/` (excluding current backup)
   - For each candidate: compute CRC64 of `checksums.txt`, compare with expected
   - Return first matching path
3. Implement `hardlink_existing_part(existing: &Path, target: &Path) -> Result<()>`:
   - Uses existing `hardlink_dir()` logic (hardlink with EXDEV fallback)
4. Modify download work loop: when `hardlink_exists_files` is true, before downloading each local part:
   - Call `find_existing_part()`
   - If found: `hardlink_existing_part()` and skip download
   - Log: `info!("Hardlink dedup: reusing existing part")`
5. Update `download()` signature to accept `hardlink_exists_files: bool`
6. Wire through from `main.rs` (remove "not yet implemented" warning) and `routes.rs` download handler
7. Write test: `test_hardlink_dedup_no_match_downloads` -- no matching part exists, normal download proceeds
8. Verify all tests pass

**Files:**
- `src/download/mod.rs` -- Add `find_existing_part()`, `hardlink_existing_part()`, modify download loop; update `download()` signature
- `src/main.rs` -- Wire flag into `download()`, remove warning
- `src/server/routes.rs` -- Wire flag into download handler, remove warning

**Acceptance:** F004

---

### Task 5a: Add indicatif dependency and ProgressTracker struct

**Description:** Add `indicatif` crate to Cargo.toml and create a `ProgressTracker` struct that wraps progress bar lifecycle. Per design doc 11.4, the progress bar shows: operation, percentage, part count, throughput, and ETA.

**TDD Steps:**
1. Add `indicatif = "0.17"` to Cargo.toml dependencies
2. Create a `ProgressTracker` in a new module (or in existing appropriate location like a new `src/progress.rs`):
   ```rust
   pub struct ProgressTracker {
       bar: Option<indicatif::ProgressBar>,
       total_parts: u64,
   }
   ```
3. Implement methods:
   - `new(operation: &str, total_parts: u64, disable: bool) -> Self` -- creates bar with template `"{operation} {bar:40} {percent}% {pos}/{len} parts {bytes_per_sec} ETA {eta}"`, or None if disabled or not a TTY
   - `inc(&self, bytes: u64)` -- increment position and add bytes
   - `finish(&self)` -- call `bar.finish_with_message("done")`
4. Add `mod progress;` to `src/lib.rs`
5. Write unit test: `test_progress_tracker_disabled` -- when disable=true, operations are no-ops
6. Write unit test: `test_progress_tracker_counts` -- verify position tracking
7. Verify cargo check passes

**Files:**
- `Cargo.toml` -- Add indicatif dependency
- `src/progress.rs` -- New file with ProgressTracker
- `src/lib.rs` -- Add `pub mod progress;`

**Acceptance:** F005a

---

### Task 5b: Wire progress bar into upload and download pipelines

**Description:** Integrate `ProgressTracker` into the upload and download parallel pipelines. Progress bar shown when: `!config.general.disable_progress_bar && atty::is(Stream::Stdout)`. In server mode, always disabled (no TTY).

**TDD Steps:**
1. In `src/upload/mod.rs`: create `ProgressTracker` before spawning upload tasks, pass `Arc<ProgressTracker>` (or make it Clone) to each spawned task, call `tracker.inc(compressed_size)` after each successful part upload
2. In `src/download/mod.rs`: same pattern for download tasks
3. Determine TTY detection: use `std::io::IsTerminal` trait (stable since Rust 1.70): `std::io::stdout().is_terminal()`
4. Construct ProgressTracker with `disable = config.general.disable_progress_bar || !stdout().is_terminal()`
5. Verify: run with `TERM=dumb` to confirm progress bar is hidden
6. Verify: run normally to confirm progress bar appears (manual/integration test)

**Files:**
- `src/upload/mod.rs` -- Create and use ProgressTracker in upload pipeline
- `src/download/mod.rs` -- Create and use ProgressTracker in download pipeline
- `src/progress.rs` -- May need to add `Clone` or `Arc` wrapping

**Acceptance:** F005b

---

### Task 6: Structured exit codes

**Description:** Map error categories to design doc 11.6 exit codes. Currently `main()` returns `anyhow::Result<()>` which gives 0 on success and 1 on any error. Must differentiate: 2 (usage), 3 (backup not found), 4 (lock conflict), 130 (SIGINT), 143 (SIGTERM).

**TDD Steps:**
1. Write test: `test_exit_code_from_error_lock` -- verify `LockError` maps to exit code 4
2. Write test: `test_exit_code_from_error_backup_not_found` -- verify `BackupError` with "not found" maps to 3
3. Write test: `test_exit_code_from_error_general` -- verify other errors map to 1
4. Create `fn exit_code_from_error(err: &anyhow::Error) -> i32` in `src/main.rs` (or `src/error.rs`):
   - Downcast to `ChBackupError`; match variant:
     - `LockError(_)` -> 4
     - `BackupError(msg)` if msg contains "not found" -> 3
     - `ManifestError(msg)` if msg contains "not found" -> 3
     - All others -> 1
   - If cannot downcast (not a ChBackupError), return 1
5. Add signal handlers for SIGINT (130) and SIGTERM (143):
   - SIGINT: clap already exits 2 for usage errors; for Ctrl+C during operation, tokio::signal::ctrl_c -> exit(130)
   - SIGTERM: register via `tokio::signal::unix::signal(SignalKind::terminate())`
6. Refactor `main()`: catch the Result, on Err: log error, call `exit_code_from_error()`, call `std::process::exit(code)`
7. Note: clap already returns exit code 2 for invalid arguments/unknown flags via its built-in error handling (before `main()` runs)
8. Verify all tests pass

**Files:**
- `src/main.rs` -- Refactor main(), add `exit_code_from_error()`, add signal handlers
- `src/error.rs` -- Potentially add helper method `ChBackupError::exit_code(&self) -> i32`

**Acceptance:** F006

---

### Task 7: API list response metadata_size, rbac_size, config_size

**Description:** Thread `metadata_size` from `BackupManifest` through `BackupSummary` to `summary_to_list_response()`. For `rbac_size`: compute from manifest's rbac data presence. For `config_size`: hardcode 0 with TODO (no manifest field for config size).

**TDD Steps:**
1. Write failing test: `test_backup_summary_has_metadata_size` -- create BackupSummary, assert metadata_size field exists
2. Add `metadata_size: u64` field to `BackupSummary` struct (src/list.rs:28)
3. Populate in `parse_backup_summary()` from `manifest.metadata_size`
4. Populate in `list_remote()` from downloaded manifest's `metadata_size`
5. Populate in `list_local()` broken paths: default to 0
6. Update `summary_to_list_response()` in routes.rs: use `s.metadata_size` instead of hardcoded 0
7. For `rbac_size`: if `manifest.rbac.is_some()`, read size of `access/` directory in local backups or estimate from manifest; for now use the metadata.json file as rough estimate. Simple approach: hardcode 0 with improved TODO noting "requires scanning access/ directory sizes"
8. For `config_size`: keep 0 with improved TODO "requires adding config_size to BackupManifest"
9. Write test: `test_summary_to_list_response_metadata_size` -- verify metadata_size flows through
10. Verify all tests pass

**Files:**
- `src/list.rs` -- Add `metadata_size` to `BackupSummary`, populate in `parse_backup_summary()` and `list_remote()`
- `src/server/routes.rs` -- Update `summary_to_list_response()` to use `s.metadata_size`

**Acceptance:** F007

---

### Task 8: Update CLAUDE.md for all modified modules (MANDATORY)

**Purpose:** Ensure all module documentation reflects code changes from this plan.

**Modules to update:** src/server, src/backup, src/download

**TDD Steps:**

1. **Read affected-modules.json for module list:**
   - src/server: Items 1 (tables endpoint), 2 (restart endpoint), 7 (metadata_size)
   - src/backup: Item 3 (skip-projections filter in collect.rs)
   - src/download: Item 4 (hardlink-exists-files dedup)

2. **For each module, regenerate directory tree** and update Directory Structure section

3. **Detect and add new patterns:**
   - src/server: Document tables() and restart() endpoints replacing stubs; remove "Stub Endpoints" section or update it
   - src/backup: Document projection filtering pattern in collect.rs
   - src/download: Document hardlink dedup pattern

4. **Update root CLAUDE.md:**
   - Update "Current Implementation Status" section for Phase 5
   - Update "Remaining Limitations" -- remove items completed by this plan
   - Add progress bar to tech stack if indicatif added

5. **Validate all CLAUDE.md files** have required sections

**Files:**
- `src/server/CLAUDE.md`
- `src/backup/CLAUDE.md`
- `src/download/CLAUDE.md`
- `CLAUDE.md` (root)

**Acceptance:** FDOC

---

## Notes

### Phase 4.5 (Interface Skeleton Simulation) -- SKIP JUSTIFICATION

Skipped because: All changes are within existing functions or extend existing structs with additional fields. No new module-level imports are needed beyond `indicatif` (a new external crate, not internal). All internal types and paths are verified in `context/knowledge_graph.json`. The plan introduces no new cross-module type dependencies that could fail at compile time unexpectedly.

### Trading Logic Checklist -- N/A

No order placement or position changes in this plan.

### Consistency Validation Results

| Check | Status | Notes |
|-------|--------|-------|
| RC-015 | PASS | No cross-task data flows (all items independent except 5a->5b) |
| RC-016 | PASS | `TablesResponseEntry` uses fields from verified `TableRow`; `BackupSummary` extended consistently |
| RC-017 | PASS | No self.X references; all struct fields verified in symbols.md |
| RC-018 | PASS | Every task has task ordering dependency: 5b depends on 5a only |
| RC-006 | PASS | All APIs verified: list_tables, list_all_tables, hardlink_dir, format_size, etc. |
| RC-008 | PASS | Task 5b uses ProgressTracker from Task 5a (preceding task) |
| RC-019 | PASS | API endpoint pattern matches existing handlers (list_backups, watch_status) |
| RC-021 | PASS | All file locations verified via grep in symbols.md |

### Redundancy Consistency Check

For REPLACE decisions (tables_stub -> tables, restart_stub -> restart):
- [x] Plan includes removal of old functions (Tasks 1, 2)
- [x] Plan includes test migration: `test_remaining_stub_endpoints_return_501` removed
- [x] Acceptance criteria check old stubs are absent (F001 structural, F002 structural)
- [x] Removal is in same task as creation
