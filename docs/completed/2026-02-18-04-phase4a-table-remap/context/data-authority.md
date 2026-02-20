# Data Authority Analysis

## Overview

Phase 4a (Table/Database Remap) does not introduce tracking fields, accumulators, or calculations. It performs **string transformations** on DDL statements and **key remapping** on manifest table entries at restore time. No new data sources are needed beyond what the manifest and ClickHouse system tables already provide.

## Data Requirements

| Data Needed | Source Type | Field Available | Decision |
|-------------|-------------|-----------------|----------|
| Original table DDL | BackupManifest | `TableManifest.ddl` (String) | USE EXISTING |
| Original table UUID | BackupManifest | `TableManifest.uuid` (Option<String>) | USE EXISTING |
| Original engine name | BackupManifest | `TableManifest.engine` (String) | USE EXISTING |
| Original database DDL | BackupManifest | `DatabaseInfo.ddl` (String) | USE EXISTING |
| Original database name | BackupManifest | `DatabaseInfo.name` (String) | USE EXISTING |
| Table key mapping | Manifest + CLI args | `manifest.tables` keys + `--as`/`-m` flags | USE EXISTING - parse at restore entry |
| Destination table UUID | ClickHouse (live) | `TableRow.uuid` from `list_tables()` | USE EXISTING - already queried in restore flow |
| Destination table existence | ClickHouse (live) | `ChClient.table_exists(db, table)` | USE EXISTING |
| ZooKeeper path from DDL | DDL string parsing | Regex on ReplicatedMergeTree params | MUST IMPLEMENT - string parsing of DDL engine clause |
| DDL table name rewriting | None | N/A | MUST IMPLEMENT - regex/string replacement on DDL |
| DDL UUID removal | None | N/A | MUST IMPLEMENT - strip UUID clause from DDL |
| DDL ZK path rewriting | None | N/A | MUST IMPLEMENT - modify replica path in engine params |
| Database mapping parsing | CLI argument | `-m prod:staging,logs:logs_copy` | MUST IMPLEMENT - parse comma+colon format to HashMap |

## Analysis Notes

- All source data (DDL strings, table names, UUIDs) is already available in the `BackupManifest` which is loaded at the start of every restore operation
- The DDL rewriting is a purely computational transformation -- no additional queries to ClickHouse or S3 are needed
- The only "new" data is the remap configuration itself, which comes from CLI flags (`--as`, `-m`)
- UUID is explicitly NOT carried over during remap (design doc 6.1 says "let ClickHouse assign new one")
- ZK path rewriting requires parsing the engine parameters from the DDL string, which is a string manipulation task

## MUST IMPLEMENT Justifications

1. **DDL table name rewriting**: The manifest stores DDL with the original table name. When restoring as a different name, the DDL must be modified. No existing code does this.
2. **DDL UUID removal**: The manifest DDL may contain a UUID clause. For remap, we must omit it so ClickHouse assigns a new one. No existing code does this.
3. **DDL ZK path rewriting**: ReplicatedMergeTree engine params include a ZK path containing the database/table name. Must be updated to reflect the new name. No existing code does this.
4. **Database mapping parsing**: The `-m` flag value is a comma-separated string that must be parsed into a `HashMap<String, String>`. No existing code does this.
