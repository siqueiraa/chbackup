# Diagnostics Report

## Compiler State (cargo check)

**Result:** CLEAN - 0 errors, 0 warnings

```
$ cargo check
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.12s
```

The codebase compiles cleanly. No pre-existing errors or warnings to account for.

## Key Observations from Diagnostics

### BUG-1: post_actions stub (src/server/routes.rs:217-271)

The `post_actions` handler at line 248-254 spawns a tokio task that:
1. Logs the command
2. Immediately calls `state_clone.finish_op(id).await`
3. Does NOT actually dispatch to create/upload/download/restore/etc.

Comment on line 250-252 confirms this is a known stub:
```rust
// For now, we mark the operation as completed immediately.
// Full dispatch to actual command functions is wired via the dedicated
// POST endpoints (create, upload, etc.) which parse proper request bodies.
```

The match arms on line 235-236 correctly recognize valid commands but the body is a no-op.

### MISSING-1: list_backups missing offset/limit/format (src/server/routes.rs:274-318)

- `ListParams` (line 64-69) has only `location` and `desc` fields
- `list_backups()` returns `Result<Json<Vec<ListResponse>>, ...>` without headers
- `TablesParams` (line 88-97) already has `offset: Option<usize>` and `limit: Option<usize>` -- pattern to follow
- `tables()` (line 1353) returns a tuple `(headers, Json<...>)` with `X-Total-Count` header -- pattern to follow

### MISSING-2: ListResponse.required always empty (src/server/routes.rs:333)

- `summary_to_list_response()` at line 333 hardcodes `required: String::new()`
- The `required` field should contain comma-separated backup names that this backup's parts reference via `carried:{base_name}` source
- `collect_incremental_bases()` in list.rs:929 already has the logic for extracting carried bases from manifests
- For local backups: needs to load manifest and extract carried sources
- For remote backups: manifest is already loaded during `list_remote()` but carried source info is discarded

### MISSING-3: No SIGTERM handler in server (src/server/mod.rs)

- The server uses `tokio::signal::ctrl_c()` for graceful shutdown (only SIGINT, line 334)
- SIGHUP handler exists (line 217-228)
- SIGQUIT handler exists (line 239-254)
- No `SignalKind::terminate()` handler for SIGTERM
- In K8s, `kubectl delete pod` sends SIGTERM, not SIGINT -- the server needs to handle both

### MISSING-4: object_disk_size always 0 (src/server/routes.rs:328)

- `summary_to_list_response()` at line 328 hardcodes `object_disk_size: 0`
- Comment says: `// Requires manifest disk_types analysis (future)`
- The `BackupManifest` already has `disk_types` (line 56 of manifest.rs) and `PartInfo.s3_objects` (line 152-153)
- `is_s3_disk()` in object_disk.rs:280 checks for "s3" or "object_storage" disk types
- Computation: sum `part.size` for parts on disks where `disk_types[disk_name]` is an S3 disk type

### MISSING-5: BackupSummary lacks required and object_disk_size fields

- `BackupSummary` (list.rs:46-68) has 10 fields but is missing:
  - `object_disk_size: u64` -- needed for MISSING-4
  - `required: String` -- needed for MISSING-2
- Both `parse_backup_summary()` (list.rs:1187) and `list_remote()` (list.rs:375) construct BackupSummary without these fields
- Adding these fields requires updating ALL BackupSummary construction sites (approximately 10 locations in list.rs + tests)

## Signal Handler Summary

| Signal | main.rs (standalone) | server/mod.rs | Handler |
|--------|---------------------|---------------|---------|
| SIGINT (Ctrl+C) | tokio::signal::ctrl_c() | with_graceful_shutdown(ctrl_c) | Shutdown watch loop |
| SIGHUP | signal(SignalKind::hangup()) | signal(SignalKind::hangup()) | Config reload |
| SIGQUIT | signal(SignalKind::quit()) | signal(SignalKind::quit()) | Stack dump |
| SIGTERM | NOT HANDLED | NOT HANDLED | Should trigger graceful shutdown |

## Integration Table DDL Reference

The `system.backup_list` table DDL (clickhouse/client.rs:1397-1411) expects these columns:
```sql
name String,
created String,
location String,
size UInt64,
data_size UInt64,
object_disk_size UInt64,
metadata_size UInt64,
rbac_size UInt64,
config_size UInt64,
compressed_size UInt64,
required String
```

The `required` column must contain the backup name(s) this backup depends on (incremental base). Empty string for non-incremental backups.
