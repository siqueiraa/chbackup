//! RBAC, config file, named collection restore and restart_command execution (Phase 4e).
//!
//! Restores RBAC objects from .jsonl files, config files from backup directory,
//! named collections from manifest DDL, and executes restart commands.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tracing::{debug, info, warn};

use crate::clickhouse::client::ChClient;
use crate::config::Config;
use crate::manifest::BackupManifest;

use super::attach::detect_clickhouse_ownership;
use super::remap::add_on_cluster_clause;

/// RBAC entry parsed from .jsonl files created during backup.
#[derive(serde::Deserialize, Debug)]
struct RbacEntry {
    entity_type: String,
    name: String,
    create_statement: String,
}

/// Restore named collections from manifest DDL.
///
/// Follows `create_functions()` pattern: iterates DDL entries, executes each,
/// logs failures as warnings. Supports ON CLUSTER injection.
pub async fn restore_named_collections(
    ch: &ChClient,
    manifest: &BackupManifest,
    on_cluster: Option<&str>,
) -> Result<()> {
    if manifest.named_collections.is_empty() {
        debug!("No named collections to restore");
        return Ok(());
    }

    let mut created = 0u32;
    for nc_ddl in &manifest.named_collections {
        let ddl = match on_cluster {
            Some(cluster) => add_on_cluster_clause(nc_ddl, cluster),
            None => nc_ddl.clone(),
        };

        match ch.execute_ddl(&ddl).await {
            Ok(()) => {
                debug!(ddl = %nc_ddl, "Created named collection");
                created += 1;
            }
            Err(e) => {
                warn!(
                    ddl = %nc_ddl,
                    error = %e,
                    "Failed to create named collection, continuing"
                );
            }
        }
    }

    info!(
        created = created,
        total = manifest.named_collections.len(),
        "Named collection restore: {} created out of {}",
        created,
        manifest.named_collections.len()
    );
    Ok(())
}

/// Restore RBAC objects from .jsonl files in the backup access/ directory.
///
/// Parses .jsonl files, then executes DDL for each entry using the specified
/// conflict resolution mode:
/// - "recreate": DROP IF EXISTS then CREATE (default)
/// - "ignore": Skip on error (object already exists)
/// - "fail": Return error on any failure
///
/// After restore, creates `need_rebuild_lists.mark` in ClickHouse access/
/// directory and chowns to ClickHouse uid/gid.
pub async fn restore_rbac(
    ch: &ChClient,
    config: &Config,
    backup_dir: &Path,
    resolve_conflicts: &str,
) -> Result<()> {
    let access_src = backup_dir.join("access");
    if !access_src.exists() {
        debug!("No access/ directory in backup, skipping RBAC restore");
        return Ok(());
    }

    // Parse all .jsonl files from access/ directory
    let src = access_src.clone();
    let entries: Vec<RbacEntry> = tokio::task::spawn_blocking(move || -> Result<Vec<RbacEntry>> {
        let mut all = Vec::new();
        for file_entry in std::fs::read_dir(&src)? {
            let file_entry = file_entry?;
            let path = file_entry.path();
            if path.extension().is_some_and(|ext| ext == "jsonl") {
                let contents = std::fs::read_to_string(&path)?;
                for line in contents.lines() {
                    let line = line.trim();
                    if line.is_empty() {
                        continue;
                    }
                    let entry: RbacEntry = serde_json::from_str(line).with_context(|| {
                        format!("Failed to parse RBAC entry from {}", path.display())
                    })?;
                    all.push(entry);
                }
            }
        }
        Ok(all)
    })
    .await
    .context("spawn_blocking panicked during RBAC file parsing")??;

    if entries.is_empty() {
        debug!("No RBAC entries found in access/ directory");
        return Ok(());
    }

    // Execute DDL for each RBAC entry with conflict resolution
    let mut created = 0u32;
    let mut skipped = 0u32;
    for entry in &entries {
        // "recreate" mode: DROP first, then CREATE
        if resolve_conflicts == "recreate" {
            if let Some(ddl) = make_drop_ddl(&entry.entity_type, &entry.name) {
                let _ = ch.execute_ddl(&ddl).await; // ignore error (may not exist)
            }
        }

        match ch.execute_ddl(&entry.create_statement).await {
            Ok(()) => {
                debug!(
                    entity = %entry.entity_type,
                    name = %entry.name,
                    "Restored RBAC object"
                );
                created += 1;
            }
            Err(e) => match resolve_conflicts {
                "fail" => {
                    return Err(e).with_context(|| {
                        format!(
                            "RBAC restore failed for {} '{}' with rbac_resolve_conflicts=fail",
                            entry.entity_type, entry.name
                        )
                    });
                }
                "ignore" => {
                    debug!(
                        entity = %entry.entity_type,
                        name = %entry.name,
                        error = %e,
                        "RBAC object already exists, skipping (ignore mode)"
                    );
                    skipped += 1;
                }
                _ => {
                    // "recreate" shouldn't normally fail after DROP, but handle gracefully
                    warn!(
                        entity = %entry.entity_type,
                        name = %entry.name,
                        error = %e,
                        "Failed to restore RBAC object, continuing"
                    );
                    skipped += 1;
                }
            },
        }
    }

    // Safety measures per design 5.6:
    // Remove stale .list files and create need_rebuild_lists.mark
    let access_dst = PathBuf::from(&config.clickhouse.data_path).join("access");
    if access_dst.exists() {
        let dst = access_dst.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            // Remove stale *.list files
            for entry in std::fs::read_dir(&dst)? {
                let entry = entry?;
                if entry.path().extension().is_some_and(|ext| ext == "list") {
                    std::fs::remove_file(entry.path())?;
                }
            }
            // Create need_rebuild_lists.mark to trigger rebuild on restart
            std::fs::write(dst.join("need_rebuild_lists.mark"), "")?;
            Ok(())
        })
        .await
        .context("spawn_blocking panicked during RBAC cleanup")??;

        // Chown access dir to ClickHouse user
        let data_path = PathBuf::from(&config.clickhouse.data_path);
        let (ch_uid, ch_gid) = detect_clickhouse_ownership(&data_path).unwrap_or_else(|e| {
            warn!(error = %e, "Failed to detect ClickHouse ownership, skipping chown");
            (None, None)
        });
        if ch_uid.is_some() || ch_gid.is_some() {
            let dst_clone = access_dst;
            tokio::task::spawn_blocking(move || -> Result<()> {
                chown_recursive(&dst_clone, ch_uid, ch_gid)?;
                Ok(())
            })
            .await
            .context("spawn_blocking panicked during chown")??;
        }
    }

    info!(
        created = created,
        skipped = skipped,
        total = entries.len(),
        "RBAC restore: {} created, {} skipped",
        created,
        skipped
    );
    Ok(())
}

/// Restore config files from the backup configs/ directory.
///
/// Copies files from `{backup_dir}/configs/` to the ClickHouse config directory
/// (`config.clickhouse.config_dir`), preserving directory structure.
pub async fn restore_configs(config: &Config, backup_dir: &Path) -> Result<()> {
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
        std::fs::create_dir_all(&dst)
            .with_context(|| format!("Failed to create config dir {}", dst.display()))?;
        for entry in walkdir::WalkDir::new(&src)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if entry.file_type().is_file() {
                let rel = entry
                    .path()
                    .strip_prefix(&src)
                    .context("Failed to strip prefix for config file")?;
                let target = dst.join(rel);
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::copy(entry.path(), &target).with_context(|| {
                    format!("Failed to copy config file {}", entry.path().display())
                })?;
                count += 1;
            }
        }
        Ok(count)
    })
    .await
    .context("spawn_blocking panicked during config restore")??;

    info!(
        files = copied,
        config_dir = %config.clickhouse.config_dir,
        "Config restore: {} files copied to {}",
        copied,
        config.clickhouse.config_dir
    );
    Ok(())
}

/// Execute restart commands after RBAC or config restore.
///
/// Commands are semicolon-separated and support two prefixes:
/// - `exec:` -- Execute as a shell command via `sh -c`
/// - `sql:` -- Execute as ClickHouse DDL via `ch.execute_ddl()`
/// - No prefix defaults to `exec:` behavior
///
/// All failures are non-fatal (logged as warnings).
pub async fn execute_restart_commands(ch: &ChClient, restart_command: &str) -> Result<()> {
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
                        warn!(
                            command = %exec_cmd,
                            stderr = %stderr,
                            "Restart command failed (non-fatal)"
                        );
                    } else {
                        info!(command = %exec_cmd, "Restart command executed successfully");
                    }
                }
                Err(e) => {
                    warn!(
                        command = %exec_cmd,
                        error = %e,
                        "Failed to execute restart command (non-fatal)"
                    );
                }
            }
        } else if let Some(sql_cmd) = cmd.strip_prefix("sql:") {
            info!(sql = %sql_cmd, "Executing restart command (sql)");
            match ch.execute_ddl(sql_cmd.trim()).await {
                Ok(()) => {
                    info!(sql = %sql_cmd, "SQL restart command executed successfully");
                }
                Err(e) => {
                    warn!(
                        sql = %sql_cmd,
                        error = %e,
                        "SQL restart command failed (non-fatal)"
                    );
                }
            }
        } else {
            // Default to exec: if no prefix
            info!(command = %cmd, "Executing restart command (exec, no prefix)");
            match tokio::process::Command::new("sh")
                .arg("-c")
                .arg(cmd)
                .output()
                .await
            {
                Ok(output) => {
                    if !output.status.success() {
                        let stderr = String::from_utf8_lossy(&output.stderr);
                        warn!(
                            command = %cmd,
                            stderr = %stderr,
                            "Restart command failed (non-fatal)"
                        );
                    }
                }
                Err(e) => {
                    warn!(
                        command = %cmd,
                        error = %e,
                        "Failed to execute restart command (non-fatal)"
                    );
                }
            }
        }
    }

    Ok(())
}

/// Generate DROP DDL for an RBAC entity type.
fn make_drop_ddl(entity_type: &str, name: &str) -> Option<String> {
    let keyword = match entity_type {
        "user" => "USER",
        "role" => "ROLE",
        "row_policy" => "ROW POLICY",
        "settings_profile" => "SETTINGS PROFILE",
        "quota" => "QUOTA",
        _ => return None,
    };
    Some(format!("DROP {} IF EXISTS `{}`", keyword, name))
}

/// Recursively chown a directory to the given uid/gid.
fn chown_recursive(dir: &Path, uid: Option<u32>, gid: Option<u32>) -> Result<()> {
    for entry in walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        nix::unistd::chown(
            entry.path(),
            uid.map(nix::unistd::Uid::from_raw),
            gid.map(nix::unistd::Gid::from_raw),
        )
        .with_context(|| format!("Failed to chown {}", entry.path().display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_make_drop_ddl_user() {
        let ddl = make_drop_ddl("user", "admin");
        assert_eq!(ddl, Some("DROP USER IF EXISTS `admin`".to_string()));
    }

    #[test]
    fn test_make_drop_ddl_role() {
        let ddl = make_drop_ddl("role", "readonly");
        assert_eq!(ddl, Some("DROP ROLE IF EXISTS `readonly`".to_string()));
    }

    #[test]
    fn test_make_drop_ddl_row_policy() {
        let ddl = make_drop_ddl("row_policy", "my_policy");
        assert_eq!(
            ddl,
            Some("DROP ROW POLICY IF EXISTS `my_policy`".to_string())
        );
    }

    #[test]
    fn test_make_drop_ddl_settings_profile() {
        let ddl = make_drop_ddl("settings_profile", "default");
        assert_eq!(
            ddl,
            Some("DROP SETTINGS PROFILE IF EXISTS `default`".to_string())
        );
    }

    #[test]
    fn test_make_drop_ddl_quota() {
        let ddl = make_drop_ddl("quota", "daily_quota");
        assert_eq!(ddl, Some("DROP QUOTA IF EXISTS `daily_quota`".to_string()));
    }

    #[test]
    fn test_make_drop_ddl_unknown() {
        let ddl = make_drop_ddl("unknown_type", "foo");
        assert_eq!(ddl, None);
    }

    #[test]
    fn test_parse_rbac_jsonl() {
        let jsonl = r#"{"entity_type":"user","name":"admin","create_statement":"CREATE USER admin IDENTIFIED BY 'pass'"}
{"entity_type":"role","name":"readonly","create_statement":"CREATE ROLE readonly"}"#;

        let entries: Vec<RbacEntry> = jsonl
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].entity_type, "user");
        assert_eq!(entries[0].name, "admin");
        assert!(entries[0].create_statement.contains("CREATE USER admin"));
        assert_eq!(entries[1].entity_type, "role");
        assert_eq!(entries[1].name, "readonly");
    }

    #[test]
    fn test_parse_rbac_jsonl_empty_lines() {
        let jsonl = r#"
{"entity_type":"user","name":"test","create_statement":"CREATE USER test"}

"#;

        let entries: Vec<RbacEntry> = jsonl
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "test");
    }

    #[test]
    fn test_execute_restart_commands_parsing() {
        // Verify that the semicolon splitting logic works correctly
        let cmd = "exec:systemctl restart clickhouse-server;sql:SYSTEM RELOAD CONFIG";
        let parts: Vec<&str> = cmd.split(';').map(|s| s.trim()).collect();
        assert_eq!(parts.len(), 2);
        assert!(parts[0].starts_with("exec:"));
        assert!(parts[1].starts_with("sql:"));
    }

    #[test]
    fn test_execute_restart_commands_empty_segments() {
        let cmd = "exec:echo hello;;sql:SELECT 1;";
        let parts: Vec<&str> = cmd
            .split(';')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect();
        assert_eq!(parts.len(), 2);
    }
}
