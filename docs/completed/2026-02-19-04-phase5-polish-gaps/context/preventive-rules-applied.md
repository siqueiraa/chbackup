# Preventive Rules Applied

## Rules Checked

| Rule | Applicable? | Notes |
|------|-------------|-------|
| RC-001 (Actor dependency wiring) | No | No actors in this project (not kameo-based) |
| RC-002 (Schema/type mismatch) | Yes | Verified types via source code for all 7 items |
| RC-003 (Tracking files not updated) | Yes | Will apply during execution |
| RC-004 (Message handler without sender) | No | No actors |
| RC-005 (Zero/null division) | No | No financial calculations in this plan |
| RC-006 (Unverified APIs in plan) | Yes | All APIs verified via grep -- see symbols.md |
| RC-007 (Tuple field order assumed) | No | No tuple types involved |
| RC-008 (TDD sequencing) | Yes | Will verify field availability per task |
| RC-015 (Cross-task return type mismatch) | Yes | Check data flows between tasks |
| RC-016 (Struct field completeness) | Yes | New structs checked for consumer usage |
| RC-017 (State field declaration) | Yes | All self.X verified |
| RC-019 (Existing pattern not followed) | Yes | Each item follows existing patterns -- documented |
| RC-021 (Struct location assumed) | Yes | All locations verified via grep |
| RC-032 (Data authority) | Partially | Item 7 (metadata_size) has data authority implications |
| RC-035 (cargo fmt) | Yes | Will apply during execution |

## Key Findings

1. **RC-006 compliance**: All functions referenced in the plan verified:
   - `list_tables()` at `src/clickhouse/client.rs:288`
   - `list_all_tables()` at `src/clickhouse/client.rs:315`
   - `format_size()` at `src/list.rs:887`
   - `collect_parts()` at `src/backup/collect.rs:116`
   - `TableRow` at `src/clickhouse/client.rs:25`
   - `BackupManifest.metadata_size` at `src/manifest.rs:48`
   - `BackupSummary` at `src/list.rs:28`

2. **RC-021 compliance**: All struct/file locations verified:
   - `Config.general.disable_progress_bar` at `src/config.rs:47`
   - `BackupConfig.skip_projections` at `src/config.rs:370`
   - `ChBackupError` at `src/error.rs:4`
   - `ListResponse` at `src/server/routes.rs:66`
   - `AppState` at `src/server/state.rs`
   - `summary_to_list_response()` at `src/server/routes.rs:274`

3. **RC-032 compliance**: For item 7 (metadata_size), the manifest already stores `metadata_size: u64` (manifest.rs:48). The `BackupSummary` does NOT expose `metadata_size`. The `summary_to_list_response()` function hardcodes it to 0 instead of reading from the manifest. Solution: thread metadata_size through BackupSummary.
