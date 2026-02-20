# Redundancy Analysis

## New Public Components Proposed

| Proposed | Description |
|----------|-------------|
| `restore::remap` module | New module for DDL rewriting and table name mapping |
| `RemapConfig` struct | Configuration for remap (parsed from CLI `--as` / `-m` flags) |
| `rewrite_ddl_for_remap()` | Function to rewrite CREATE TABLE DDL for new db/table name |
| `rewrite_database_ddl()` | Function to rewrite CREATE DATABASE DDL for new database name |
| `parse_database_mapping()` | Function to parse `-m prod:staging,logs:logs_copy` string |
| `remap_table_key()` | Function to compute new `db.table` key from original + remap config |

## Search Results

### `rewrite_ddl` / DDL manipulation

Existing functions in `src/restore/schema.rs`:
- `ensure_if_not_exists_database(ddl: &str) -> String` (private)
- `ensure_if_not_exists_table(ddl: &str) -> String` (private)

These are simple string replacements (add `IF NOT EXISTS`). The remap DDL rewriting is significantly more complex (regex parsing of engine params, table name replacement, UUID removal, ZK path rewriting).

**Decision: COEXIST** -- The existing `ensure_if_not_exists_*` functions serve a different purpose (safety guard). The new remap functions handle name/path/UUID transformations. No overlap in functionality. The existing functions will continue to be called after the remap rewriting.

### `parse_*_mapping` / Config parsing

No existing function parses colon-separated mapping strings.

**Decision: CREATE** -- No existing equivalent.

### `RemapConfig` / Remap configuration

No existing remap-related types.

**Decision: CREATE** -- No existing equivalent.

## Decision Table

| Proposed | Existing Match (fully qualified) | Decision | Task/Acceptance | Justification |
|----------|----------------------------------|----------|-----------------|---------------|
| `restore::remap` module | (none) | CREATE | - | No existing remap module |
| `RemapConfig` struct | (none) | CREATE | - | No existing remap config type |
| `rewrite_ddl_for_remap()` | `schema::ensure_if_not_exists_table()` | COEXIST | cleanup: N/A | Different purpose: name/UUID/ZK rewriting vs IF NOT EXISTS guard |
| `rewrite_database_ddl()` | `schema::ensure_if_not_exists_database()` | COEXIST | cleanup: N/A | Different purpose: database name rewriting vs IF NOT EXISTS guard |
| `parse_database_mapping()` | (none) | CREATE | - | No existing mapping parser |
| `remap_table_key()` | (none) | CREATE | - | No existing table key remapping |

## COEXIST Justification

The `ensure_if_not_exists_*` functions and the new remap functions operate at different stages of the DDL pipeline:
1. First: `rewrite_ddl_for_remap()` transforms the DDL to change table name, UUID, ZK path
2. Then: `ensure_if_not_exists_table()` adds the `IF NOT EXISTS` safety guard

They are orthogonal and compose sequentially. No cleanup needed.
