# Preventive Rules Applied

## Root Causes Read

- **File read:** `.claude/skills/self-healing/references/root-causes.md` (35 rules)
- **File read:** `.claude/skills/self-healing/references/planning-rules.md` (14 rules)

## Rules Checked

| Rule | Applies | Check Result |
|------|---------|--------------|
| RC-001 | NO | No actors in this plan -- CLI application |
| RC-002 | YES | All types verified via source code reads. **Caught correction:** `TableRow.total_bytes` is `Option<u64>` not `u64` -- tables command must use `.unwrap_or(0)` |
| RC-004 | NO | No message/handler patterns -- not an actor system |
| RC-005 | NO | No division operations in this plan |
| RC-006 | YES | All APIs verified in source: `compress_part()` at upload/stream.rs:16 and download/stream.rs:36, `decompress_part()` at download/stream.rs:16, `s3_key_for_part()` at upload/mod.rs:68, `list_tables()` at client.rs:276, `check_parts_columns()` at client.rs:604, `print_backup_table()` at list.rs:907, `format_size()` at list.rs:887, `TableFilter::matches()` at table_filter.rs:47 |
| RC-007 | YES | Tuple/struct field order verified for BackupSummary, PartInfo, TableRow, ColumnInconsistency via source reads |
| RC-008 | YES | TDD sequencing: Cargo.toml deps added in Task 5 (first of Group D), before stream.rs changes in Tasks 6-7. All other groups are independent. |
| RC-015 | YES | Cross-task data flows: compress_part and decompress_part format parameter consistent across upload and download. s3_key_for_part extension matches compress_part format string. |
| RC-016 | NO | No new struct definitions -- extending existing functions only |
| RC-017 | NO | No new `self.X` state fields |
| RC-018 | YES | Every task has explicit TDD steps with named test functions, inputs, and expected assertions |
| RC-019 | YES | New `check_json_columns()` follows exact pattern of `check_parts_columns()` at client.rs:604-661. Tables command follows existing dispatch pattern (see `Command::List` at main.rs:373-380). |
| RC-021 | YES | All file locations verified via reads: compress_part in upload/stream.rs:16 and download/stream.rs:36, s3_key_for_part in upload/mod.rs:68, print_backup_table in list.rs:907, check_parts_columns in client.rs:604 |
| RC-029 | NO | No sync-to-async signature changes |
| RC-032 | YES | Data authority verified: `system.columns` for JSON/Object type detection (new query, documented in data-authority.md) |
| RC-035 | YES | Each task notes: run cargo fmt before commit |

## Key Findings

1. **Type correction:** `TableRow.total_bytes` is `Option<u64>` not `u64`. Tables command output must use `.unwrap_or(0)`.
2. **Compression format:** Config already validates "lz4|zstd|gzip|none" (config.rs:1235). Manifest already has `data_format` field. Only compress/decompress code and S3 key extension are hardcoded to lz4.
3. **Tables command:** CLI fully defined (cli.rs:260-273). Stubbed in main.rs:382. `list_tables()` exists but always excludes system DBs -- need a new `list_all_tables()` for `--all` flag.
4. **JSON/Object detection:** New query against `system.columns` needed. Follows same pattern as `check_parts_columns()`. Warning only, non-blocking.
5. **List enhancement:** `BackupSummary` already has `compressed_size`. `format_size()` already exists. Only `print_backup_table()` needs updating.
6. **Download does NOT use manifest.data_format** -- currently hardcoded to lz4. This is the key gap.
