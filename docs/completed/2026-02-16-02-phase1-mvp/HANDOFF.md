# Handoff: Phase 1 -- MVP: Single-Table Backup & Restore

## Plan Location
`docs/plans/2026-02-16-02-phase1-mvp/`

## Directory Contents

| File | Purpose |
|------|---------|
| PLAN.md | Task definitions and TDD steps for 13 tasks across 6 dependency groups |
| SESSION.md | Status tracking and checklists |
| acceptance.json | 4-layer verification criteria (13 features) |
| HANDOFF.md | This file - resume context |
| context/patterns.md | Discovered patterns from Phase 0 codebase |
| context/symbols.md | Type verification with crate API details |
| context/diagnostics.md | Compiler state (clean), test state (14 pass), dependency analysis |
| context/references.md | Symbol and reference analysis for Phase 1 |
| context/git-history.md | Git context and Phase 0 completion summary |
| context/preventive-rules-applied.md | Applied preventive rules (RC-006, RC-008, RC-015, RC-016, RC-019, RC-021, RC-032) |
| context/knowledge_graph.json | Structured JSON for all verified symbols and planned new symbols |
| context/affected-modules.json | Machine-readable module status (4 new dirs, 2 updates, 3 new files, 4 modified files) |
| context/affected-modules.md | Human-readable module summary |
| context/redundancy-analysis.md | No redundancy found - all new modules |
| context/data-authority.md | Data source verification |

## To Resume

1. Read SESSION.md for current status
2. Check Agent Execution Status table
3. Continue from last incomplete phase/agent
4. Use `/project-execute` when planning is complete

## Key References

### Design Document Sections (Phase 1 scope)
- `docs/design.md` section 3.1: Pre-flight mutation check
- `docs/design.md` section 3.2: Pre-flight SYNC REPLICA
- `docs/design.md` section 3.4: FREEZE and Collect (sequential in Phase 1)
- `docs/design.md` section 3.6: Upload (sequential in Phase 1)
- `docs/design.md` section 4: Download (sequential in Phase 1)
- `docs/design.md` section 5.1-5.3: Restore (Mode B only, local disk)
- `docs/design.md` section 7/7.1: Manifest format

### Files Being Modified
- `Cargo.toml` -- add dependencies: crc, glob, nix, tar, async-compression
- `src/error.rs` -- add BackupError, RestoreError, ManifestError variants
- `src/lib.rs` -- add pub mod declarations for 7 new modules
- `src/main.rs` -- wire command match arms to new module entry points
- `src/clickhouse/client.rs` -- extend ChClient with FREEZE/UNFREEZE/query methods
- `src/storage/s3.rs` -- extend S3Client with put/get/list/delete methods

### Files Being Created
- `src/manifest.rs` -- BackupManifest, TableManifest, PartInfo, DatabaseInfo structs
- `src/table_filter.rs` -- glob-based table filter for -t flag
- `src/list.rs` -- local + remote backup listing and delete
- `src/backup/mod.rs` -- backup::create entry point
- `src/backup/freeze.rs` -- FreezeGuard, freeze_table, unfreeze_table
- `src/backup/mutations.rs` -- check_mutations
- `src/backup/sync_replica.rs` -- sync_replicas
- `src/backup/checksum.rs` -- compute_crc64
- `src/backup/collect.rs` -- shadow walk, hardlink, collect parts
- `src/upload/mod.rs` -- upload::upload entry point
- `src/upload/stream.rs` -- compress_part (tar + lz4)
- `src/download/mod.rs` -- download::download entry point
- `src/download/stream.rs` -- decompress_part (lz4 + untar)
- `src/restore/mod.rs` -- restore::restore entry point
- `src/restore/schema.rs` -- CREATE DATABASE/TABLE from DDL
- `src/restore/attach.rs` -- hardlink + ATTACH PART + chown
- `src/restore/sort.rs` -- SortPartsByMinBlock

### Test Files
- Unit tests embedded in source files (`#[cfg(test)] mod tests`)
- Key test areas: manifest serde roundtrip, table filter glob matching, CRC64 computation, part name parsing, sort order, compress/decompress roundtrip

### Related Documentation
- `docs/roadmap.md` -- Phase 1 section with definition of done
- `docs/design.md` -- Full technical specification

## Architecture Constraints

- **Sequential only** -- No tokio::spawn for parallel operations. Phase 2 adds parallelism.
- **Buffered upload** -- Compress to Vec<u8>, then PutObject. Phase 2 adds streaming multipart.
- **Mode B restore only** -- Non-destructive (no --rm DROP). Phase 4d adds Mode A.
- **Local disk only** -- No S3 object disk parts. Phase 2c adds S3 disk support.
- **No incrementals** -- Full backup every time. Phase 2b adds --diff-from.
- **HTTP interface** -- clickhouse crate uses HTTP port (8123), not native port (9000).
