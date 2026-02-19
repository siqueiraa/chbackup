# CLAUDE.md

## Project Context

**chbackup** — Drop-in Rust replacement for Altinity/clickhouse-backup. Single static binary, S3-only storage, non-destructive restore.

## Authoritative Documents

**ALWAYS read these before planning or implementing:**

| Document | Path | Purpose |
|----------|------|---------|
| Design | `docs/design.md` | Full technical spec (~1800 lines, 17 sections). Covers deployment, backup/restore flows, S3 layout, manifest format, config, CLI commands, error handling. |
| Roadmap | `docs/roadmap.md` | Implementation phases (0-4). Each phase produces a working binary. Maps design sections to deliverables. |

**Rules:**
- Before creating any plan, read both documents for the relevant sections
- Design doc sections are numbered (e.g., §3.1 mutations, §5.2 restore modes) — reference them in plans
- Roadmap phases are sequential — check which phase we're on before starting work

## Key Architecture Decisions (from design)

- **Must run on same host as ClickHouse** — FREEZE creates hardlinks requiring local filesystem access
- **Streaming by default** — upload/download by individual data part, no full-archive mode
- **S3-only storage** — no GCS, Azure, or local-only backup mode
- **ClickHouse 21.8+** required (ALTER TABLE FREEZE WITH NAME)
- **Static musl binary** — zero runtime dependencies, ~15MB

## Tech Stack (from §11.3)

| Component | Crate |
|-----------|-------|
| CLI | `clap` (derive API) |
| Config | `serde_yaml` + env overlay |
| ClickHouse | `clickhouse` v0.13 (HTTP protocol, async) |
| S3 | `aws-sdk-s3` + `aws-config` |
| Async | `tokio` |
| Async streams | `tokio-util` (codec feature) |
| Directory walks | `walkdir` (via `spawn_blocking`) |
| Errors | `thiserror` + `anyhow` |
| Logging | `tracing` + `tracing-subscriber` |
| Compression | `lz4_flex` (sync, via `spawn_blocking`) |
| Archiving | `tar` (v0.4, sync, via `spawn_blocking`) |
| CRC64 | `crc` (v3, CRC_64_XZ algorithm) |
| Concurrency utils | `futures` (v0.3, for `try_join_all`) |
| Glob matching | `glob` (v0.3, for table filter `-t` flag) |
| Unix permissions | `nix` (v0.29, chown for restore) |
| Progress bar | `indicatif` (v0.17, TTY-aware, disabled in server mode) |
| Hot-swap state | `arc-swap` (v1, atomic pointer swap for server restart) |

## Design Doc Gotchas

Things that are easy to get wrong when reading the design doc:

- **Config param count**: Design says "~40 params" but actual count from §12 YAML block is **~106 params** across 7 sections (general:14, clickhouse:37, s3:20, backup:13, retention:2, watch:7, api:13)
- **CreateRemote != Create**: `create_remote` has a DIFFERENT flag set than `create` — no `--diff-from`, `--partitions`, `--schema`. Uses `--diff-from-remote` instead. Always check the §2 flag reference table.
- **RestoreRemote != Restore**: `restore_remote` has no `--partitions`, `--schema`, `--data-only`. But DOES have `--as` (per flag table).
- **Logging mode**: JSON mode is triggered by `server` command OR `general.log_format: json` config. Not just server mode.

## Current Implementation Status

**Phase 0** (skeleton): Complete -- CLI, config, ChClient, S3Client, PidLock, logging, error types.
**Phase 1** (MVP): Complete -- Single-table backup and restore pipeline (sequential, no parallelism).
**Phase 2a** (parallelism): Complete -- Parallel operations for all four command pipelines (create, upload, download, restore), multipart S3 upload for large parts, byte-level rate limiting.
**Phase 2b** (incremental): Complete -- Incremental backups via --diff-from/--diff-from-remote, diff_parts() comparison, create_remote command.
**Phase 2c** (S3 object disk): Complete -- S3 disk metadata parser (5 format versions), disk-aware shadow walk, CopyObject with retry+streaming fallback, mixed disk upload/download, UUID-isolated S3 restore with same-name optimization.
**Phase 2d** (resume & reliability): Complete -- Resumable upload/download/restore via state files, atomic manifest upload (.tmp+CopyObject+delete), post-download CRC64 verification with retry, disk space pre-flight check, partition-level FREEZE, disk filtering (skip_disks/skip_disk_types), parts column consistency check, broken backup cleanup (clean_broken), ClickHouse TLS support.
**Phase 3d** (watch mode): Complete -- Watch state machine loop with full+incremental backup chains, name template resolution ({type}/{time:FORMAT}/{shard} macros), resume-on-restart from remote backup listing, SIGHUP config hot-reload, API endpoints (watch/start, watch/stop, watch/status, reload), server --watch flag, Prometheus watch metrics.
**Phase 3e** (docker/deploy): Complete -- Production Dockerfile, GitHub Actions CI with ClickHouse version matrix, K8s sidecar example, integration test seed data and round-trip smoke tests, WATCH_INTERVAL/FULL_INTERVAL env var overlay.
**Phase 5** (polish gaps): Complete -- API tables/restart endpoints (replacing 501 stubs), --skip-projections flag, --hardlink-exists-files download dedup, progress bar (indicatif), structured exit codes (0/1/2/3/4/130/143), metadata_size in list API response.

## Source Module Map

```
src/
  main.rs            -- CLI entry point, command dispatch
  lib.rs             -- Module declarations
  cli.rs             -- clap derive CLI definitions
  concurrency.rs     -- Effective concurrency accessors (upload, download, max_connections, object_disk_copy)
  config.rs          -- Config loader (~106 params, env overlay)
  error.rs           -- ChBackupError (thiserror), exit_code_from_error() for structured exit codes
  lock.rs            -- PidLock (three-tier scope)
  logging.rs         -- tracing init (text/JSON)
  manifest.rs        -- BackupManifest, TableManifest, PartInfo, S3ObjectInfo, DatabaseInfo (serde JSON)
  object_disk.rs     -- ClickHouse S3 object disk metadata parser (5 format versions)
  rate_limiter.rs    -- Token-bucket rate limiter (shared via Arc, 0 = unlimited)
  resume.rs          -- Resume state types (UploadState, DownloadState, RestoreState), atomic save/load, graceful degradation
  table_filter.rs    -- Glob pattern matching for -t flag, disk exclusion checks
  progress.rs        -- ProgressTracker (indicatif wrapper, TTY-aware, Clone for spawned tasks)
  list.rs            -- list + delete + clean_broken + retention + GC + clean_shadow (local dir scan + S3)
  backup/            -- create command: parallel FREEZE, disk-aware shadow walk, hardlink/S3 metadata, CRC64, UNFREEZE
  upload/            -- upload command: parallel tar+LZ4 compress, S3 PutObject/multipart, CopyObject for S3 disk parts
  download/          -- download command: parallel S3 GetObject, LZ4+untar decompress, metadata-only for S3 disk parts
  restore/           -- restore command: Mode A (--rm DROP) + Mode B (non-destructive), ON CLUSTER, ZK conflict resolution, ATTACH TABLE mode, mutation re-apply, parallel hardlink+ATTACH PART, UUID-isolated S3 CopyObject restore
  clickhouse/        -- ChClient wrapper (FREEZE/UNFREEZE, DDL, DROP, DETACH/ATTACH TABLE, ZK queries, mutation exec, get_macros)
  storage/           -- S3Client wrapper (put, get, list, delete, head, multipart, copy_object)
  watch/             -- Watch mode scheduler: state machine loop, name template resolution, resume state, config hot-reload
  server/            -- HTTP API server: axum routes, AppState, metrics, auth, watch lifecycle endpoints
```

Each directory module has its own `CLAUDE.md` with detailed API and pattern documentation.

## Data Flow

```
create:   Config -> ChClient -> [parts_columns check] -> FREEZE (whole-table or per-partition) -> walk shadow (all disks, with disk filtering) -> hardlink (local) / parse metadata (S3 disk) -> CRC64 -> UNFREEZE -> BackupManifest -> JSON
upload:   BackupManifest(JSON) -> [load resume state] -> local parts: tar+lz4 compress -> S3Client.put_object | S3 disk parts: S3Client.copy_object -> [save state per-part] -> manifest atomic upload (.tmp+copy+delete) -> [delete state]
download: S3Client.get_object(manifest) -> [disk space pre-flight] -> BackupManifest -> [load resume state] -> local parts: S3Client.get_object -> lz4+untar -> [CRC64 verify] | S3 disk parts: metadata only -> [save state per-part] -> [delete state]
restore:  BackupManifest(JSON) -> [load resume state + query system.parts] -> [Phase 0: DROP tables/dbs if --rm] -> [detect DatabaseReplicated] -> CREATE DB/TABLE (with ON CLUSTER, ZK conflict resolution, Distributed cluster rewrite) -> [ATTACH TABLE mode for Replicated] | local parts: hardlink to detached/ | S3 disk parts: CopyObject to UUID paths + rewrite metadata -> ATTACH PART -> [mutation re-apply] -> [save state per-part] -> chown -> [delete state]
list:     scan local dirs + S3Client.list -> display (with broken backup detection)
delete:   rm local dir or S3Client.delete_objects
clean_broken: list -> filter is_broken -> delete each
retention_local: list_local -> filter out broken -> sort by timestamp asc -> delete oldest exceeding keep count
retention_remote: list_remote -> filter out broken -> sort by timestamp asc -> for each to delete: gc_collect_referenced_keys (load all surviving manifests) -> gc_delete_backup (delete unreferenced keys, manifest last)
clean_shadow: ChClient.get_disks -> filter out backup-type disks -> for each disk: scan shadow/ for chbackup_* dirs -> remove_dir_all
watch:    list_remote -> resume_state(filter by template prefix) -> [SleepThen|FullNow|IncrNow] -> resolve_name_template -> backup::create -> upload::upload -> [delete_local] -> [retention_local + retention_remote] -> sleep(watch_interval) -> loop
```

## Key Implementation Patterns

- **Client wrappers**: `ChClient` and `S3Client` wrap third-party crates with config-driven setup
- **BackupManifest**: Central data structure flowing between all commands; serialized as `metadata.json`
- **FreezeGuard**: Tracks frozen tables for cleanup; explicit `unfreeze_all()` (not Drop-based)
- **SortPartsByMinBlock**: Parse part name from right (partition can contain underscores)
- **Hardlink with EXDEV fallback**: `std::fs::hard_link` with copy fallback on cross-device (error 18)
- **Buffered upload/download**: Buffers in memory; single PutObject for parts <= 32 MiB, multipart for larger
- **spawn_blocking for sync I/O**: walkdir, tar, lz4 compression/decompression run via `spawn_blocking`
- **Flat semaphore concurrency**: Each command pipeline uses a single `Arc<Semaphore>` shared across all spawned tasks; effective concurrency resolved from config via `concurrency.rs`
- **Rate limiting**: Token-bucket `RateLimiter` (shared via `Clone`/`Arc`) gates bytes per second for uploads and downloads; 0 = unlimited
- **OwnedAttachParams**: Owned variant of `AttachParams` for crossing `tokio::spawn` boundaries (no lifetime constraints); extended with S3 fields for Phase 2c
- **Object disk metadata parsing**: `object_disk.rs` parses all 5 ClickHouse metadata format versions (v1-v5) to extract S3 object references from frozen shadow files
- **Disk-aware routing**: Each command pipeline checks `disk_type_map` to route parts: local disks use hardlink+compress, S3 disks ("s3"/"object_storage") use CopyObject
- **CopyObject with retry+fallback**: `S3Client.copy_object_with_retry()` retries 3x with exponential backoff; conditional streaming fallback gated by `allow_object_disk_streaming` config
- **UUID-isolated S3 restore**: Copies S3 objects to `store/{3char}/{uuid_with_dashes}/` paths derived from destination table UUID; same-name optimization skips objects that already exist with matching size
- **Resume state tracking**: `UploadState`/`DownloadState`/`RestoreState` in `resume.rs` with atomic write (write to `.tmp`, rename), graceful degradation on write failure (warn + continue per design 16.1), params_hash invalidation on parameter change
- **Manifest atomicity**: Upload to `.tmp` key, CopyObject to final key, delete `.tmp`. Crash between steps produces "broken" backup cleaned by `clean_broken`
- **Post-download CRC64 verification**: After decompressing each part, recomputes CRC64 from `checksums.txt` and compares against manifest; mismatch triggers retry up to `retries_on_failure`
- **Disk filtering**: `is_disk_excluded()` in `table_filter.rs` checks parts against `skip_disks` and `skip_disk_types` config before processing
- **Partition-level FREEZE**: `--partitions` flag triggers `ALTER TABLE FREEZE PARTITION` per partition instead of whole-table FREEZE
- **Parts column consistency check**: Pre-flight check queries `system.parts_columns` for type inconsistencies before FREEZE; filters benign Enum/Tuple/Nullable drift
- **Broken backup cleanup**: `clean_broken` command deletes broken backups (missing/corrupt metadata.json) from local or remote storage
- **Local retention**: `retention_local()` follows the list->filter->sort->delete pattern; broken backups are excluded from counting; `keep=0` means unlimited, `keep=-1` is upload module's concern
- **Remote retention with GC**: `retention_remote()` calls `gc_collect_referenced_keys()` fresh per-backup-deletion to build a set of all S3 keys referenced by surviving backups, then `gc_delete_backup()` only deletes unreferenced keys (manifest deleted last). Design 8.2 race protection satisfied by fresh key collection each iteration.
- **Config resolution for retention**: `effective_retention_local/remote()` resolves `retention.*` vs `general.*` config -- `retention.*` overrides when non-zero, else falls back to `general.*`
- **Shadow directory cleanup**: `clean_shadow()` queries `get_disks()`, filters out backup-type disks, then removes `chbackup_*` directories from each disk's `shadow/` path; optional name filter matches `chbackup_{sanitized_name}_*`
- **Watch state machine**: `run_watch_loop()` cycles through resume -> create -> upload -> delete_local -> retention -> sleep; uses `tokio::select!` for interruptible sleep (shutdown/reload signals); `WatchState` enum maps to Prometheus IntGauge values (1-7)
- **Watch error recovery**: `force_next_full` flag forces full backup after any error; `consecutive_errors` counter resets to 0 on success; `max_consecutive_errors` (0 = unlimited) triggers loop exit
- **Watch name templates**: `resolve_name_template()` substitutes `{type}`, `{time:FORMAT}`, and ClickHouse `system.macros` values; `resolve_template_prefix()` extracts the static prefix for backup filtering
- **Watch config hot-reload**: SIGHUP (Unix) or `/api/v1/reload` triggers `Config::load()` + `validate()` at next sleep entry; current cycle completes first (design 10.8); logs old->new values for key parameters
- **Watch server integration**: `start_server()` optionally spawns watch loop as background task; `spawn_watch_from_state()` enables dynamic start via API; channels (`watch::Sender<bool>`) for shutdown/reload signaling
- **Watch env var overlay**: `WATCH_INTERVAL` and `FULL_INTERVAL` env vars are mapped in `apply_env_overlay()` for K8s deployments where config file overlay is less ergonomic than environment variables
- **Mode A restore (--rm)**: Phase 0 DROP phase uses reverse engine priority order (Distributed first, data tables last) with retry loop for dependency failures. `drop_tables()` and `drop_databases()` in `schema.rs` follow the same retry pattern as `create_ddl_objects()`. System databases are never dropped.
- **ON CLUSTER DDL**: `add_on_cluster_clause()` injects `ON CLUSTER '{cluster}'` into CREATE/DROP DDL when `restore_schema_on_cluster` config is set. Skipped for DatabaseReplicated databases (detected via `query_database_engine()`).
- **ZK conflict resolution**: Before creating Replicated tables, `resolve_zk_conflict()` parses ZK path + replica from DDL, resolves macros via `system.macros`, checks `system.zookeeper` for existing replicas, and `SYSTEM DROP REPLICA` if conflict. All ZK failures are non-fatal.
- **ATTACH TABLE mode**: When `restore_as_attach` config is true, Replicated*MergeTree tables use DETACH TABLE SYNC -> DROP REPLICA -> hardlink to data dir -> ATTACH TABLE -> SYSTEM RESTORE REPLICA instead of per-part ATTACH. Falls back to normal flow on failure.
- **Mutation re-apply**: After all data attached, `reapply_pending_mutations()` re-applies `pending_mutations` from manifest via `ALTER TABLE ... {command} SETTINGS mutations_sync=2`. Sequential per-table, failures are warnings (not fatal per design 5.7).
- **Distributed cluster rewrite**: `rewrite_distributed_cluster()` changes the cluster name in Distributed engine DDL when `restore_distributed_cluster` config is set
- **ArcSwap hot-swap** (Phase 5): `AppState.config`, `AppState.ch`, `AppState.s3` use `Arc<ArcSwap<T>>` for atomic pointer swap via `store()`/`load()`. The `/api/v1/restart` endpoint reloads config, creates new clients, pings ClickHouse, then atomically swaps all three. On error, old clients remain active (no partial state).
- **ProgressTracker** (Phase 5): `progress.rs` wraps `indicatif::ProgressBar` with TTY detection and `disable` flag. `Clone` via `Arc<ProgressBar>`. Integrated into upload and download parallel pipelines -- each spawned task calls `tracker.inc()` after successful part processing. Disabled in non-TTY (server mode, piped output) or when `config.general.disable_progress_bar` is true.
- **Structured exit codes** (Phase 5): `main()` wraps `run()` and maps errors to exit codes via `exit_code_from_error()` in `error.rs`. Downcasts `anyhow::Error` to `ChBackupError` for variant-specific codes: `LockError` -> 4, `BackupError`/`ManifestError` containing "not found" -> 3, all others -> 1. Clap handles code 2 (usage). SIGINT/SIGTERM handled by tokio signal handlers for codes 130/143.
- **Hardlink dedup** (Phase 5): `--hardlink-exists-files` flag enables post-download deduplication. `find_existing_part()` scans `{data_path}/backup/*/shadow/{table_key}/{part_name}/` (excluding current backup), computes CRC64 of `checksums.txt`, returns first match. `hardlink_existing_part()` creates hardlinks (with EXDEV fallback). Checked before each part download in the parallel pipeline.
- **Skip projections** (Phase 5): `--skip-projections` flag (CLI comma-separated or `config.backup.skip_projections` list) filters `.proj` subdirectories during `hardlink_dir()` shadow walk. `should_skip_projection()` uses `glob::Pattern` matching on the stem (name without `.proj`). Special value `*` skips all projections. `WalkDir::skip_current_dir()` prevents descending into skipped directories.

## Remaining Limitations

- No parallel ATTACH within a single table (deferred -- tables parallel is sufficient)
- No streaming multipart upload (Phase 2a buffers compressed data, then decides single vs multipart)
- API tables endpoint has no pagination (returns all tables in a single response)
- rbac_size and config_size in list API response are hardcoded to 0 (requires scanning directory sizes)

## Build & Test

```bash
cargo build --release --target x86_64-unknown-linux-musl  # static binary
cargo test                                                  # unit tests
docker build -t chbackup .                                  # production image
```

Integration tests require real ClickHouse + S3 (no mocks).

## Infrastructure Files

| File | Purpose |
|------|---------|
| `Dockerfile` | Production multi-stage build (musl static binary, Alpine runtime) |
| `.github/workflows/ci.yml` | CI with ClickHouse version matrix (23.8, 24.3, 24.8, 25.1) |
| `examples/kubernetes/sidecar.yaml` | K8s sidecar deployment example (server --watch) |
| `test/fixtures/seed_data.sql` | Deterministic test data for round-trip verification |
| `test/run_tests.sh` | Integration test runner with round-trip smoke test |

## Git Conventions

- Conventional commits: `feat:`, `fix:`, `refactor:`, `test:`, `docs:`, `chore:`
- Zero warnings policy
- Never mention Claude, AI, or any AI tool in commits/PRs
