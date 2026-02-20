# References and Symbol Analysis

## Phase 1: MCP-equivalent Analysis (via LSP + Grep)

### Item 1: API GET /api/v1/tables endpoint

**Current state:** `tables_stub()` at `src/server/routes.rs:1192` returns `(StatusCode::NOT_IMPLEMENTED, "not implemented (Phase 4f)")`.

**Route registration:** `src/server/mod.rs:88` -- `.route("/api/v1/tables", get(routes::tables_stub))`

**References found (3 total across 2 files):**
- `src/server/mod.rs:88` -- route registration
- `src/server/routes.rs:1192` -- definition
- `src/server/routes.rs:1587` -- test assertion

**CLI tables command already works** (`src/main.rs:384-481`):
- Live mode: calls `ch.list_tables()` or `ch.list_all_tables()`
- Remote mode: downloads manifest from S3 and lists tables
- Uses `TableFilter::new()` and `TableFilter::matches()` for filtering
- Output format: `{db}.{table}\t{engine}\t{size}`

**Key types for implementation:**
- `TableRow` struct at `src/clickhouse/client.rs:25-33`: `database`, `name`, `engine`, `data_compressed_bytes`, `data_uncompressed_bytes`, `total_bytes`, `total_rows`
- `ChClient::list_tables()` at `src/clickhouse/client.rs:288` -- `async fn(&self) -> Result<Vec<TableRow>>`
- `ChClient::list_all_tables()` at `src/clickhouse/client.rs:315` -- `async fn(&self) -> Result<Vec<TableRow>>`
- `TableFilter::new()` and `TableFilter::matches()` at `src/table_filter.rs`
- `list::format_size()` at `src/list.rs:887`
- `AppState` has `ch: ChClient` for live queries

**API response type needed:**
- New `TablesResponse` struct matching clickhouse-backup API format
- Fields: database, name, engine, data_path, data_compressed_bytes, data_uncompressed_bytes, total_bytes, total_rows
- Query params: `all` (bool), `tables` (pattern string)

### Item 2: API POST /api/v1/restart endpoint

**Current state:** `restart_stub()` at `src/server/routes.rs:1187` returns `(StatusCode::NOT_IMPLEMENTED, "not implemented")`.

**Route registration:** `src/server/mod.rs:87` -- `.route("/api/v1/restart", post(routes::restart_stub))`

**References found (3 total across 2 files):**
- `src/server/mod.rs:87` -- route registration
- `src/server/routes.rs:1187` -- definition
- `src/server/routes.rs:1584` -- test assertion

**Implementation approach:** Self-restart via `std::process::Command::new(std::env::current_exe())` with same args. Needs to coordinate with graceful shutdown.

### Item 3: --skip-projections CLI flag

**Current state:** Parsed but warns "not yet implemented" in two places:
- `src/main.rs:135-136` (create command)
- `src/main.rs:280-281` (create_remote command)

**CLI definition:** `src/cli.rs:51` -- `skip_projections: Option<String>` with `#[arg(long = "skip-projections")]`
Also at `src/cli.rs:200` for CreateRemote.

**Config equivalent:** `src/config.rs:370` -- `pub skip_projections: Vec<String>` in `BackupConfig`

**No projection filtering exists in backup module:** Grep for "projection" in `src/backup/` returns zero matches.

**Implementation needed:**
- In `collect_parts()` (`src/backup/collect.rs`), during shadow walk, detect projection directories (parts containing `.proj` suffix or nested under a projection directory)
- Filter based on glob patterns from `--skip-projections` or `config.backup.skip_projections`
- Projections in ClickHouse appear as subdirectories within a part: `{part_name}/{projection_name}.proj/`

### Item 4: --hardlink-exists-files CLI flag

**Current state:** Parsed but warns "not yet implemented" in two places:
- `src/main.rs:198-199` (download command)
- `src/server/routes.rs:480-481` (download API endpoint)

**CLI definition:** `src/cli.rs:103` -- `hardlink_exists_files: bool` with `#[arg(long = "hardlink-exists-files")]`

**API request:** `src/server/routes.rs:532` -- `DownloadRequest { hardlink_exists_files: Option<bool> }`

**Implementation approach:** After downloading a part, check if an identical part exists in another local backup directory; if so, hardlink instead of keeping the decompressed copy. Saves disk space when multiple local backups share parts.

### Item 5: Progress bar (disable_progress_bar config)

**Current state:** Config field exists but is never read outside config loading:
- `src/config.rs:47` -- `pub disable_progress_bar: bool` (default `false`)
- `src/config.rs:492` -- default value `false`
- `src/config.rs:933-936` -- CLI env overlay for `general.disable_progress_bar`

**No progress bar implementation exists.** No dependency on `indicatif` or similar crate.

**Implementation approach:** Add `indicatif` crate. Add progress bars to:
- Upload: per-part progress within upload pipeline
- Download: per-part progress within download pipeline
- Optionally: FREEZE phase (per-table), restore phase (per-table)
- When `disable_progress_bar` is true or stdout is not a TTY: disable all progress bars

### Item 6: Structured exit codes (design 11.6)

**Current state:** No `process::exit()` or `ExitCode` usage anywhere in the codebase. All error handling flows through `anyhow::Result<()>` from `main()`. When main returns `Err`, Rust prints the error and exits with code 1.

**Design spec (from docs/design.md lines 2422-2442):**
| Code | Meaning |
|---|---|
| 0 | Success |
| 1 | General error |
| 2 | Usage error (invalid flags, unknown command) |
| 3 | Backup not found |
| 4 | Lock conflict |
| 130 | SIGINT |
| 143 | SIGTERM |

**ChBackupError variants** (`src/error.rs:4-29`):
- `ClickHouseError(String)` -> exit 1
- `S3Error(String)` -> exit 1
- `ConfigError(String)` -> exit 1 or 2
- `LockError(String)` -> exit 4
- `BackupError(String)` -> exit 1 or 3
- `RestoreError(String)` -> exit 1
- `ManifestError(String)` -> exit 1 or 3
- `IoError(#[from] std::io::Error)` -> exit 1

**Implementation approach:**
- Change `main()` return type from `Result<()>` to `ExitCode` or use `std::process::exit()`
- Map error categories to exit codes
- Install signal handlers for SIGINT (130) and SIGTERM (143)
- Clap already exits with code 2 for usage errors by default

### Item 7: API list response sizes (metadata_size, rbac_size, config_size)

**Current state:** In `summary_to_list_response()` at `src/server/routes.rs:274-288`:
```rust
metadata_size: 0,    // TODO: expose from manifest metadata_size field
rbac_size: 0,        // Not implemented until Phase 4e
config_size: 0,      // Not implemented until Phase 4e
```

**`BackupManifest.metadata_size`** exists at `src/manifest.rs:48` -- `pub metadata_size: u64`
- Set in `src/backup/mod.rs:613-619` after manifest save (file size of metadata.json)
- Available in `BackupSummary` flow? NO -- `BackupSummary` at `src/list.rs:28-44` does NOT have a `metadata_size` field.

**Current `BackupSummary` fields:**
- `name`, `timestamp`, `size`, `compressed_size`, `table_count`, `is_broken`, `broken_reason`

**Changes needed:**
- Add `metadata_size: u64` to `BackupSummary`
- Populate from manifest in `parse_backup_summary()` (local) and `list_remote()` (remote)
- Wire through to `summary_to_list_response()`
- For `rbac_size`: compute from manifest.rbac presence (sum of access/ files)
- For `config_size`: not available in manifest currently; would need a new manifest field

## Phase 1.5: LSP Call Hierarchy Analysis

### `summary_to_list_response()` callers
- `src/server/routes.rs:248` -- inside `list_backups()` for local backups
- `src/server/routes.rs:259` -- inside `list_backups()` for remote backups
- Signature: `fn summary_to_list_response(s: list::BackupSummary, location: &str) -> ListResponse`

### `restart_stub()` callers
- `src/server/mod.rs:87` -- route handler registration
- `src/server/routes.rs:1584` -- test

### `tables_stub()` callers
- `src/server/mod.rs:88` -- route handler registration
- `src/server/routes.rs:1587` -- test

### `BackupManifest.metadata_size` references
- Defined: `src/manifest.rs:48`
- Set: `src/backup/mod.rs:619`
- Integration table DDL: `src/clickhouse/client.rs:1292`
- Tests: multiple test files (default value 0 or 256)

### Signal handling
- SIGINT: handled via `tokio::signal::ctrl_c()` in `src/main.rs:549` (watch), `src/server/mod.rs:261,291` (server)
- SIGHUP: handled in `src/main.rs:557-568` (watch), `src/server/mod.rs:191-204` (server)
- No SIGTERM handler currently installed anywhere
