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

## Source Module Map

```
src/
  main.rs            -- CLI entry point, command dispatch
  lib.rs             -- Module declarations
  cli.rs             -- clap derive CLI definitions
  concurrency.rs     -- Effective concurrency accessors (upload, download, max_connections, object_disk_copy)
  config.rs          -- Config loader (~106 params, env overlay)
  error.rs           -- ChBackupError (thiserror)
  lock.rs            -- PidLock (three-tier scope)
  logging.rs         -- tracing init (text/JSON)
  manifest.rs        -- BackupManifest, TableManifest, PartInfo, S3ObjectInfo, DatabaseInfo (serde JSON)
  object_disk.rs     -- ClickHouse S3 object disk metadata parser (5 format versions)
  rate_limiter.rs    -- Token-bucket rate limiter (shared via Arc, 0 = unlimited)
  table_filter.rs    -- Glob pattern matching for -t flag
  list.rs            -- list + delete commands (local dir scan + S3)
  backup/            -- create command: parallel FREEZE, disk-aware shadow walk, hardlink/S3 metadata, CRC64, UNFREEZE
  upload/            -- upload command: parallel tar+LZ4 compress, S3 PutObject/multipart, CopyObject for S3 disk parts
  download/          -- download command: parallel S3 GetObject, LZ4+untar decompress, metadata-only for S3 disk parts
  restore/           -- restore command: CREATE DB/TABLE, parallel hardlink+ATTACH PART, UUID-isolated S3 CopyObject restore
  clickhouse/        -- ChClient wrapper (FREEZE/UNFREEZE, DDL, queries, DiskRow with remote_path)
  storage/           -- S3Client wrapper (put, get, list, delete, head, multipart, copy_object)
```

Each directory module has its own `CLAUDE.md` with detailed API and pattern documentation.

## Data Flow

```
create:   Config -> ChClient -> FREEZE -> walk shadow (all disks) -> hardlink (local) / parse metadata (S3 disk) -> CRC64 -> UNFREEZE -> BackupManifest -> JSON
upload:   BackupManifest(JSON) -> local parts: tar+lz4 compress -> S3Client.put_object | S3 disk parts: S3Client.copy_object -> manifest last
download: S3Client.get_object(manifest) -> BackupManifest -> local parts: S3Client.get_object -> lz4+untar | S3 disk parts: metadata only
restore:  BackupManifest(JSON) -> CREATE DB/TABLE -> local parts: hardlink to detached/ | S3 disk parts: CopyObject to UUID paths + rewrite metadata -> ATTACH PART -> chown
list:     scan local dirs + S3Client.list -> display
delete:   rm local dir or S3Client.delete_objects
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

## Remaining Limitations

- No resume (Phase 2d)
- No Mode A restore / --rm (Phase 4d)
- No table remap / --as (Phase 4a)
- No RBAC/config backup (Phase 4e)
- No parallel ATTACH within a single table (deferred -- tables parallel is sufficient)
- No streaming multipart upload (Phase 2a buffers compressed data, then decides single vs multipart)

## Build & Test

```bash
cargo build --release --target x86_64-unknown-linux-musl  # static binary
cargo test                                                  # unit tests
```

Integration tests require real ClickHouse + S3 (no mocks).

## Git Conventions

- Conventional commits: `feat:`, `fix:`, `refactor:`, `test:`, `docs:`, `chore:`
- Zero warnings policy
- Never mention Claude, AI, or any AI tool in commits/PRs
