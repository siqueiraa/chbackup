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
| ClickHouse | `clickhouse-rs` (async, connection pooling) |
| S3 | `aws-sdk-s3` + `aws-config` |
| Async | `tokio` |
| Async streams | `tokio-util` (codec feature) |
| Directory walks | `walkdir` (via `spawn_blocking`) |
| Errors | `thiserror` + `anyhow` |
| Logging | `tracing` + `tracing-subscriber` |
| Compression | `lz4_flex` + `async-compression` |
| Archiving | `tar` (v0.4, sync, via `spawn_blocking`) |
| CRC64 | `crc` (v3, CRC_64_XZ algorithm) |
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

## Source Module Map

```
src/
  main.rs            -- CLI entry point, command dispatch
  lib.rs             -- Module declarations
  cli.rs             -- clap derive CLI definitions
  config.rs          -- Config loader (~106 params, env overlay)
  error.rs           -- ChBackupError (thiserror)
  lock.rs            -- PidLock (three-tier scope)
  logging.rs         -- tracing init (text/JSON)
  manifest.rs        -- BackupManifest, TableManifest, PartInfo, DatabaseInfo (serde JSON)
  table_filter.rs    -- Glob pattern matching for -t flag
  list.rs            -- list + delete commands (local dir scan + S3)
  backup/            -- create command: FREEZE, shadow walk, hardlink, CRC64, UNFREEZE
  upload/            -- upload command: tar+LZ4 compress, S3 PutObject
  download/          -- download command: S3 GetObject, LZ4+untar decompress
  restore/           -- restore command: CREATE DB/TABLE, hardlink to detached, ATTACH PART
  clickhouse/        -- ChClient wrapper (FREEZE/UNFREEZE, DDL, queries)
  storage/           -- S3Client wrapper (put, get, list, delete, head)
```

Each directory module has its own `CLAUDE.md` with detailed API and pattern documentation.

## Data Flow

```
create:   Config -> ChClient -> FREEZE -> walk shadow -> hardlink -> CRC64 -> UNFREEZE -> BackupManifest -> JSON
upload:   BackupManifest(JSON) -> read parts -> tar+lz4 compress -> S3Client.put_object -> manifest last
download: S3Client.get_object(manifest) -> BackupManifest -> S3Client.get_object(parts) -> lz4+untar -> local
restore:  BackupManifest(JSON) -> CREATE DB/TABLE -> hardlink to detached/ -> ATTACH PART -> chown
list:     scan local dirs + S3Client.list -> display
delete:   rm local dir or S3Client.delete_objects
```

## Key Implementation Patterns

- **Client wrappers**: `ChClient` and `S3Client` wrap third-party crates with config-driven setup
- **BackupManifest**: Central data structure flowing between all commands; serialized as `metadata.json`
- **FreezeGuard**: Tracks frozen tables for cleanup; explicit `unfreeze_all()` (not Drop-based)
- **SortPartsByMinBlock**: Parse part name from right (partition can contain underscores)
- **Hardlink with EXDEV fallback**: `std::fs::hard_link` with copy fallback on cross-device (error 18)
- **Buffered upload/download**: Phase 1 buffers in memory; Phase 2 will add streaming multipart
- **spawn_blocking for sync I/O**: walkdir, tar, lz4 compression/decompression run via `spawn_blocking`

## Phase 1 Limitations

- No parallel operations (Phase 2)
- No multipart S3 upload (Phase 2)
- No incremental backups / --diff-from (Phase 2b)
- No S3 disk support (Phase 2c)
- No resume (Phase 2d)
- No Mode A restore / --rm (Phase 4d)
- No table remap / --as (Phase 4a)
- No RBAC/config backup (Phase 4e)

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
