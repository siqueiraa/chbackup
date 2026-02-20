# Handoff: Phase 4f -- Operational Extras

## Plan Location
`docs/plans/2026-02-19-03-phase4f-operational-extras/`

## Directory Contents

| File | Purpose |
|------|---------|
| PLAN.md | Task definitions and TDD steps (8 tasks, 5 dependency groups) |
| SESSION.md | Status tracking and checklists |
| acceptance.json | 4-layer verification criteria (5 features: F001-F004, FDOC) |
| HANDOFF.md | This file -- resume context |
| context/patterns.md | Compression pipeline and ChClient query patterns |
| context/symbols.md | Type verification table for all referenced types |
| context/knowledge_graph.json | Structured JSON for symbol lookup |
| context/affected-modules.json | Module status for CLAUDE.md updates (4 modules to update) |
| context/affected-modules.md | Human-readable module summary |
| context/redundancy-analysis.md | EXTEND/COEXIST decisions for all components |
| context/diagnostics.md | Clean compilation baseline (0 errors, 0 warnings) |
| context/references.md | Reference analysis for all 4 features |
| context/git-history.md | Git context and file-specific history |
| context/preventive-rules-applied.md | Applied root-cause rules with type correction findings |
| context/data-authority.md | Data source authority for JSON column detection |

## To Resume

1. Read SESSION.md for current status
2. Check Agent Execution Status table
3. Continue from last incomplete phase/agent
4. Use `/project-execute` when planning is complete

## Key References

### Files Being Modified
- `src/list.rs` -- Enhanced print_backup_table() with compressed size column
- `src/clickhouse/client.rs` -- New check_json_columns() and list_all_tables() methods
- `src/backup/mod.rs` -- JSON/Object column pre-flight integration
- `src/main.rs` -- Tables command implementation (replacing stub)
- `src/upload/stream.rs` -- Multi-format compress_part() with archive_extension()
- `src/download/stream.rs` -- Multi-format decompress_part() and compress_part()
- `src/upload/mod.rs` -- s3_key_for_part() dynamic extension, compress_part call site
- `src/download/mod.rs` -- decompress_part call site with manifest.data_format
- `Cargo.toml` -- Add flate2 and zstd dependencies

### Test Files
- Tests are co-located in the same files (Rust `#[cfg(test)] mod tests` pattern)
- `src/upload/stream.rs` -- Compression roundtrip tests (lz4, zstd, gzip, none)
- `src/download/stream.rs` -- Decompression roundtrip tests
- `src/list.rs` -- List output format tests
- `src/clickhouse/client.rs` -- JSON column detection tests

### Related Documentation
- `docs/design.md` section 2 (tables command), section 12 (compression), section 16.4 (JSON columns)
- `docs/roadmap.md` Phase 4f

### Critical Type Notes
- `TableRow.total_bytes` is `Option<u64>` -- use `.unwrap_or(0)` for display
- `BackupConfig.compression_level` is `u32` -- cast to `i32` for zstd encoder
- `TableFilter::matches()` always excludes system DBs -- use direct pattern access for --all
