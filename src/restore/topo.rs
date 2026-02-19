//! Phased restore classification and topological sort for DDL-only objects.
//!
//! Implements the table classification and dependency-aware ordering from
//! design doc sections 5.1 and 5.5:
//! - `classify_restore_tables()` splits filtered tables into data tables
//!   (Phase 2) and DDL-only objects (Phase 3)
//! - `topological_sort()` orders DDL-only objects by their dependency graph
//!   (Kahn's algorithm), falling back to engine-priority sorting when
//!   dependency info is unavailable

use std::collections::{HashMap, HashSet, VecDeque};

use anyhow::Result;
use tracing::{info, warn};

use crate::manifest::{BackupManifest, TableManifest};

/// Engine priority for Phase 2 (data tables). Lower = created first.
/// 0: Regular MergeTree tables
/// 1: .inner tables (MV storage targets -- name starts with ".inner" or ".inner_id")
pub fn data_table_priority(table_key: &str) -> u8 {
    // Use split_once('.') to correctly handle .inner tables whose names contain dots
    // e.g. "default..inner_id.5f3a7b2c-..." -> table_name = ".inner_id.5f3a7b2c-..."
    let table_name = table_key
        .split_once('.')
        .map(|(_, t)| t)
        .unwrap_or(table_key);
    if table_name.starts_with(".inner") {
        1
    } else {
        0
    }
}

/// Engine priority for Phase 3 (DDL-only objects). Lower = created first.
/// Per design doc 5.1:
/// 0: Dictionary
/// 1: View, MaterializedView, LiveView, WindowView
/// 2: Distributed, Merge
pub fn engine_restore_priority(engine: &str) -> u8 {
    match engine {
        "Dictionary" => 0,
        "View" | "MaterializedView" | "LiveView" | "WindowView" => 1,
        "Distributed" | "Merge" => 2,
        _ => 3, // Null, Set, Join, Buffer -- rarely restored as DDL-only
    }
}

/// Classification of tables into restore phases.
#[derive(Debug, Clone)]
pub struct RestorePhases {
    /// Phase 2: Data tables (MergeTree family) sorted by engine priority.
    pub data_tables: Vec<String>,
    /// Phase 2b: Postponed tables (streaming engines) -- empty for now (Phase 4c).
    pub postponed_tables: Vec<String>,
    /// Phase 3: DDL-only objects, topologically sorted by dependencies.
    pub ddl_only_tables: Vec<String>,
}

/// Classify filtered tables into restore phases using metadata_only flag.
pub fn classify_restore_tables(
    manifest: &BackupManifest,
    table_keys: &[String],
) -> RestorePhases {
    let mut data_tables: Vec<String> = Vec::new();
    let mut ddl_only_tables: Vec<String> = Vec::new();

    for key in table_keys {
        if let Some(tm) = manifest.tables.get(key) {
            if tm.metadata_only {
                ddl_only_tables.push(key.clone());
            } else {
                data_tables.push(key.clone());
            }
        }
    }

    // Sort data tables by priority (regular first, then .inner tables)
    data_tables.sort_by_key(|k| data_table_priority(k));

    info!(
        data = data_tables.len(),
        ddl_only = ddl_only_tables.len(),
        "Classified {} tables: {} data, {} DDL-only",
        table_keys.len(),
        data_tables.len(),
        ddl_only_tables.len(),
    );

    RestorePhases {
        data_tables,
        postponed_tables: Vec::new(), // Phase 4c
        ddl_only_tables,
    }
}

/// Topological sort of DDL-only tables using their dependency graph (Kahn's algorithm).
///
/// If dependencies are empty (CH < 23.3 or old manifest), falls back to
/// engine-priority sorting.
///
/// Handles cycles by breaking them (removes back-edges) with a warning log.
pub fn topological_sort(
    tables: &HashMap<String, TableManifest>,
    keys: &[String],
) -> Result<Vec<String>> {
    // Check if any table has non-empty dependencies
    let has_deps = keys.iter().any(|k| {
        tables
            .get(k)
            .is_some_and(|tm| !tm.dependencies.is_empty())
    });

    if !has_deps {
        // Fallback: sort by engine priority only
        let mut sorted = keys.to_vec();
        sorted.sort_by_key(|k| {
            tables
                .get(k)
                .map_or(3, |tm| engine_restore_priority(&tm.engine))
        });
        info!(
            count = sorted.len(),
            "Topological sort produced (engine-priority fallback, no dependency info)"
        );
        return Ok(sorted);
    }

    // Build adjacency and in-degree for Kahn's algorithm
    let key_set: HashSet<&String> = keys.iter().collect();
    let mut in_degree: HashMap<&String, usize> = HashMap::new();
    let mut adjacency: HashMap<&String, Vec<&String>> = HashMap::new();

    for key in keys {
        in_degree.entry(key).or_insert(0);
        adjacency.entry(key).or_default();
    }

    // For each table, its dependencies are tables that must be created BEFORE it.
    // So if table A depends on table B, there is an edge B -> A (B must come first).
    for key in keys {
        if let Some(tm) = tables.get(key) {
            for dep in &tm.dependencies {
                // Only count edges within our key set (deps on Phase 2 tables are already satisfied)
                if key_set.contains(dep) {
                    adjacency.entry(dep).or_default().push(key);
                    *in_degree.entry(key).or_insert(0) += 1;
                }
            }
        }
    }

    // Kahn's algorithm with engine-priority tie-breaking
    let mut queue: VecDeque<&String> = VecDeque::new();
    for key in keys {
        if in_degree.get(key).copied().unwrap_or(0) == 0 {
            queue.push_back(key);
        }
    }

    // Sort initial queue by engine priority for deterministic ordering
    let mut queue_vec: Vec<&String> = queue.into_iter().collect();
    queue_vec.sort_by_key(|k| {
        tables
            .get(*k)
            .map_or(3, |tm| engine_restore_priority(&tm.engine))
    });
    let mut queue: VecDeque<&String> = queue_vec.into_iter().collect();

    let mut result: Vec<String> = Vec::with_capacity(keys.len());

    while let Some(node) = queue.pop_front() {
        result.push(node.clone());
        if let Some(neighbors) = adjacency.get(node) {
            for neighbor in neighbors {
                if let Some(deg) = in_degree.get_mut(neighbor) {
                    *deg = deg.saturating_sub(1);
                    if *deg == 0 {
                        queue.push_back(neighbor);
                    }
                }
            }
        }
    }

    // Check for cycles (nodes left with non-zero in-degree)
    if result.len() < keys.len() {
        let remaining: Vec<String> = keys
            .iter()
            .filter(|k| !result.contains(k))
            .cloned()
            .collect();
        warn!(
            remaining = ?remaining,
            "Dependency cycle detected among DDL-only objects, appending in engine-priority order"
        );
        let mut remaining_sorted = remaining;
        remaining_sorted.sort_by_key(|k| {
            tables
                .get(k)
                .map_or(3, |tm| engine_restore_priority(&tm.engine))
        });
        result.extend(remaining_sorted);
    }

    info!(
        count = result.len(),
        "Topological sort produced {} DDL-only objects in dependency order",
        result.len()
    );
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{BackupManifest, DatabaseInfo, TableManifest};
    use chrono::Utc;
    use std::collections::HashMap;

    fn make_table_manifest(engine: &str, metadata_only: bool, deps: Vec<String>) -> TableManifest {
        TableManifest {
            ddl: format!("CREATE TABLE test (id UInt64) ENGINE = {}", engine),
            uuid: None,
            engine: engine.to_string(),
            total_bytes: 0,
            parts: HashMap::new(),
            pending_mutations: Vec::new(),
            metadata_only,
            dependencies: deps,
        }
    }

    fn make_manifest(tables: HashMap<String, TableManifest>) -> BackupManifest {
        BackupManifest {
            manifest_version: 1,
            name: "test-backup".to_string(),
            timestamp: Utc::now(),
            clickhouse_version: "24.1.0".to_string(),
            chbackup_version: "0.1.0".to_string(),
            data_format: "lz4".to_string(),
            compressed_size: 0,
            metadata_size: 0,
            disks: HashMap::new(),
            disk_types: HashMap::new(),
            disk_remote_paths: HashMap::new(),
            tables,
            databases: vec![DatabaseInfo {
                name: "default".to_string(),
                ddl: "CREATE DATABASE default ENGINE = Atomic".to_string(),
            }],
            functions: Vec::new(),
            named_collections: Vec::new(),
            rbac: None,
        }
    }

    #[test]
    fn test_engine_restore_priority() {
        // Design doc 5.1 specifies these priorities
        assert_eq!(engine_restore_priority("Dictionary"), 0);
        assert_eq!(engine_restore_priority("View"), 1);
        assert_eq!(engine_restore_priority("MaterializedView"), 1);
        assert_eq!(engine_restore_priority("LiveView"), 1);
        assert_eq!(engine_restore_priority("WindowView"), 1);
        assert_eq!(engine_restore_priority("Distributed"), 2);
        assert_eq!(engine_restore_priority("Merge"), 2);
        assert_eq!(engine_restore_priority("Null"), 3);
        assert_eq!(engine_restore_priority("MergeTree"), 3);
    }

    #[test]
    fn test_data_table_priority() {
        assert_eq!(data_table_priority("default.trades"), 0);
        assert_eq!(data_table_priority("default.users"), 0);
        assert_eq!(
            data_table_priority("default..inner_id.5f3a7b2c-1234"),
            1
        );
        assert_eq!(data_table_priority("default..inner.mv_target"), 1);
    }

    #[test]
    fn test_classify_restore_tables_basic() {
        let mut tables = HashMap::new();
        tables.insert(
            "default.trades".to_string(),
            make_table_manifest("MergeTree", false, vec![]),
        );
        tables.insert(
            "default.users".to_string(),
            make_table_manifest("MergeTree", false, vec![]),
        );
        tables.insert(
            "default.my_view".to_string(),
            make_table_manifest("View", true, vec![]),
        );
        tables.insert(
            "default.user_dict".to_string(),
            make_table_manifest("Dictionary", true, vec![]),
        );

        let manifest = make_manifest(tables);
        let all_keys: Vec<String> = manifest.tables.keys().cloned().collect();

        let phases = classify_restore_tables(&manifest, &all_keys);

        assert_eq!(phases.data_tables.len(), 2);
        assert_eq!(phases.ddl_only_tables.len(), 2);
        assert!(phases.postponed_tables.is_empty());

        // Data tables should contain trades and users
        assert!(phases.data_tables.contains(&"default.trades".to_string()));
        assert!(phases.data_tables.contains(&"default.users".to_string()));

        // DDL-only should contain view and dict
        assert!(
            phases
                .ddl_only_tables
                .contains(&"default.my_view".to_string())
        );
        assert!(
            phases
                .ddl_only_tables
                .contains(&"default.user_dict".to_string())
        );
    }

    #[test]
    fn test_classify_with_inner_tables() {
        let mut tables = HashMap::new();
        tables.insert(
            "default.trades".to_string(),
            make_table_manifest("MergeTree", false, vec![]),
        );
        tables.insert(
            "default..inner_id.abc123".to_string(),
            make_table_manifest("MergeTree", false, vec![]),
        );

        let manifest = make_manifest(tables);
        let all_keys: Vec<String> = manifest.tables.keys().cloned().collect();

        let phases = classify_restore_tables(&manifest, &all_keys);

        assert_eq!(phases.data_tables.len(), 2);
        // Regular tables should come before .inner tables (priority 0 < 1)
        assert_eq!(phases.data_tables[0], "default.trades");
        assert_eq!(phases.data_tables[1], "default..inner_id.abc123");
    }

    #[test]
    fn test_topological_sort_simple() {
        let mut tables = HashMap::new();
        // Dict depends on source table (but source table is in Phase 2, so external)
        // View B depends on View A
        tables.insert(
            "default.view_a".to_string(),
            make_table_manifest("View", true, vec![]),
        );
        tables.insert(
            "default.view_b".to_string(),
            make_table_manifest("View", true, vec!["default.view_a".to_string()]),
        );
        tables.insert(
            "default.user_dict".to_string(),
            make_table_manifest("Dictionary", true, vec!["default.users".to_string()]),
        );

        let keys = vec![
            "default.view_b".to_string(),
            "default.view_a".to_string(),
            "default.user_dict".to_string(),
        ];

        let sorted = topological_sort(&tables, &keys).unwrap();

        // user_dict has external dep only, so in-degree 0 within our set
        // view_a has no deps within set, so in-degree 0
        // view_b depends on view_a, so in-degree 1
        // Dictionary (priority 0) should come before View (priority 1) among zero-degree nodes
        assert_eq!(sorted[0], "default.user_dict");
        assert_eq!(sorted[1], "default.view_a");
        assert_eq!(sorted[2], "default.view_b");
    }

    #[test]
    fn test_topological_sort_cycle_detection() {
        let mut tables = HashMap::new();
        // Circular dependency: A -> B -> C -> A
        tables.insert(
            "default.a".to_string(),
            make_table_manifest("View", true, vec!["default.c".to_string()]),
        );
        tables.insert(
            "default.b".to_string(),
            make_table_manifest("View", true, vec!["default.a".to_string()]),
        );
        tables.insert(
            "default.c".to_string(),
            make_table_manifest("View", true, vec!["default.b".to_string()]),
        );

        let keys = vec![
            "default.a".to_string(),
            "default.b".to_string(),
            "default.c".to_string(),
        ];

        // Should NOT error -- cycles are broken with a warning
        let sorted = topological_sort(&tables, &keys).unwrap();

        // All three should be in the result
        assert_eq!(sorted.len(), 3);
        assert!(sorted.contains(&"default.a".to_string()));
        assert!(sorted.contains(&"default.b".to_string()));
        assert!(sorted.contains(&"default.c".to_string()));
    }

    #[test]
    fn test_topological_sort_empty_deps() {
        let mut tables = HashMap::new();
        // No dependencies at all -- should use engine-priority fallback
        tables.insert(
            "default.my_view".to_string(),
            make_table_manifest("View", true, vec![]),
        );
        tables.insert(
            "default.user_dict".to_string(),
            make_table_manifest("Dictionary", true, vec![]),
        );
        tables.insert(
            "default.dist_table".to_string(),
            make_table_manifest("Distributed", true, vec![]),
        );

        let keys = vec![
            "default.dist_table".to_string(),
            "default.my_view".to_string(),
            "default.user_dict".to_string(),
        ];

        let sorted = topological_sort(&tables, &keys).unwrap();

        // Engine priority: Dictionary(0) < View(1) < Distributed(2)
        assert_eq!(sorted[0], "default.user_dict");
        assert_eq!(sorted[1], "default.my_view");
        assert_eq!(sorted[2], "default.dist_table");
    }
}
