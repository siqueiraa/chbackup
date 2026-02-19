//! RBAC, config file, named collection, and function backup logic (Phase 4e).
//!
//! Collects RBAC objects (users, roles, etc.), config files from the filesystem,
//! named collections, and user-defined SQL functions during `backup::create()`.
//! Results are written to the backup directory and/or stored in the manifest.

use std::path::Path;

use anyhow::{Context, Result};
use tracing::{debug, info, warn};

use crate::clickhouse::client::ChClient;
use crate::config::Config;
use crate::manifest::{BackupManifest, RbacInfo};

/// A single RBAC entity serialized in JSONL format.
#[derive(serde::Serialize, serde::Deserialize, Debug)]
struct RbacEntry {
    entity_type: String,
    name: String,
    create_statement: String,
}

/// Entity type descriptors: (entity_type_sql, entity_type_lower, jsonl_filename).
const RBAC_ENTITY_TYPES: &[(&str, &str, &str)] = &[
    ("USER", "user", "users.jsonl"),
    ("ROLE", "role", "roles.jsonl"),
    ("ROW POLICY", "row_policy", "row_policies.jsonl"),
    ("SETTINGS PROFILE", "settings_profile", "settings_profiles.jsonl"),
    ("QUOTA", "quota", "quotas.jsonl"),
];

/// Backup RBAC objects, config files, named collections, and functions.
///
/// Called after manifest creation, before the diff step. Populates manifest
/// fields and writes files to the backup directory.
///
/// The `rbac`, `configs`, and `named_collections` flags gate each subsystem.
/// Each flag is OR'd with the corresponding `*_backup_always` config value.
/// Functions are always backed up (zero-cost DDL in manifest).
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

    // Functions backup (always -- DDL stored in manifest, zero cost)
    backup_functions(ch, manifest).await?;

    Ok(())
}

/// Backup RBAC objects (users, roles, row policies, settings profiles, quotas).
///
/// For each entity type, queries ClickHouse for the CREATE DDL and writes
/// a JSONL file to `{backup_dir}/access/{entity_type}.jsonl`.
///
/// Sets `manifest.rbac` to point at the access/ directory.
async fn backup_rbac(
    ch: &ChClient,
    backup_dir: &Path,
    manifest: &mut BackupManifest,
) -> Result<()> {
    let access_dir = backup_dir.join("access");
    std::fs::create_dir_all(&access_dir)
        .context("Failed to create access/ directory for RBAC backup")?;

    let mut total_count: usize = 0;
    let mut summary_parts: Vec<String> = Vec::new();

    for &(entity_type_sql, entity_type_lower, filename) in RBAC_ENTITY_TYPES {
        let objects = ch.query_rbac_objects(entity_type_sql).await?;
        let count = objects.len();

        if !objects.is_empty() {
            let file_path = access_dir.join(filename);
            let mut lines = Vec::with_capacity(objects.len());
            for (name, ddl) in &objects {
                let entry = RbacEntry {
                    entity_type: entity_type_lower.to_string(),
                    name: name.clone(),
                    create_statement: ddl.clone(),
                };
                lines.push(
                    serde_json::to_string(&entry)
                        .context("Failed to serialize RBAC entry to JSON")?,
                );
            }
            let content = lines.join("\n") + "\n";
            std::fs::write(&file_path, content)
                .with_context(|| format!("Failed to write {}", filename))?;
        }

        total_count += count;
        summary_parts.push(format!("{} {}s", count, entity_type_lower));
    }

    manifest.rbac = Some(RbacInfo {
        path: "access/".to_string(),
    });

    info!(
        total = total_count,
        "RBAC backup: {}",
        summary_parts.join(", ")
    );

    Ok(())
}

/// Backup config files from the ClickHouse config directory.
///
/// Copies all files from `config.clickhouse.config_dir` to `{backup_dir}/configs/`,
/// preserving directory structure. Uses `spawn_blocking` for filesystem I/O.
async fn backup_configs(config: &Config, backup_dir: &Path) -> Result<()> {
    let config_dir = std::path::PathBuf::from(&config.clickhouse.config_dir);
    let configs_target = backup_dir.join("configs");

    if !config_dir.exists() {
        warn!(
            config_dir = %config_dir.display(),
            "Config directory does not exist, skipping config backup"
        );
        return Ok(());
    }

    let config_dir_clone = config_dir.clone();
    let configs_target_clone = configs_target.clone();

    let file_count = tokio::task::spawn_blocking(move || -> Result<usize> {
        let mut count = 0usize;
        for entry in walkdir::WalkDir::new(&config_dir_clone)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if entry.file_type().is_file() {
                let rel = entry
                    .path()
                    .strip_prefix(&config_dir_clone)
                    .context("Failed to compute relative path for config file")?;
                let dest = configs_target_clone.join(rel);
                if let Some(parent) = dest.parent() {
                    std::fs::create_dir_all(parent)
                        .with_context(|| format!("Failed to create directory {}", parent.display()))?;
                }
                std::fs::copy(entry.path(), &dest)
                    .with_context(|| format!("Failed to copy config file {}", entry.path().display()))?;
                count += 1;
            }
        }
        Ok(count)
    })
    .await
    .context("spawn_blocking panicked during config backup")??;

    info!(
        file_count = file_count,
        config_dir = %config_dir.display(),
        "Config backup: {} files copied from {}",
        file_count,
        config_dir.display()
    );

    Ok(())
}

/// Backup named collections (DDL stored in manifest).
async fn backup_named_collections(ch: &ChClient, manifest: &mut BackupManifest) -> Result<()> {
    let collections = ch.query_named_collections().await?;
    let count = collections.len();
    manifest.named_collections = collections;

    info!(count = count, "Named collections backup: {} collections", count);
    Ok(())
}

/// Backup user-defined SQL functions (DDL stored in manifest).
async fn backup_functions(ch: &ChClient, manifest: &mut BackupManifest) -> Result<()> {
    let functions = ch.query_user_defined_functions().await?;
    let count = functions.len();
    manifest.functions = functions;

    debug!(count = count, "Functions backup: {} functions", count);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rbac_entry_serialization() {
        let entry = RbacEntry {
            entity_type: "user".to_string(),
            name: "admin".to_string(),
            create_statement: "CREATE USER admin IDENTIFIED BY 'password'".to_string(),
        };

        let json = serde_json::to_string(&entry).unwrap();
        assert!(json.contains("\"entity_type\":\"user\""));
        assert!(json.contains("\"name\":\"admin\""));
        assert!(json.contains("\"create_statement\":\"CREATE USER admin"));

        // Verify round-trip
        let parsed: RbacEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.entity_type, "user");
        assert_eq!(parsed.name, "admin");
    }

    #[test]
    fn test_rbac_entry_jsonl_format() {
        let entries = [
            RbacEntry {
                entity_type: "user".to_string(),
                name: "alice".to_string(),
                create_statement: "CREATE USER alice".to_string(),
            },
            RbacEntry {
                entity_type: "user".to_string(),
                name: "bob".to_string(),
                create_statement: "CREATE USER bob".to_string(),
            },
        ];

        let lines: Vec<String> = entries
            .iter()
            .map(|e| serde_json::to_string(e).unwrap())
            .collect();
        let content = lines.join("\n") + "\n";

        // Each line should be valid JSON
        for line in content.trim().lines() {
            let parsed: RbacEntry = serde_json::from_str(line).unwrap();
            assert!(!parsed.name.is_empty());
        }
    }

    #[test]
    fn test_rbac_entity_types_constant() {
        // Verify all 5 entity types are defined
        assert_eq!(RBAC_ENTITY_TYPES.len(), 5);

        // Verify the SQL entity types
        let sql_types: Vec<&str> = RBAC_ENTITY_TYPES.iter().map(|(s, _, _)| *s).collect();
        assert!(sql_types.contains(&"USER"));
        assert!(sql_types.contains(&"ROLE"));
        assert!(sql_types.contains(&"ROW POLICY"));
        assert!(sql_types.contains(&"SETTINGS PROFILE"));
        assert!(sql_types.contains(&"QUOTA"));

        // Verify filenames end with .jsonl
        for &(_, _, filename) in RBAC_ENTITY_TYPES {
            assert!(filename.ends_with(".jsonl"), "Expected .jsonl suffix: {}", filename);
        }
    }
}
