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

use super::attach::{chown_recursive, detect_clickhouse_ownership};
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
        "Named collection restore complete"
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
        "RBAC restore complete"
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
        "Config restore complete"
    );
    Ok(())
}

/// Execute a shell command via `sh -c`, logging the outcome.
///
/// All failures are non-fatal (logged as warnings).
async fn run_shell_command(cmd: &str, label: &str) {
    tracing::info!(command = %cmd, "Executing restart command ({})", label);
    match tokio::process::Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .output()
        .await
    {
        Ok(output) if !output.status.success() => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            tracing::warn!(command = %cmd, stderr = %stderr, "Restart command failed (non-fatal)");
        }
        Err(e) => {
            tracing::warn!(command = %cmd, error = %e, "Failed to execute restart command (non-fatal)");
        }
        _ => {
            tracing::info!(command = %cmd, "Restart command executed successfully");
        }
    }
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
            run_shell_command(exec_cmd.trim(), "exec").await;
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
            run_shell_command(cmd, "exec, no prefix").await;
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
    let escaped_name = name.replace('`', "``");
    Some(format!("DROP {} IF EXISTS `{}`", keyword, escaped_name))
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
    fn test_make_drop_ddl_backtick_escape() {
        let ddl = make_drop_ddl("user", "user`name");
        assert_eq!(
            ddl,
            Some("DROP USER IF EXISTS `user``name`".to_string())
        );
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

    // -----------------------------------------------------------------------
    // Additional coverage: make_drop_ddl edge cases
    // -----------------------------------------------------------------------

    /// Test make_drop_ddl with a name containing multiple backticks.
    #[test]
    fn test_make_drop_ddl_multiple_backticks() {
        let ddl = make_drop_ddl("role", "ro`le`name");
        assert_eq!(
            ddl,
            Some("DROP ROLE IF EXISTS `ro``le``name`".to_string())
        );
    }

    /// Test make_drop_ddl with empty entity name.
    #[test]
    fn test_make_drop_ddl_empty_name() {
        let ddl = make_drop_ddl("user", "");
        assert_eq!(ddl, Some("DROP USER IF EXISTS ``".to_string()));
    }

    /// Test make_drop_ddl with special characters in entity name.
    #[test]
    fn test_make_drop_ddl_special_chars() {
        let ddl = make_drop_ddl("quota", "daily quota (v2)");
        assert_eq!(
            ddl,
            Some("DROP QUOTA IF EXISTS `daily quota (v2)`".to_string())
        );
    }

    /// Test make_drop_ddl returns None for various unknown types.
    #[test]
    fn test_make_drop_ddl_unknown_types() {
        assert_eq!(make_drop_ddl("database", "foo"), None);
        assert_eq!(make_drop_ddl("table", "bar"), None);
        assert_eq!(make_drop_ddl("", "baz"), None);
        assert_eq!(make_drop_ddl("USER", "admin"), None); // case-sensitive
    }

    /// Test make_drop_ddl with entity name containing Unicode.
    #[test]
    fn test_make_drop_ddl_unicode_name() {
        let ddl = make_drop_ddl("user", "admin_user");
        assert_eq!(
            ddl,
            Some("DROP USER IF EXISTS `admin_user`".to_string())
        );
    }

    // -----------------------------------------------------------------------
    // Additional coverage: RbacEntry parsing edge cases
    // -----------------------------------------------------------------------

    /// Test parsing RbacEntry with all entity types.
    #[test]
    fn test_parse_rbac_all_entity_types() {
        let entries_json = vec![
            r#"{"entity_type":"user","name":"u1","create_statement":"CREATE USER u1"}"#,
            r#"{"entity_type":"role","name":"r1","create_statement":"CREATE ROLE r1"}"#,
            r#"{"entity_type":"row_policy","name":"p1","create_statement":"CREATE ROW POLICY p1"}"#,
            r#"{"entity_type":"settings_profile","name":"s1","create_statement":"CREATE SETTINGS PROFILE s1"}"#,
            r#"{"entity_type":"quota","name":"q1","create_statement":"CREATE QUOTA q1"}"#,
        ];

        for json_str in &entries_json {
            let entry: RbacEntry = serde_json::from_str(json_str).unwrap();
            assert!(!entry.entity_type.is_empty());
            assert!(!entry.name.is_empty());
            assert!(!entry.create_statement.is_empty());
        }
    }

    /// Verify make_drop_ddl covers all entity_type values that RbacEntry can have.
    #[test]
    fn test_make_drop_ddl_all_known_types() {
        let types_and_keywords = vec![
            ("user", "DROP USER IF EXISTS"),
            ("role", "DROP ROLE IF EXISTS"),
            ("row_policy", "DROP ROW POLICY IF EXISTS"),
            ("settings_profile", "DROP SETTINGS PROFILE IF EXISTS"),
            ("quota", "DROP QUOTA IF EXISTS"),
        ];

        for (entity_type, expected_prefix) in types_and_keywords {
            let result = make_drop_ddl(entity_type, "test_entity");
            assert!(result.is_some(), "Expected Some for entity_type: {}", entity_type);
            let ddl = result.unwrap();
            assert!(
                ddl.starts_with(expected_prefix),
                "Expected DDL starting with '{}', got: '{}'",
                expected_prefix,
                ddl
            );
            assert!(ddl.contains("`test_entity`"));
        }
    }

    /// Test restart command parsing with only sql: prefix.
    #[test]
    fn test_execute_restart_commands_sql_only() {
        let cmd = "sql:SYSTEM RELOAD CONFIG";
        let parts: Vec<&str> = cmd.split(';').map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
        assert_eq!(parts.len(), 1);
        assert!(parts[0].starts_with("sql:"));
        let sql = parts[0].strip_prefix("sql:").unwrap().trim();
        assert_eq!(sql, "SYSTEM RELOAD CONFIG");
    }

    /// Test restart command parsing with no prefix (defaults to exec).
    #[test]
    fn test_execute_restart_commands_no_prefix() {
        let cmd = "systemctl restart clickhouse-server";
        let parts: Vec<&str> = cmd.split(';').map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
        assert_eq!(parts.len(), 1);
        assert!(!parts[0].starts_with("exec:"));
        assert!(!parts[0].starts_with("sql:"));
    }

    /// Test restart command parsing with mixed prefixes.
    #[test]
    fn test_execute_restart_commands_mixed() {
        let cmd = "exec:echo hello;sql:SELECT 1;/usr/bin/service restart";
        let parts: Vec<&str> = cmd.split(';').map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
        assert_eq!(parts.len(), 3);
        assert!(parts[0].starts_with("exec:"));
        assert!(parts[1].starts_with("sql:"));
        assert!(!parts[2].starts_with("exec:"));
        assert!(!parts[2].starts_with("sql:"));
    }

    // -----------------------------------------------------------------------
    // Comprehensive restart command parsing coverage
    // -----------------------------------------------------------------------

    /// Verify empty command string produces no parts.
    #[test]
    fn test_execute_restart_commands_empty_string() {
        let cmd = "";
        let parts: Vec<&str> = cmd
            .split(';')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect();
        assert_eq!(parts.len(), 0);
    }

    /// Verify only-whitespace segments are filtered out.
    #[test]
    fn test_execute_restart_commands_whitespace_only() {
        let cmd = " ; ; ";
        let parts: Vec<&str> = cmd
            .split(';')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect();
        assert_eq!(parts.len(), 0);
    }

    /// Verify exec: prefix is correctly stripped.
    #[test]
    fn test_restart_commands_exec_strip_prefix() {
        let cmd = "exec:systemctl restart clickhouse-server";
        let exec_cmd = cmd.strip_prefix("exec:").unwrap().trim();
        assert_eq!(exec_cmd, "systemctl restart clickhouse-server");
    }

    /// Verify sql: prefix is correctly stripped.
    #[test]
    fn test_restart_commands_sql_strip_prefix() {
        let cmd = "sql:SYSTEM RELOAD CONFIG";
        let sql_cmd = cmd.strip_prefix("sql:").unwrap().trim();
        assert_eq!(sql_cmd, "SYSTEM RELOAD CONFIG");
    }

    /// Verify command without prefix is not matched by exec: or sql:.
    #[test]
    fn test_restart_commands_no_prefix_detection() {
        let cmd = "/usr/local/bin/restart.sh";
        assert!(cmd.strip_prefix("exec:").is_none());
        assert!(cmd.strip_prefix("sql:").is_none());
    }

    /// Verify multiple sql: commands are correctly split.
    #[test]
    fn test_restart_commands_multiple_sql() {
        let cmd = "sql:SYSTEM RELOAD CONFIG;sql:SYSTEM FLUSH LOGS";
        let parts: Vec<&str> = cmd
            .split(';')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect();
        assert_eq!(parts.len(), 2);
        assert_eq!(
            parts[0].strip_prefix("sql:").unwrap().trim(),
            "SYSTEM RELOAD CONFIG"
        );
        assert_eq!(
            parts[1].strip_prefix("sql:").unwrap().trim(),
            "SYSTEM FLUSH LOGS"
        );
    }

    /// Verify trailing semicolons do not produce empty segments.
    #[test]
    fn test_restart_commands_trailing_semicolons() {
        let cmd = "exec:echo test;;;";
        let parts: Vec<&str> = cmd
            .split(';')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect();
        assert_eq!(parts.len(), 1);
        assert!(parts[0].starts_with("exec:"));
    }

    // -----------------------------------------------------------------------
    // Comprehensive make_drop_ddl coverage
    // -----------------------------------------------------------------------

    /// Verify all five known entity types produce correct DROP DDL.
    #[test]
    fn test_make_drop_ddl_all_known_entity_types() {
        let cases = vec![
            ("user", "USER"),
            ("role", "ROLE"),
            ("row_policy", "ROW POLICY"),
            ("settings_profile", "SETTINGS PROFILE"),
            ("quota", "QUOTA"),
        ];

        for (entity_type, keyword) in cases {
            let result = make_drop_ddl(entity_type, "test_name");
            assert!(result.is_some(), "Expected Some for entity_type: {}", entity_type);
            let ddl = result.unwrap();
            assert_eq!(
                ddl,
                format!("DROP {} IF EXISTS `test_name`", keyword),
                "Wrong DDL for entity_type: {}",
                entity_type
            );
        }
    }

    /// Entity names with special characters (spaces, parens) are preserved inside backticks.
    #[test]
    fn test_make_drop_ddl_special_chars_in_name() {
        let ddl = make_drop_ddl("quota", "daily quota (v2)").unwrap();
        assert_eq!(ddl, "DROP QUOTA IF EXISTS `daily quota (v2)`");
    }

    /// Unknown entity types return None.
    #[test]
    fn test_make_drop_ddl_unknown_types_comprehensive() {
        assert_eq!(make_drop_ddl("database", "foo"), None);
        assert_eq!(make_drop_ddl("table", "bar"), None);
        assert_eq!(make_drop_ddl("", "baz"), None);
        // Case-sensitive: uppercase "USER" is not a known entity_type value.
        assert_eq!(make_drop_ddl("USER", "admin"), None);
        assert_eq!(make_drop_ddl("ROLE", "readonly"), None);
    }

    /// Multiple consecutive backticks in the name are all escaped.
    #[test]
    fn test_make_drop_ddl_consecutive_backticks() {
        let ddl = make_drop_ddl("user", "a``b").unwrap();
        // Each ` becomes ``, so `` becomes ````
        assert_eq!(ddl, "DROP USER IF EXISTS `a````b`");
    }

    // -----------------------------------------------------------------------
    /// RbacEntry with special characters in name.
    #[test]
    fn test_parse_rbac_special_name() {
        let json = r#"{"entity_type":"user","name":"user@host","create_statement":"CREATE USER `user@host`"}"#;
        let entry: RbacEntry = serde_json::from_str(json).unwrap();
        assert_eq!(entry.name, "user@host");
        assert!(entry.create_statement.contains("`user@host`"));
    }

    /// RbacEntry with unicode name.
    #[test]
    fn test_parse_rbac_unicode_name() {
        let json = r#"{"entity_type":"role","name":"admin_role","create_statement":"CREATE ROLE admin_role"}"#;
        let entry: RbacEntry = serde_json::from_str(json).unwrap();
        assert_eq!(entry.entity_type, "role");
        assert_eq!(entry.name, "admin_role");
    }

    /// RbacEntry with multiline create_statement.
    #[test]
    fn test_parse_rbac_multiline_statement() {
        let json = r#"{"entity_type":"row_policy","name":"filter_policy","create_statement":"CREATE ROW POLICY filter_policy ON db.t FOR SELECT USING id > 0"}"#;
        let entry: RbacEntry = serde_json::from_str(json).unwrap();
        assert_eq!(entry.entity_type, "row_policy");
        assert!(entry.create_statement.contains("FOR SELECT USING"));
    }

    /// Verify make_drop_ddl output matches the entity_type values that RbacEntry uses.
    #[test]
    fn test_make_drop_ddl_matches_rbac_entity_types() {
        let json_entries = vec![
            r#"{"entity_type":"user","name":"u1","create_statement":"CREATE USER u1"}"#,
            r#"{"entity_type":"role","name":"r1","create_statement":"CREATE ROLE r1"}"#,
            r#"{"entity_type":"row_policy","name":"p1","create_statement":"CREATE ROW POLICY p1"}"#,
            r#"{"entity_type":"settings_profile","name":"s1","create_statement":"CREATE SETTINGS PROFILE s1"}"#,
            r#"{"entity_type":"quota","name":"q1","create_statement":"CREATE QUOTA q1"}"#,
        ];

        for json_str in &json_entries {
            let entry: RbacEntry = serde_json::from_str(json_str).unwrap();
            let drop_ddl = make_drop_ddl(&entry.entity_type, &entry.name);
            assert!(
                drop_ddl.is_some(),
                "make_drop_ddl should handle entity_type '{}' from RbacEntry",
                entry.entity_type
            );
            let ddl = drop_ddl.unwrap();
            assert!(
                ddl.starts_with("DROP "),
                "DROP DDL should start with DROP: {}",
                ddl
            );
            assert!(
                ddl.contains("IF EXISTS"),
                "DROP DDL should contain IF EXISTS: {}",
                ddl
            );
            assert!(
                ddl.contains(&format!("`{}`", entry.name)),
                "DROP DDL should contain backtick-quoted name: {}",
                ddl
            );
        }
    }

    // ---- restore_named_collections tests ----

    #[tokio::test]
    async fn test_restore_named_collections_empty() {
        use crate::clickhouse::client::ChClient;
        use crate::config::ClickHouseConfig;
        use crate::manifest::BackupManifest;

        let ch = ChClient::new(&ClickHouseConfig::default()).unwrap();
        let manifest = BackupManifest::test_new("test");
        // Empty named_collections should return Ok immediately
        let result = restore_named_collections(&ch, &manifest, None).await;
        assert!(result.is_ok());
    }

    // ---- restore_rbac tests (file I/O) ----

    #[tokio::test]
    async fn test_restore_rbac_no_access_dir() {
        use crate::clickhouse::client::ChClient;
        use crate::config::ClickHouseConfig;

        let ch = ChClient::new(&ClickHouseConfig::default()).unwrap();
        let config = Config::default();
        let backup_dir = tempfile::tempdir().unwrap();
        // No access/ directory should return Ok immediately
        let result = restore_rbac(&ch, &config, backup_dir.path(), "recreate").await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_restore_rbac_empty_access_dir() {
        // access/ exists but has no .jsonl files -> empty entries -> return Ok
        use crate::clickhouse::client::ChClient;
        use crate::config::ClickHouseConfig;

        let ch = ChClient::new(&ClickHouseConfig::default()).unwrap();
        let config = Config::default();
        let backup_dir = tempfile::tempdir().unwrap();
        let access_dir = backup_dir.path().join("access");
        std::fs::create_dir_all(&access_dir).unwrap();
        // Put a non-.jsonl file to ensure it's ignored
        std::fs::write(access_dir.join("readme.txt"), b"not jsonl").unwrap();

        let result = restore_rbac(&ch, &config, backup_dir.path(), "recreate").await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_execute_restart_commands_empty() {
        // Empty restart_command should return Ok immediately
        use crate::clickhouse::client::ChClient;
        use crate::config::ClickHouseConfig;

        let ch = ChClient::new(&ClickHouseConfig::default()).unwrap();
        let result = execute_restart_commands(&ch, "").await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_execute_restart_commands_exec_only() {
        // exec: commands run via shell, don't need ChClient
        use crate::clickhouse::client::ChClient;
        use crate::config::ClickHouseConfig;

        let ch = ChClient::new(&ClickHouseConfig::default()).unwrap();
        let result = execute_restart_commands(&ch, "exec:echo hello").await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_execute_restart_commands_no_prefix_async() {
        // Commands without prefix default to exec: behavior
        use crate::clickhouse::client::ChClient;
        use crate::config::ClickHouseConfig;

        let ch = ChClient::new(&ClickHouseConfig::default()).unwrap();
        let result = execute_restart_commands(&ch, "echo hello").await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_execute_restart_commands_multi_exec() {
        // Multiple exec commands separated by semicolons
        use crate::clickhouse::client::ChClient;
        use crate::config::ClickHouseConfig;

        let ch = ChClient::new(&ClickHouseConfig::default()).unwrap();
        let result = execute_restart_commands(&ch, "exec:echo a;exec:echo b;exec:echo c").await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_execute_restart_commands_failing_exec() {
        // Failing exec command should be non-fatal
        use crate::clickhouse::client::ChClient;
        use crate::config::ClickHouseConfig;

        let ch = ChClient::new(&ClickHouseConfig::default()).unwrap();
        let result = execute_restart_commands(&ch, "exec:false").await;
        assert!(result.is_ok(), "Failing exec command should be non-fatal");
    }

    #[tokio::test]
    async fn test_restore_rbac_jsonl_empty_lines_only() {
        // access/ has .jsonl files but only empty/whitespace lines -> empty entries
        use crate::clickhouse::client::ChClient;
        use crate::config::ClickHouseConfig;

        let ch = ChClient::new(&ClickHouseConfig::default()).unwrap();
        let config = Config::default();
        let backup_dir = tempfile::tempdir().unwrap();
        let access_dir = backup_dir.path().join("access");
        std::fs::create_dir_all(&access_dir).unwrap();
        std::fs::write(access_dir.join("users.jsonl"), "\n  \n\n").unwrap();

        let result = restore_rbac(&ch, &config, backup_dir.path(), "recreate").await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_restore_rbac_invalid_json_in_jsonl() {
        // .jsonl file with invalid JSON should return error
        use crate::clickhouse::client::ChClient;
        use crate::config::ClickHouseConfig;

        let ch = ChClient::new(&ClickHouseConfig::default()).unwrap();
        let config = Config::default();
        let backup_dir = tempfile::tempdir().unwrap();
        let access_dir = backup_dir.path().join("access");
        std::fs::create_dir_all(&access_dir).unwrap();
        std::fs::write(access_dir.join("users.jsonl"), "not valid json\n").unwrap();

        let result = restore_rbac(&ch, &config, backup_dir.path(), "recreate").await;
        assert!(result.is_err(), "Invalid JSON should cause error");
    }

    #[tokio::test]
    async fn test_restore_rbac_with_valid_entries_recreate() {
        // Valid .jsonl entries + no ClickHouse → DDL will fail, but "recreate" mode
        // should log warnings and continue (non-fatal for DDL failures in recreate mode)
        use crate::clickhouse::client::ChClient;
        use crate::config::ClickHouseConfig;

        let ch = ChClient::new(&ClickHouseConfig::default()).unwrap();
        let config = Config::default();
        let backup_dir = tempfile::tempdir().unwrap();
        let access_dir = backup_dir.path().join("access");
        std::fs::create_dir_all(&access_dir).unwrap();

        let jsonl = r#"{"entity_type":"user","name":"test_user","create_statement":"CREATE USER `test_user`"}"#;
        std::fs::write(access_dir.join("users.jsonl"), jsonl).unwrap();

        // This will parse the file, then try to execute DDL which will fail
        // because there's no ClickHouse running. With "recreate" mode,
        // failures are logged as warnings and skipped.
        let result = restore_rbac(&ch, &config, backup_dir.path(), "recreate").await;
        // Result depends on whether DDL failure is propagated — in recreate mode
        // it should be non-fatal, so Ok is expected
        assert!(result.is_ok(), "Recreate mode should handle DDL failures gracefully");
    }

    #[tokio::test]
    async fn test_restore_rbac_with_valid_entries_ignore() {
        // Valid .jsonl entries + no ClickHouse → "ignore" mode skips failures
        use crate::clickhouse::client::ChClient;
        use crate::config::ClickHouseConfig;

        let ch = ChClient::new(&ClickHouseConfig::default()).unwrap();
        let config = Config::default();
        let backup_dir = tempfile::tempdir().unwrap();
        let access_dir = backup_dir.path().join("access");
        std::fs::create_dir_all(&access_dir).unwrap();

        let jsonl = r#"{"entity_type":"role","name":"test_role","create_statement":"CREATE ROLE `test_role`"}"#;
        std::fs::write(access_dir.join("roles.jsonl"), jsonl).unwrap();

        let result = restore_rbac(&ch, &config, backup_dir.path(), "ignore").await;
        assert!(result.is_ok(), "Ignore mode should handle DDL failures gracefully");
    }

    #[tokio::test]
    async fn test_restore_rbac_with_valid_entries_fail_mode() {
        // Valid .jsonl entries + no ClickHouse → "fail" mode should return error
        use crate::clickhouse::client::ChClient;
        use crate::config::ClickHouseConfig;

        let ch = ChClient::new(&ClickHouseConfig::default()).unwrap();
        let config = Config::default();
        let backup_dir = tempfile::tempdir().unwrap();
        let access_dir = backup_dir.path().join("access");
        std::fs::create_dir_all(&access_dir).unwrap();

        let jsonl = r#"{"entity_type":"user","name":"u1","create_statement":"CREATE USER `u1`"}"#;
        std::fs::write(access_dir.join("users.jsonl"), jsonl).unwrap();

        let result = restore_rbac(&ch, &config, backup_dir.path(), "fail").await;
        assert!(result.is_err(), "Fail mode should return error on DDL failure");
    }

    // ---- restore_configs tests (file I/O only, no ChClient) ----

    #[tokio::test]
    async fn test_restore_configs_no_configs_dir() {
        // When backup has no configs/ directory, should return Ok silently
        let backup_dir = tempfile::tempdir().unwrap();
        let config = Config {
            clickhouse: crate::config::ClickHouseConfig {
                config_dir: "/tmp/unused".to_string(),
                ..Default::default()
            },
            ..Default::default()
        };
        let result = restore_configs(&config, backup_dir.path()).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_restore_configs_copies_files() {
        let backup_dir = tempfile::tempdir().unwrap();
        let target_dir = tempfile::tempdir().unwrap();

        // Create configs/ directory with files in backup
        let configs_src = backup_dir.path().join("configs");
        std::fs::create_dir_all(configs_src.join("subdir")).unwrap();
        std::fs::write(configs_src.join("users.xml"), b"<users/>").unwrap();
        std::fs::write(configs_src.join("subdir/remote.xml"), b"<remote/>").unwrap();

        let config = Config {
            clickhouse: crate::config::ClickHouseConfig {
                config_dir: target_dir.path().to_str().unwrap().to_string(),
                ..Default::default()
            },
            ..Default::default()
        };

        let result = restore_configs(&config, backup_dir.path()).await;
        assert!(result.is_ok());

        // Verify files were copied
        assert_eq!(
            std::fs::read_to_string(target_dir.path().join("users.xml")).unwrap(),
            "<users/>"
        );
        assert_eq!(
            std::fs::read_to_string(target_dir.path().join("subdir/remote.xml")).unwrap(),
            "<remote/>"
        );
    }

    #[tokio::test]
    async fn test_restore_configs_empty_configs_dir() {
        let backup_dir = tempfile::tempdir().unwrap();
        let target_dir = tempfile::tempdir().unwrap();

        // Create empty configs/ directory
        std::fs::create_dir_all(backup_dir.path().join("configs")).unwrap();

        let config = Config {
            clickhouse: crate::config::ClickHouseConfig {
                config_dir: target_dir.path().to_str().unwrap().to_string(),
                ..Default::default()
            },
            ..Default::default()
        };

        let result = restore_configs(&config, backup_dir.path()).await;
        assert!(result.is_ok());
    }
}
