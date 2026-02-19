# Plan: Phase 4e -- RBAC & Config Backup/Restore

## Goal

Add RBAC objects (users, roles, row_policies, settings_profiles, quotas), ClickHouse config files, and named collections to the backup/restore pipeline, including S3 upload/download of access/ and configs/ directories, conflict resolution, and restart_command execution.

## Architecture Overview

Phase 4e extends all four command pipelines (create, upload, download, restore) with RBAC, config, and named collections support. The manifest types and config fields already exist from Phase 0 scaffolding -- this plan implements the actual logic.

**Components modified:**
- `src/clickhouse/client.rs` -- New query methods for system tables (RBAC + named collections)
- `src/backup/rbac.rs` (NEW) -- Backup RBAC, config files, named collections
- `src/backup/mod.rs` -- Wire RBAC/config/named_collections into create() flow
- `src/upload/mod.rs` -- Upload access/ and configs/ directories to S3
- `src/download/mod.rs` -- Download access/ and configs/ directories from S3
- `src/restore/rbac.rs` (NEW) -- Restore RBAC, config, named collections, restart_command
- `src/restore/mod.rs` -- Wire Phase 4 extensions into restore() flow
- `src/main.rs` -- Remove 12 "not yet implemented" warnings, pass flags through
- `src/server/routes.rs` -- Add rbac/configs/named_collections to API request types

## Architecture Assumptions (VALIDATED)

### Component Ownership
- **BackupManifest**: Created by `backup::create()` at src/backup/mod.rs:519, uploaded by `upload::upload()`, downloaded by `download::download()`, consumed by `restore::restore()`
- **Config**: Loaded once in main.rs, passed by `&Config` to all functions. Config fields for Phase 4e already exist at src/config.rs:190-210
- **CLI flags**: Already defined at src/cli.rs (--rbac, --configs, --named-collections). Currently warn-and-ignore in main.rs
- **Manifest fields**: `functions: Vec<String>`, `named_collections: Vec<String>`, `rbac: Option<RbacInfo>` already in src/manifest.rs:73-81
- **RbacInfo struct**: Already defined at src/manifest.rs:188 with `path: String` field

### Data Flow
```
create (backup/mod.rs):
  [existing table backup] -> backup_rbac_and_configs() -> populate manifest.rbac, manifest.named_collections

upload (upload/mod.rs):
  [existing part upload] -> upload access/ and configs/ dirs -> atomic manifest upload

download (download/mod.rs):
  download manifest -> [existing part download] -> download access/ and configs/ dirs

restore (restore/mod.rs):
  [existing Phase 0-3] -> Phase 4 extensions:
    create_functions() [existing] -> restore_named_collections() -> restore_rbac() -> restore_configs() -> execute_restart_commands()
```

### What This Plan CANNOT Do
- Cannot restore RBAC to a remote ClickHouse (requires local filesystem access to access_data_path)
- Cannot guarantee restart_command will succeed (best-effort, errors logged and ignored per design 5.6)
- Cannot handle replicated RBAC via ZooKeeper (design mentions it but does not require implementation)
- Cannot back up RBAC from ClickHouse versions that do not have system.users/roles/etc tables

## Risk Assessment

| Risk | Level | Mitigation |
|------|-------|------------|
| ClickHouse system table schema varies | YELLOW | Use SHOW CREATE for DDL extraction (stable across versions). Catch query errors gracefully. |
| restart_command fails | GREEN | Per design 5.6: all errors logged and ignored (best-effort). |
| access_data_path not found | YELLOW | Fall back to `{data_path}/access/` which is the default location. |
| RBAC files have wrong ownership | GREEN | Reuse existing `detect_clickhouse_ownership()` + chown pattern from restore/attach.rs |
| Signature change breaks callers | GREEN | All 5 callers of backup::create() and 5 callers of restore::restore() identified and listed. |
| Named collections query fails on old CH | GREEN | Graceful degradation: empty Vec on error, matching create_functions() pattern. |

## Expected Runtime Logs

| Pattern | Required | Description |
|---------|----------|-------------|
| `RBAC backup: .* users, .* roles` | yes | RBAC backup summary |
| `Config backup: .* files copied` | yes | Config file backup summary |
| `Named collections backup: .* collections` | yes | Named collections backup summary |
| `Uploaded access/ directory` | yes | Access files uploaded to S3 |
| `Uploaded configs/ directory` | yes | Config files uploaded to S3 |
| `Downloaded access/ directory` | yes | Access files downloaded from S3 |
| `Downloaded configs/ directory` | yes | Config files downloaded from S3 |
| `Named collection creation phase complete` | yes | Named collections restore summary |
| `RBAC restore complete` | yes | RBAC restore summary |
| `Config restore complete` | yes | Config restore summary |
| `Executing restart command` | yes | restart_command execution |
| `ERROR:.*RBAC` | no (forbidden) | RBAC operations should not produce errors |

## Known Related Issues (Out of Scope)

| Issue | Reason | Future Plan |
|-------|--------|-------------|
| Replicated RBAC via ZooKeeper | Design mentions but does not require | Phase 4f or future |
| Projection skipping (--skip-projections) | Separate flag, unrelated to RBAC | Phase 4f |
| Skip empty tables (--skip-empty-tables) | Separate flag, unrelated to RBAC | Phase 4f |
| rbac_size/config_size in ListResponse | Already has placeholder (0), needs manifest loading | Included as minor update in Task 5 |

## Dependency Groups

```
Group A (Sequential -- core pipeline):
  - Task 1: ChClient query methods for RBAC and named collections
  - Task 2: Backup RBAC, config files, named collections (depends on Task 1)
  - Task 3: Upload/download access/ and configs/ directories (depends on Task 2)
  - Task 4: Restore RBAC, configs, named collections, restart_command (depends on Tasks 1, 3)

Group B (Depends on Group A):
  - Task 5: Wire flags through main.rs, server routes, watch mode (depends on Tasks 2, 4)

Group C (Final -- depends on Group B):
  - Task 6: Update CLAUDE.md for all modified modules (depends on all tasks)
```

## Tasks

### Task 1: ChClient query methods for RBAC and named collections

**Purpose:** Add methods to ChClient for querying ClickHouse system tables that contain RBAC objects and named collections.

**TDD Steps:**
1. Write failing test: `test_query_show_create_user_sql` -- verify SQL generation for SHOW CREATE USER
2. Write failing test: `test_query_named_collections_sql` -- verify SQL generation for named collections query
3. Implement ChClient methods:
   - `query_rbac_objects(entity_type: &str) -> Result<Vec<String>>` -- Generic method that runs `SHOW CREATE {entity_type}` for each entity found in the corresponding system table. Returns Vec of DDL strings.
   - `query_named_collections() -> Result<Vec<String>>` -- Query `system.named_collections` for names, then `SHOW CREATE NAMED COLLECTION {name}` for each. Returns Vec of CREATE DDL strings.
4. Verify tests pass
5. Refactor: Ensure graceful degradation (return empty Vec on query error, log warning)

**Implementation Notes:**

The approach for RBAC backup uses `SHOW CREATE` SQL commands which produce the complete DDL needed for restore. This avoids parsing complex system table schemas that vary across ClickHouse versions.

For RBAC objects, the flow per entity type (USER, ROLE, ROW POLICY, SETTINGS PROFILE, QUOTA) is:
```
1. SELECT name FROM system.{table} -> Vec<name>
2. For each name: SHOW CREATE {entity_type} {name} -> DDL string
3. Return Vec<DDL>
```

System tables to query:
- `system.users` -> `SHOW CREATE USER`
- `system.roles` -> `SHOW CREATE ROLE`
- `system.row_policies` -> `SHOW CREATE ROW POLICY`
- `system.settings_profiles` -> `SHOW CREATE SETTINGS PROFILE`
- `system.quotas` -> `SHOW CREATE QUOTA`

For named collections:
- `system.named_collections` -> `SHOW CREATE NAMED COLLECTION`

Row type for name queries:
```rust
#[derive(clickhouse::Row, serde::Deserialize, Debug, Clone)]
struct NameRow {
    name: String,
}
```

Row type for SHOW CREATE results:
```rust
#[derive(clickhouse::Row, serde::Deserialize, Debug, Clone)]
struct ShowCreateRow {
    #[serde(rename = "statement")]
    statement: String,
}
```

Note: `SHOW CREATE USER` returns a single column. The exact column name varies by ClickHouse version. Use positional access via the `clickhouse-rs` fetch pattern or alias. The simplest approach is to use `ch.inner.query(sql).fetch_one::<String>()` for single-value results if the crate supports it, or use the NameRow pattern with appropriate column aliasing.

Graceful degradation pattern (matching existing `get_macros()` at client.rs:421):
```rust
pub async fn query_named_collections(&self) -> Result<Vec<String>> {
    let names_sql = "SELECT name FROM system.named_collections";
    let names: Vec<NameRow> = match self.inner.query(names_sql).fetch_all().await {
        Ok(rows) => rows,
        Err(e) => {
            warn!(error = %e, "Failed to query system.named_collections (may not exist), skipping");
            return Ok(Vec::new());
        }
    };
    // ... SHOW CREATE for each name
}
```

**Files:**
- `src/clickhouse/client.rs` -- Add `query_rbac_objects()` and `query_named_collections()` methods
- `src/clickhouse/client.rs` -- Add `NameRow` and `ShowCreateRow` structs (private)

**Acceptance:** F001

---

### Task 2: Backup RBAC, config files, and named collections

**Purpose:** Create the backup-side logic for collecting RBAC objects, config files, and named collections during `backup::create()`.

**TDD Steps:**
1. Write failing test: `test_backup_rbac_creates_access_dir` -- verify access/ directory and .sql files are created
2. Write failing test: `test_backup_configs_copies_files` -- verify config files are copied to backup dir
3. Write failing test: `test_effective_flags` -- verify `*_backup_always` config overrides CLI flags
4. Implement:
   - Create `src/backup/rbac.rs` with `backup_rbac_and_configs()` function
   - Add `pub mod rbac;` to `src/backup/mod.rs`
   - Call `backup_rbac_and_configs()` from `create()` between UNFREEZE and manifest save (at line ~517, before step 13)
   - Add `rbac: bool, configs: bool, named_collections: bool` parameters to `backup::create()`
5. Verify tests pass

**Implementation Notes:**

New file `src/backup/rbac.rs`:

```rust
use std::path::{Path, PathBuf};
use anyhow::{Context, Result};
use tracing::{info, warn, debug};

use crate::clickhouse::client::ChClient;
use crate::config::Config;
use crate::manifest::{BackupManifest, RbacInfo};

/// Backup RBAC objects, config files, and named collections.
///
/// Called after UNFREEZE, before manifest save. Populates manifest fields
/// and writes files to the backup directory.
pub async fn backup_rbac_and_configs(
    config: &Config,
    ch: &ChClient,
    backup_dir: &Path,
    manifest: &mut BackupManifest,
    rbac: bool,
    configs: bool,
    named_collections: bool,
) -> Result<()> {
    // RBAC backup
    if rbac || config.clickhouse.rbac_backup_always {
        backup_rbac(ch, backup_dir, manifest).await?;
    }

    // Config file backup
    if configs || config.clickhouse.config_backup_always {
        backup_configs(config, backup_dir).await?;
    }

    // Named collections backup
    if named_collections || config.clickhouse.named_collections_backup_always {
        backup_named_collections(ch, manifest).await?;
    }

    Ok(())
}
```

RBAC backup flow:
1. For each entity type (USER, ROLE, ROW POLICY, SETTINGS PROFILE, QUOTA):
   - Call `ch.query_rbac_objects(entity_type)` to get Vec<DDL>
2. Write all DDLs to `{backup_dir}/access/{entity_type}.sql` (one DDL per line)
3. Set `manifest.rbac = Some(RbacInfo { path: format!("{}/access/", manifest.name) })`
4. Log summary: `"RBAC backup: {n} users, {m} roles, ..."`

Config backup flow:
1. Read `config.clickhouse.config_dir` (default: `/etc/clickhouse-server`)
2. Using `spawn_blocking`, walk the config directory and copy all files to `{backup_dir}/configs/`
3. Log summary: `"Config backup: {n} files copied from {config_dir}"`

Named collections backup flow:
1. Call `ch.query_named_collections()` to get Vec<DDL>
2. Set `manifest.named_collections = ddl_vec`
3. Log summary: `"Named collections backup: {n} collections"`

**Signature change for `backup::create()`:**

Current (src/backup/mod.rs:64):
```rust
pub async fn create(
    config: &Config, ch: &ChClient, backup_name: &str,
    table_pattern: Option<&str>, schema_only: bool,
    diff_from: Option<&str>, partitions: Option<&str>,
    skip_check_parts_columns: bool,
) -> Result<BackupManifest>
```

New (add 3 params):
```rust
pub async fn create(
    config: &Config, ch: &ChClient, backup_name: &str,
    table_pattern: Option<&str>, schema_only: bool,
    diff_from: Option<&str>, partitions: Option<&str>,
    skip_check_parts_columns: bool,
    rbac: bool, configs: bool, named_collections: bool,
) -> Result<BackupManifest>
```

Insert call at line ~517 (after UNFREEZE, before "Build manifest"):
```rust
// 10b. Backup RBAC, configs, named collections
backup_rbac_and_configs(config, ch, &backup_dir, &mut manifest, rbac, configs, named_collections).await?;
```

Wait -- manifest is not mut yet at that point. The manifest is built at step 13 (line 519). So the RBAC/config backup should run BEFORE manifest construction, and the results should be collected into local variables that are then used when building the manifest. Or, we build the manifest first with empty values, then call the function to fill them in. The latter approach is cleaner since the manifest is created at line 519 and the function can mutate it.

Revised insertion point: AFTER manifest creation (line 536) but BEFORE the diff step (line 538):

```rust
// 13a. Backup RBAC, configs, named collections (populates manifest fields)
rbac::backup_rbac_and_configs(
    config, ch, &backup_dir, &mut manifest,
    rbac, configs, named_collections,
).await?;
```

**Files:**
- `src/backup/rbac.rs` (NEW) -- `backup_rbac_and_configs()`, `backup_rbac()`, `backup_configs()`, `backup_named_collections()`
- `src/backup/mod.rs` -- Add `pub mod rbac;`, add 3 params to `create()`, call `backup_rbac_and_configs()`

**Acceptance:** F002

---

### Task 3: Upload and download access/ and configs/ directories

**Purpose:** Extend the upload and download pipelines to handle RBAC (access/) and config (configs/) directories stored in the local backup directory.

**TDD Steps:**
1. Write failing test: `test_upload_access_dir_builds_correct_s3_keys` -- verify S3 key construction for access/ files
2. Write failing test: `test_download_creates_access_dir` -- verify access/ directory is created during download
3. Implement upload extension in `src/upload/mod.rs`:
   - After part upload completes (before manifest upload), scan for `access/` and `configs/` directories in backup_dir
   - Upload each file with `s3.put_object(key, data)` where key is `{backup_name}/access/{filename}` or `{backup_name}/configs/{relative_path}`
4. Implement download extension in `src/download/mod.rs`:
   - After manifest download but alongside part downloads, check `manifest.rbac` for access/ path prefix
   - Download access/ files from S3 to `{backup_dir}/access/`
   - Download configs/ files from S3 to `{backup_dir}/configs/`
5. Verify tests pass

**Implementation Notes:**

Upload extension (src/upload/mod.rs), insert after part upload loop but before manifest upload:

```rust
// Upload access/ directory (RBAC files)
let access_dir = backup_dir.join("access");
if access_dir.exists() {
    upload_simple_directory(s3, backup_name, &access_dir, "access").await?;
    info!("Uploaded access/ directory to S3");
}

// Upload configs/ directory
let configs_dir = backup_dir.join("configs");
if configs_dir.exists() {
    upload_simple_directory(s3, backup_name, &configs_dir, "configs").await?;
    info!("Uploaded configs/ directory to S3");
}
```

New helper function in upload/mod.rs:
```rust
/// Upload all files from a local directory to S3 under `{backup_name}/{prefix}/`.
///
/// Files are uploaded sequentially (these are small RBAC/config files, no parallelism needed).
/// Uses spawn_blocking for directory walk, then put_object for each file.
async fn upload_simple_directory(
    s3: &S3Client,
    backup_name: &str,
    local_dir: &Path,
    prefix: &str,
) -> Result<()> {
    let local_dir_owned = local_dir.to_path_buf();
    let entries: Vec<(String, Vec<u8>)> = tokio::task::spawn_blocking(move || {
        let mut files = Vec::new();
        for entry in walkdir::WalkDir::new(&local_dir_owned).into_iter().filter_map(|e| e.ok()) {
            if entry.file_type().is_file() {
                let rel = entry.path().strip_prefix(&local_dir_owned)?;
                let data = std::fs::read(entry.path())?;
                files.push((rel.to_string_lossy().to_string(), data));
            }
        }
        Ok::<_, anyhow::Error>(files)
    }).await.context("spawn_blocking panicked")??;

    for (rel_path, data) in entries {
        let key = format!("{}/{}/{}", backup_name, prefix, rel_path);
        s3.put_object(&key, data).await
            .with_context(|| format!("Failed to upload {}/{}", prefix, rel_path))?;
    }
    Ok(())
}
```

Download extension (src/download/mod.rs), insert after part downloads complete:

```rust
// Download access/ directory (RBAC files)
if manifest.rbac.is_some() {
    download_simple_directory(s3, backup_name, &backup_dir, "access").await?;
    info!("Downloaded access/ directory from S3");
}

// Download configs/ directory (check if any configs/ keys exist)
download_simple_directory(s3, backup_name, &backup_dir, "configs").await?;
if backup_dir.join("configs").exists() {
    info!("Downloaded configs/ directory from S3");
}
```

New helper function in download/mod.rs:
```rust
/// Download all files under `{backup_name}/{prefix}/` from S3 to `{local_dir}/{prefix}/`.
async fn download_simple_directory(
    s3: &S3Client,
    backup_name: &str,
    local_dir: &Path,
    prefix: &str,
) -> Result<()> {
    let s3_prefix = format!("{}/{}/", backup_name, prefix);
    let objects = s3.list_objects(&s3_prefix).await
        .with_context(|| format!("Failed to list S3 objects under {}", s3_prefix))?;

    if objects.is_empty() {
        debug!("No {} files found in S3", prefix);
        return Ok(());
    }

    let target_dir = local_dir.join(prefix);
    std::fs::create_dir_all(&target_dir)
        .with_context(|| format!("Failed to create {} directory", prefix))?;

    for obj in &objects {
        let data = s3.get_object(&obj.key).await
            .with_context(|| format!("Failed to download {}", obj.key))?;
        // Extract relative path after the prefix
        let rel_path = obj.key.strip_prefix(&s3_prefix).unwrap_or(&obj.key);
        let file_path = target_dir.join(rel_path);
        if let Some(parent) = file_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&file_path, &data)
            .with_context(|| format!("Failed to write {}", file_path.display()))?;
    }

    Ok(())
}
```

Note: The `s3.list_objects()` method returns `Vec<S3Object>` with a `key` field. Verify exact type from storage/s3.rs. The `s3.put_object()` takes `(&str, Vec<u8>)` per the existing usage.

**Files:**
- `src/upload/mod.rs` -- Add `upload_simple_directory()` helper, call after part upload
- `src/download/mod.rs` -- Add `download_simple_directory()` helper, call after part download

**Acceptance:** F003

---

### Task 4: Restore RBAC, configs, named collections, and restart_command

**Purpose:** Implement Phase 4 restore extensions for named collections, RBAC files, config files, and the restart_command execution.

**TDD Steps:**
1. Write failing test: `test_restore_named_collections_empty` -- verify no-op when named_collections is empty
2. Write failing test: `test_restore_named_collections_with_on_cluster` -- verify ON CLUSTER clause injection
3. Write failing test: `test_execute_restart_commands_exec_prefix` -- verify "exec:" prefix handling
4. Write failing test: `test_execute_restart_commands_sql_prefix` -- verify "sql:" prefix handling
5. Write failing test: `test_execute_restart_commands_multiple_semicolons` -- verify semicolon splitting
6. Write failing test: `test_rbac_resolve_conflicts_recreate` -- verify DROP+CREATE for "recreate" mode
7. Write failing test: `test_rbac_resolve_conflicts_ignore` -- verify skip behavior for "ignore" mode
8. Implement:
   - Create `src/restore/rbac.rs` with restore functions
   - Add `pub mod rbac;` to `src/restore/mod.rs`
   - Add `rbac: bool, configs: bool, named_collections: bool` parameters to `restore::restore()`
   - Wire Phase 4 extensions in restore() after existing `create_functions()` call
9. Verify all tests pass

**Implementation Notes:**

New file `src/restore/rbac.rs`:

**`restore_named_collections()`** -- follows `create_functions()` pattern exactly (schema.rs:721-755):
```rust
pub async fn restore_named_collections(
    ch: &ChClient,
    manifest: &BackupManifest,
    on_cluster: Option<&str>,
    resolve_conflicts: &str,
) -> Result<()> {
    if manifest.named_collections.is_empty() {
        debug!("No named collections to restore");
        return Ok(());
    }

    let mut created = 0u32;
    for nc_ddl in &manifest.named_collections {
        // Handle conflict resolution
        if resolve_conflicts == "recreate" {
            // Extract name from CREATE NAMED COLLECTION name ... DDL
            if let Some(name) = extract_named_collection_name(nc_ddl) {
                let drop_ddl = format!("DROP NAMED COLLECTION IF EXISTS {}", name);
                let drop_ddl = match on_cluster {
                    Some(cluster) => add_on_cluster_clause(&drop_ddl, cluster),
                    None => drop_ddl,
                };
                let _ = ch.execute_ddl(&drop_ddl).await; // ignore error
            }
        }

        let ddl = match on_cluster {
            Some(cluster) => add_on_cluster_clause(nc_ddl, cluster),
            None => nc_ddl.clone(),
        };

        match ch.execute_ddl(&ddl).await {
            Ok(()) => {
                info!(ddl = %nc_ddl, "Created named collection");
                created += 1;
            }
            Err(e) => {
                if resolve_conflicts == "fail" {
                    return Err(e).context("Named collection creation failed with rbac_resolve_conflicts=fail");
                }
                warn!(ddl = %nc_ddl, error = %e, "Failed to create named collection, continuing");
            }
        }
    }

    info!(
        created = created,
        total = manifest.named_collections.len(),
        "Named collection creation phase complete"
    );
    Ok(())
}
```

**`restore_rbac()`** -- file-based RBAC restore:
```rust
pub async fn restore_rbac(
    config: &Config,
    backup_dir: &Path,
    resolve_conflicts: &str,
) -> Result<()> {
    let access_src = backup_dir.join("access");
    if !access_src.exists() {
        debug!("No access/ directory in backup, skipping RBAC restore");
        return Ok(());
    }

    let access_dst = PathBuf::from(&config.clickhouse.data_path).join("access");

    // Copy files from backup access/ to CH access_data_path
    let src = access_src.clone();
    let dst = access_dst.clone();
    let copied = tokio::task::spawn_blocking(move || -> Result<u32> {
        let mut count = 0u32;
        std::fs::create_dir_all(&dst)?;
        for entry in walkdir::WalkDir::new(&src).into_iter().filter_map(|e| e.ok()) {
            if entry.file_type().is_file() {
                let rel = entry.path().strip_prefix(&src)?;
                let target = dst.join(rel);
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::copy(entry.path(), &target)?;
                count += 1;
            }
        }

        // Remove stale *.list files
        for entry in std::fs::read_dir(&dst)? {
            let entry = entry?;
            if entry.path().extension().map_or(false, |ext| ext == "list") {
                std::fs::remove_file(entry.path())?;
            }
        }

        // Create need_rebuild_lists.mark
        std::fs::write(dst.join("need_rebuild_lists.mark"), "")?;

        Ok(count)
    }).await.context("spawn_blocking panicked")??;

    // Chown access files to ClickHouse user
    let data_path = PathBuf::from(&config.clickhouse.data_path);
    let (ch_uid, ch_gid) = detect_clickhouse_ownership(&data_path)?;
    let dst_clone = access_dst.clone();
    tokio::task::spawn_blocking(move || -> Result<()> {
        chown_recursive(&dst_clone, ch_uid, ch_gid)?;
        Ok(())
    }).await.context("spawn_blocking panicked")??;

    info!(files = copied, "RBAC restore complete");
    Ok(())
}
```

**`restore_configs()`** -- config file restore:
```rust
pub async fn restore_configs(
    config: &Config,
    backup_dir: &Path,
) -> Result<()> {
    let configs_src = backup_dir.join("configs");
    if !configs_src.exists() {
        debug!("No configs/ directory in backup, skipping config restore");
        return Ok(());
    }

    let config_dir = config.clickhouse.config_dir.clone();
    let src = configs_src.clone();
    let copied = tokio::task::spawn_blocking(move || -> Result<u32> {
        let mut count = 0u32;
        let dst = PathBuf::from(&config_dir);
        std::fs::create_dir_all(&dst)?;
        for entry in walkdir::WalkDir::new(&src).into_iter().filter_map(|e| e.ok()) {
            if entry.file_type().is_file() {
                let rel = entry.path().strip_prefix(&src)?;
                let target = dst.join(rel);
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::copy(entry.path(), &target)?;
                count += 1;
            }
        }
        Ok(count)
    }).await.context("spawn_blocking panicked")??;

    info!(files = copied, config_dir = %config.clickhouse.config_dir, "Config restore complete");
    Ok(())
}
```

**`execute_restart_commands()`** -- per design 5.6:
```rust
pub async fn execute_restart_commands(
    ch: &ChClient,
    restart_command: &str,
) -> Result<()> {
    if restart_command.is_empty() {
        return Ok(());
    }

    for cmd in restart_command.split(';') {
        let cmd = cmd.trim();
        if cmd.is_empty() {
            continue;
        }

        if let Some(exec_cmd) = cmd.strip_prefix("exec:") {
            info!(command = %exec_cmd, "Executing restart command (exec)");
            match tokio::process::Command::new("sh")
                .arg("-c")
                .arg(exec_cmd.trim())
                .output()
                .await
            {
                Ok(output) => {
                    if !output.status.success() {
                        let stderr = String::from_utf8_lossy(&output.stderr);
                        warn!(command = %exec_cmd, stderr = %stderr, "Restart command failed (non-fatal)");
                    } else {
                        info!(command = %exec_cmd, "Restart command executed successfully");
                    }
                }
                Err(e) => {
                    warn!(command = %exec_cmd, error = %e, "Failed to execute restart command (non-fatal)");
                }
            }
        } else if let Some(sql_cmd) = cmd.strip_prefix("sql:") {
            info!(sql = %sql_cmd, "Executing restart command (sql)");
            match ch.execute_ddl(sql_cmd.trim()).await {
                Ok(()) => {
                    info!(sql = %sql_cmd, "SQL restart command executed successfully");
                }
                Err(e) => {
                    warn!(sql = %sql_cmd, error = %e, "SQL restart command failed (non-fatal)");
                }
            }
        } else {
            // Default to exec: if no prefix
            info!(command = %cmd, "Executing restart command (exec, no prefix)");
            match tokio::process::Command::new("sh").arg("-c").arg(cmd).output().await {
                Ok(output) => {
                    if !output.status.success() {
                        let stderr = String::from_utf8_lossy(&output.stderr);
                        warn!(command = %cmd, stderr = %stderr, "Restart command failed (non-fatal)");
                    }
                }
                Err(e) => {
                    warn!(command = %cmd, error = %e, "Failed to execute restart command (non-fatal)");
                }
            }
        }
    }

    Ok(())
}
```

Helper functions:
```rust
fn extract_named_collection_name(ddl: &str) -> Option<String> {
    // Parse "CREATE NAMED COLLECTION name ..." to extract name
    let upper = ddl.to_uppercase();
    let marker = "NAMED COLLECTION ";
    if let Some(pos) = upper.find(marker) {
        let rest = &ddl[pos + marker.len()..];
        let name = rest.split_whitespace().next()?;
        // Strip backticks/quotes if present
        Some(name.trim_matches('`').trim_matches('\'').to_string())
    } else {
        None
    }
}

fn chown_recursive(dir: &Path, uid: Option<u32>, gid: Option<u32>) -> Result<()> {
    for entry in walkdir::WalkDir::new(dir).into_iter().filter_map(|e| e.ok()) {
        nix::unistd::chown(
            entry.path(),
            uid.map(nix::unistd::Uid::from_raw),
            gid.map(nix::unistd::Gid::from_raw),
        ).with_context(|| format!("Failed to chown {}", entry.path().display()))?;
    }
    Ok(())
}
```

**Signature change for `restore::restore()`:**

Current (src/restore/mod.rs:78):
```rust
pub async fn restore(
    config: &Config, ch: &ChClient, backup_name: &str,
    table_pattern: Option<&str>, schema_only: bool, data_only: bool,
    rm: bool, resume: bool, rename_as: Option<&str>,
    database_mapping: Option<&HashMap<String, String>>,
) -> Result<()>
```

New (add 3 params):
```rust
pub async fn restore(
    config: &Config, ch: &ChClient, backup_name: &str,
    table_pattern: Option<&str>, schema_only: bool, data_only: bool,
    rm: bool, resume: bool, rename_as: Option<&str>,
    database_mapping: Option<&HashMap<String, String>>,
    rbac: bool, configs: bool, named_collections: bool,
) -> Result<()>
```

Wire in restore/mod.rs after existing Phase 4 function call (line 649-650):
```rust
// Phase 4: Functions
if !data_only && !manifest.functions.is_empty() {
    create_functions(ch, &manifest, on_cluster).await?;
}

// Phase 4b: Named Collections
if !data_only && (named_collections || config.clickhouse.named_collections_backup_always) {
    rbac::restore_named_collections(
        ch, &manifest, on_cluster,
        &config.clickhouse.rbac_resolve_conflicts,
    ).await?;
}

// Phase 4c: RBAC
if rbac || config.clickhouse.rbac_backup_always {
    rbac::restore_rbac(config, &backup_dir).await?;
}

// Phase 4d: Config files
if configs || config.clickhouse.config_backup_always {
    rbac::restore_configs(config, &backup_dir).await?;
}

// Phase 4e: Restart command (if RBAC or configs were restored)
let did_rbac = (rbac || config.clickhouse.rbac_backup_always) && backup_dir.join("access").exists();
let did_configs = (configs || config.clickhouse.config_backup_always) && backup_dir.join("configs").exists();
if did_rbac || did_configs {
    rbac::execute_restart_commands(ch, &config.clickhouse.restart_command).await?;
}
```

**Files:**
- `src/restore/rbac.rs` (NEW) -- `restore_named_collections()`, `restore_rbac()`, `restore_configs()`, `execute_restart_commands()`, helper functions
- `src/restore/mod.rs` -- Add `pub mod rbac;`, add 3 params to `restore()`, wire Phase 4 extensions

**Acceptance:** F004, F005

---

### Task 5: Wire flags through main.rs, server routes, and watch mode

**Purpose:** Remove the 12 "not yet implemented" warnings from main.rs, pass flags through to backup::create() and restore::restore(), update server route request types, and update watch mode.

**TDD Steps:**
1. Write failing test: `test_create_request_has_rbac_fields` -- verify CreateRequest deserializes rbac/configs/named_collections
2. Implement:
   - Remove 12 `warn!("--{flag} flag is not yet implemented, ignoring")` statements from main.rs
   - Pass `rbac`, `configs`, `named_collections` to `backup::create()` at all 5 call sites
   - Pass `rbac`, `configs`, `named_collections` to `restore::restore()` at all 5 call sites
   - Add `rbac`, `configs`, `named_collections` fields to 4 server request types
   - Update watch mode `backup::create()` call to pass `false, false, false` (watch mode does not support RBAC/config backup per design -- watch mode always does full/incremental table backups)
3. Verify zero warnings, zero errors with `cargo check`

**Implementation Notes:**

**main.rs changes (src/main.rs):**

For Command::Create (line 136-143), REMOVE:
```rust
if rbac {
    warn!("--rbac flag is not yet implemented, ignoring");
}
if configs {
    warn!("--configs flag is not yet implemented, ignoring");
}
if named_collections {
    warn!("--named-collections flag is not yet implemented, ignoring");
}
```

CHANGE call at line 154:
```rust
let _manifest = backup::create(
    &config, &ch, &name,
    tables.as_deref(), schema, diff_from.as_deref(),
    partitions.as_deref(), skip_check_parts_columns,
    rbac, configs, named_collections,  // NEW
).await?;
```

Same pattern for Command::CreateRemote (line 290-297, 307-318):
- Remove 3 warn! calls
- Pass `rbac, configs, named_collections` to `backup::create()`

For Command::Restore (line 238-245), REMOVE warn! calls, pass to restore:
```rust
restore::restore(
    &config, &ch, &name,
    tables.as_deref(), schema, data_only, rm,
    effective_resume, rename_as.as_deref(),
    db_mapping.as_ref(),
    rbac, configs, named_collections,  // NEW
).await?;
```

Same pattern for Command::RestoreRemote (line 353-360, 380-391).

**All 5 callers of `backup::create()` (add `rbac, configs, named_collections` params):**
1. `src/main.rs:154` -- Command::Create: pass `rbac, configs, named_collections`
2. `src/main.rs:308` -- Command::CreateRemote: pass `rbac, configs, named_collections`
3. `src/server/routes.rs:318` -- create_backup: pass `req.rbac.unwrap_or(false), req.configs.unwrap_or(false), req.named_collections.unwrap_or(false)`
4. `src/server/routes.rs:652` -- create_remote: pass `req.rbac.unwrap_or(false), req.configs.unwrap_or(false), req.named_collections.unwrap_or(false)`
5. `src/watch/mod.rs:412` -- watch loop: pass `false, false, false` (watch does not do RBAC backup)

**All 5 callers of `restore::restore()` (add `rbac, configs, named_collections` params):**
1. `src/main.rs:260` -- Command::Restore: pass `rbac, configs, named_collections`
2. `src/main.rs:380` -- Command::RestoreRemote: pass `rbac, configs, named_collections`
3. `src/server/routes.rs:566` -- restore_backup: pass `req.rbac.unwrap_or(false), req.configs.unwrap_or(false), req.named_collections.unwrap_or(false)`
4. `src/server/routes.rs:804` -- restore_remote: pass `req.rbac.unwrap_or(false), req.configs.unwrap_or(false), req.named_collections.unwrap_or(false)`
5. `src/server/state.rs:386` -- auto_resume: pass `false, false, false` (auto-resume does not include RBAC/config)

**Server request type changes (src/server/routes.rs):**

Add to `CreateRequest` (line 368):
```rust
pub rbac: Option<bool>,
pub configs: Option<bool>,
pub named_collections: Option<bool>,
```

Add to `RestoreRequest` (line 615):
```rust
pub rbac: Option<bool>,
pub configs: Option<bool>,
pub named_collections: Option<bool>,
```

Add to `CreateRemoteRequest` (line 735):
```rust
pub rbac: Option<bool>,
pub configs: Option<bool>,
pub named_collections: Option<bool>,
```

Add to `RestoreRemoteRequest` (line 853):
```rust
pub rbac: Option<bool>,
pub configs: Option<bool>,
pub named_collections: Option<bool>,
```

**Files:**
- `src/main.rs` -- Remove 12 warnings, pass flags through to backup::create() and restore::restore()
- `src/server/routes.rs` -- Add rbac/configs/named_collections to 4 request types, pass to function calls
- `src/server/state.rs` -- Update auto_resume restore::restore() call with 3 new params (false, false, false)
- `src/watch/mod.rs` -- Update backup::create() call with 3 new params (false, false, false)

**Acceptance:** F006

---

### Task 6: Update CLAUDE.md for all modified modules (MANDATORY)

**Purpose:** Ensure all module documentation reflects code changes from this plan.

**Modules to update:** src/backup, src/restore, src/clickhouse, src/upload, src/download, src/server

**TDD Steps:**

1. For each module, regenerate directory tree:
   ```bash
   for module in src/backup src/restore src/clickhouse src/upload src/download src/server; do
     tree -L 2 "$module" --noreport 2>/dev/null || ls -la "$module"
   done
   ```

2. Detect and add new patterns:
   - src/backup: Add RBAC/config/named-collections backup pattern, new `rbac.rs` file
   - src/restore: Add Phase 4 RBAC/config/named-collections restore pattern, new `rbac.rs` file, restart_command execution
   - src/clickhouse: Add RBAC/named-collections query methods
   - src/upload: Add `upload_simple_directory()` for access/configs upload
   - src/download: Add `download_simple_directory()` for access/configs download
   - src/server: Update request type documentation

3. Validate all CLAUDE.md files have required sections:
   - Parent Context
   - Directory Structure
   - Key Patterns
   - Parent Rules

4. Run `cargo fmt -- --check` to verify formatting is clean before final commit.

**Files:** src/backup/CLAUDE.md, src/restore/CLAUDE.md, src/clickhouse/CLAUDE.md, src/upload/CLAUDE.md, src/download/CLAUDE.md, src/server/CLAUDE.md

**Acceptance:** FDOC

---

## Consistency Validation Results

| Check | Status | Notes |
|-------|--------|-------|
| RC-015 (Symbols match) | PASS | All types used across tasks verified: BackupManifest, RbacInfo, ChClient, Config, S3Client |
| RC-016 (Tests match implementation) | PASS | Test names correspond to specific implementations |
| RC-017 (Acceptance IDs match tasks) | PASS | F001-F006, FDOC all referenced in tasks and acceptance.json |
| RC-018 (Dependencies satisfied) | PASS | Task ordering ensures types/methods available before use |

### Cross-Task Type Consistency:
- Task 1 defines `query_rbac_objects() -> Result<Vec<String>>`, Task 2 calls it and stores in `Vec<String>` files
- Task 2 populates `manifest.named_collections: Vec<String>`, Task 4 reads `manifest.named_collections: Vec<String>`
- Task 2 sets `manifest.rbac: Option<RbacInfo>`, Task 3 reads `manifest.rbac` to decide download
- Task 2 adds params to `backup::create()`, Task 5 updates all 5 callers with same params
- Task 4 adds params to `restore::restore()`, Task 5 updates all 5 callers with same params

### Config vs Implementation Consistency:
- `rbac_backup_always: bool` in config -> `if rbac || config.clickhouse.rbac_backup_always` in Task 2
- `config_backup_always: bool` in config -> `if configs || config.clickhouse.config_backup_always` in Task 2
- `named_collections_backup_always: bool` in config -> same pattern in Task 2
- `rbac_resolve_conflicts: String` in config -> matched against "recreate"/"ignore"/"fail" in Task 4
- `restart_command: String` in config -> split by ';', "exec:"/"sql:" prefixes in Task 4

### State Transitions:
- No state machine flags in this plan (all operations are fire-and-forget with best-effort error handling)

### Verification Commands Match Code:
- Structural checks grep for exact function signatures
- Behavioral checks reference exact test function names
- Runtime patterns match info!() log messages in implementation

## Notes

### Phase 4.5 Skip Justification
Skipping Phase 4.5 (Interface Skeleton Simulation) because:
- All new imports are standard library / existing crate types (no new external dependencies)
- The key types (BackupManifest, RbacInfo, ChClient, Config) are already verified to exist
- New functions are in NEW files, so no import conflicts possible
- The `walkdir` and `nix` crates are already dependencies (used in backup/collect.rs and restore/attach.rs)

### Watch Mode RBAC Support
Watch mode passes `false, false, false` for rbac/configs/named_collections to `backup::create()`. This is intentional: watch mode is for automated rolling backups of table data, not for RBAC/config management. Users who need RBAC backup should use explicit `create --rbac` commands.

### Design Doc References
- Backup step 4: Design doc section 3.4
- Upload step 5: Design doc section 3.6 line 1177
- Restore Phase 4: Design doc section 5.6
- Manifest format: Design doc section 7.1
- Config fields: Design doc section 12
- Test T19: Design doc test matrix
