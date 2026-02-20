# Redundancy Analysis

## New Public Components Proposed

| Proposed | Existing Match (fully qualified) | Decision | Task/Acceptance | Justification |
|----------|----------------------------------|----------|-----------------|---------------|
| `upload::stream::compress_part` with format param | `upload::stream::compress_part(part_dir, archive_name)` | EXTEND | Task for compression | Adding `format: &str` and `level: u32` params to existing function |
| `download::stream::decompress_part` with format param | `download::stream::decompress_part(data, output_dir)` | EXTEND | Task for decompression | Adding `format: &str` param to existing function |
| `clickhouse::ChClient::check_json_columns` | `clickhouse::ChClient::check_parts_columns` | COEXIST | Task for JSON detection | Different tables (system.columns vs system.parts_columns), different purposes (type detection vs consistency check). Cleanup deadline: N/A - permanent coexistence. |
| `upload::s3_key_for_part` with format extension | `upload::s3_key_for_part(backup_name, db, table, part_name)` | EXTEND | Task for S3 key | Adding `data_format: &str` param to derive correct file extension |
| `tables` command in main.rs | `Command::Tables` stub in main.rs | REPLACE | Task for tables command | Replacing stub with actual implementation |

## Notes

- No new public structs proposed -- all features extend existing code
- `compress_part` exists in BOTH `upload/stream.rs` AND `download/stream.rs` -- both need the same format extension
- The `tables` command "replace" is replacing a stub, not removing working code
- The `print_backup_table` function in `list.rs` will be EXTENDED (more columns), not replaced

## COEXIST Justification

`check_json_columns` vs `check_parts_columns`:
- `check_parts_columns`: Queries `system.parts_columns` for type CONSISTENCY across parts (same column, different types in different parts)
- `check_json_columns`: Queries `system.columns` for column TYPE detection (find Object/JSON types that cannot be frozen)
- These serve fundamentally different purposes and query different system tables
- Both are needed permanently -- no cleanup deadline
