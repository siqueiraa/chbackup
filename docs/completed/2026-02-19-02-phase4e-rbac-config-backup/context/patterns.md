# Pattern Discovery

No global patterns directory exists (`docs/patterns/` does not exist). All patterns discovered locally from the codebase.

## Identified Patterns

### 1. Backup Extension Pattern (backup/mod.rs)

The `create()` function follows a linear pipeline:
1. Pre-flight checks (mutations, sync replicas, parts columns)
2. FREEZE tables + collect parts (parallel)
3. UNFREEZE all tables
4. **Extension point**: Between UNFREEZE and manifest save (lines 518-536)
5. Build manifest with `functions: Vec::new()`, `named_collections: Vec::new()`, `rbac: None`
6. Apply incremental diff if `--diff-from`
7. Save manifest

The `create()` signature currently does NOT accept `rbac`/`configs`/`named_collections` flags. These must be added as parameters (the flags exist in CLI but are currently ignored with warnings in main.rs).

### 2. Restore Phase 4 Extension Pattern (restore/mod.rs)

The restore flow has explicit phases:
```
Phase 0: DROP (Mode A)
Phase 1: CREATE databases
Phase 2: CREATE + ATTACH data tables
Phase 2.5: Re-apply mutations
Phase 2b: CREATE postponed tables
Phase 3: CREATE DDL-only objects (topo sorted)
Phase 4: CREATE functions  <-- extend here with named collections, RBAC, configs
```

Phase 4 currently only calls `create_functions(ch, &manifest, on_cluster)` (line 649 in restore/mod.rs). The restore function also does NOT accept `rbac`/`configs`/`named_collections` flags.

### 3. create_functions() Reference Pattern (restore/schema.rs:715-755)

```rust
pub async fn create_functions(
    ch: &ChClient,
    manifest: &BackupManifest,
    on_cluster: Option<&str>,
) -> Result<()> {
    if manifest.functions.is_empty() {
        debug!("No functions to create");
        return Ok(());
    }
    let mut created = 0u32;
    for func_ddl in &manifest.functions {
        let ddl = match on_cluster {
            Some(cluster) => add_on_cluster_clause(func_ddl, cluster),
            None => func_ddl.clone(),
        };
        match ch.execute_ddl(&ddl).await {
            Ok(()) => {
                info!(ddl = %func_ddl, "Created function");
                created += 1;
            }
            Err(e) => {
                warn!(ddl = %func_ddl, error = %e, "Failed to create function, continuing");
            }
        }
    }
    info!(created = created, total = manifest.functions.len(), "Function creation phase complete");
    Ok(())
}
```

This is the exact template for `restore_named_collections()`: sequential DDL execution, non-fatal failures logged as warnings, ON CLUSTER support.

### 4. CLI Flag Pass-Through Pattern (main.rs)

Current pattern for unimplemented flags:
```rust
if rbac {
    warn!("--rbac flag is not yet implemented, ignoring");
}
```

Implementation: Remove the warn, pass the flag through to `backup::create()` and `restore::restore()`.

### 5. ChClient Query Pattern (clickhouse/client.rs)

Existing patterns for system table queries:
- `list_tables()` (line 262): SELECT from system.tables, deserialize via `clickhouse::Row`
- `get_macros()` (line 421): SELECT from system.macros, returns `HashMap<String, String>`
- `query_table_dependencies()` (line 660): SELECT from system.tables with special columns

New queries for RBAC/named-collections follow the same pattern: define a `#[derive(clickhouse::Row, serde::Deserialize)]` struct, SELECT from the system table, `fetch_all`, return `Result<Vec<T>>`.

### 6. S3 Upload/Download for Simple Files

For RBAC `.jsonl` files and config files (small, no compression needed):
- Upload: `s3.put_object(key, body_vec_u8)` -- existing method at storage/s3.rs:160
- Download: `s3.get_object(key) -> Vec<u8>` -- existing method at storage/s3.rs:223
- List: `s3.list_objects(prefix) -> Vec<S3Object>` -- existing method at storage/s3.rs:314

No tar+lz4 compression needed for these files.

### 7. Filesystem I/O with spawn_blocking

All sync filesystem operations use `tokio::task::spawn_blocking`:
```rust
let result = tokio::task::spawn_blocking(move || {
    // sync I/O here (walkdir, tar, fs::copy, etc.)
}).await.context("spawn_blocking panicked")?;
```

Config file copy and RBAC file operations should follow this pattern.

### 8. Chown Pattern (restore/attach.rs)

```rust
let (ch_uid, ch_gid) = detect_clickhouse_ownership(Path::new(data_path))?;
// Then for each file:
nix::unistd::chown(path, ch_uid.map(Uid::from_raw), ch_gid.map(Gid::from_raw))?;
```

Reuse for RBAC access file chown during restore.

### 9. Manifest Field Existing State

Already defined in manifest.rs (no changes needed for basic types):
- `functions: Vec<String>` -- DDL strings, `skip_serializing_if = "Vec::is_empty"`
- `named_collections: Vec<String>` -- DDL strings, same pattern
- `rbac: Option<RbacInfo>` -- `skip_serializing_if = "Option::is_none"`
- `RbacInfo { path: String }` -- S3 path prefix for RBAC files

### 10. Design Doc RBAC Backup Step (Section 3.4, Step 4)

```
4. Collect RBAC and config objects (if --rbac / --configs flags or always-on config):
   - RBAC: query system.users, system.roles, system.row_policies, system.settings_profiles,
     system.quotas -> serialize to access/*.jsonl files in backup directory
   - Configs: copy CH config files from config_dir to backup directory
   - Named Collections: query system.named_collections -> serialize to backup
   Controlled by: rbac_backup_always, config_backup_always, named_collections_backup_always
```

### 11. Design Doc RBAC Restore Step (Section 5.6)

```
Phase 4: Functions, Named Collections, RBAC
- CREATE FUNCTION ...
- CREATE NAMED COLLECTION ... (supports local and keeper storage types)
- RBAC: restore .jsonl files from access/ directory to ClickHouse's access_data_path
  - Create need_rebuild_lists.mark file to trigger RBAC rebuild on restart
  - Remove stale *.list files
  - Handle replicated user directories via ZooKeeper if configured
  - Chown all access files to ClickHouse user
  - Execute restart_command (default: exec:systemctl restart clickhouse-server)
    to apply RBAC changes. Multiple commands separated by ;. Prefixes:
    exec: runs a shell command, sql: executes a ClickHouse query.
    All errors are logged and ignored (best-effort restart).
- Config files: copy restored configs to config_dir, then execute restart_command
- rbac_resolve_conflicts: when a user/role already exists:
  - "recreate" (default): DROP + CREATE
  - "ignore": skip, log warning
  - "fail": error, abort restore
```
