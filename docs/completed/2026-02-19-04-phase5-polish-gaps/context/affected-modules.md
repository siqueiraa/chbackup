# Affected Modules Analysis

## Summary

- **Modules to update:** 3 (src/server, src/backup, src/download)
- **Modules to create:** 0
- **Top-level files modified:** 3 (main.rs, error.rs, list.rs)
- **Git base:** 9fff0b48

## Modules Being Modified

| Module | CLAUDE.md Status | Triggers | Action | Items |
|--------|------------------|----------|--------|-------|
| src/server | EXISTS | new_patterns | UPDATE | 1 (tables endpoint), 2 (restart endpoint), 7 (metadata_size) |
| src/backup | EXISTS | new_patterns | UPDATE | 3 (skip-projections in collect.rs) |
| src/download | EXISTS | new_patterns | UPDATE | 4 (hardlink-exists-files dedup) |
| src/clickhouse | EXISTS | none | NO CHANGE | Existing APIs sufficient |

## Top-Level Files Modified

| File | Items | Changes |
|------|-------|---------|
| src/main.rs | 3, 4, 6 | Wire skip-projections, wire hardlink-exists-files, structured exit codes |
| src/error.rs | 6 | Exit code mapping (or new function/trait) |
| src/list.rs | 7 | Add metadata_size to BackupSummary, populate from manifest |
| Cargo.toml | 5 | Add indicatif dependency (if progress bar implemented) |

## CLAUDE.md Tasks to Generate

1. **Update:** src/server/CLAUDE.md -- Document tables and restart endpoints replacing stubs
2. **Update:** src/backup/CLAUDE.md -- Document skip-projections filter in collect_parts
3. **Update:** src/download/CLAUDE.md -- Document hardlink-exists-files dedup

## Item-to-File Mapping

| Item | Files Modified |
|------|---------------|
| 1. API tables endpoint | server/routes.rs, server/mod.rs |
| 2. API restart endpoint | server/routes.rs, server/mod.rs, server/state.rs |
| 3. skip-projections | main.rs, backup/mod.rs, backup/collect.rs |
| 4. hardlink-exists-files | main.rs, download/mod.rs |
| 5. Progress bar | Cargo.toml, new src/progress.rs (or inline), upload/mod.rs, download/mod.rs, backup/mod.rs, restore/mod.rs |
| 6. Exit codes | main.rs, error.rs |
| 7. List response sizes | list.rs, server/routes.rs |
