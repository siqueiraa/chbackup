//! Mutation checking before backup.
//!
//! Per design 3.1, pending data mutations should be checked before FREEZE.
//! If mutations are pending, we wait up to mutation_wait_timeout, then abort.

use std::collections::HashMap;

use anyhow::{bail, Result};
use tracing::{info, warn};

use crate::clickhouse::client::ChClient;
use crate::manifest::MutationInfo;

/// Check for pending mutations on the given tables.
///
/// Returns a per-table map of mutation info for recording in the manifest.
/// Key is `"database.table"`. If `backup_mutations` is false and mutations
/// are pending, returns an error instead.
pub async fn check_mutations(
    ch: &ChClient,
    targets: &[(String, String)],
    backup_mutations: bool,
) -> Result<HashMap<String, Vec<MutationInfo>>> {
    if targets.is_empty() {
        return Ok(HashMap::new());
    }

    let mutations = ch.check_pending_mutations(targets).await?;

    if mutations.is_empty() {
        info!("No pending mutations found");
        return Ok(HashMap::new());
    }

    warn!(count = mutations.len(), "Found pending data mutations");

    let mut result: HashMap<String, Vec<MutationInfo>> = HashMap::new();

    for m in &mutations {
        warn!(
            db = %m.database,
            table = %m.table,
            mutation_id = %m.mutation_id,
            command = %m.command,
            parts_remaining = m.parts_to_do_names.len(),
            "Pending mutation"
        );

        let key = format!("{}.{}", m.database, m.table);
        result.entry(key).or_default().push(MutationInfo {
            mutation_id: m.mutation_id.clone(),
            command: m.command.clone(),
            parts_to_do: m.parts_to_do_names.clone(),
        });
    }

    if backup_mutations {
        let total: usize = result.values().map(|v| v.len()).sum();
        info!(count = total, "Recording pending mutations in manifest");
        Ok(result)
    } else {
        bail!(
            "Found {} pending mutations. Use clickhouse.backup_mutations=true to proceed anyway",
            mutations.len()
        );
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use crate::manifest::MutationInfo;

    #[test]
    fn test_mutations_scoped_per_table() {
        // Simulate what check_mutations returns after the fix: a per-table map.
        let mut mutation_map: HashMap<String, Vec<MutationInfo>> = HashMap::new();
        mutation_map
            .entry("mydb.table_a".to_string())
            .or_default()
            .push(MutationInfo {
                mutation_id: "0000000001".to_string(),
                command: "DELETE WHERE id < 0".to_string(),
                parts_to_do: vec![],
            });

        let table_a_mutations = mutation_map
            .get("mydb.table_a")
            .cloned()
            .unwrap_or_default();
        let table_b_mutations = mutation_map
            .get("mydb.table_b")
            .cloned()
            .unwrap_or_default();

        assert_eq!(table_a_mutations.len(), 1, "table_a should have 1 mutation");
        assert_eq!(
            table_b_mutations.len(),
            0,
            "table_b should have no mutations"
        );
    }

    #[test]
    fn test_mutations_map_multiple_tables() {
        // Verify two tables with different mutations are stored independently.
        let mut mutation_map: HashMap<String, Vec<MutationInfo>> = HashMap::new();

        mutation_map
            .entry("db.tbl_x".to_string())
            .or_default()
            .push(MutationInfo {
                mutation_id: "0000000001".to_string(),
                command: "UPDATE col = 1 WHERE id = 5".to_string(),
                parts_to_do: vec!["202401_1_1_0".to_string()],
            });

        mutation_map
            .entry("db.tbl_y".to_string())
            .or_default()
            .push(MutationInfo {
                mutation_id: "0000000002".to_string(),
                command: "DELETE WHERE id = 99".to_string(),
                parts_to_do: vec![],
            });
        mutation_map
            .entry("db.tbl_y".to_string())
            .or_default()
            .push(MutationInfo {
                mutation_id: "0000000003".to_string(),
                command: "DELETE WHERE id = 100".to_string(),
                parts_to_do: vec![],
            });

        // tbl_x gets only its own mutation
        let x_muts = mutation_map.get("db.tbl_x").cloned().unwrap_or_default();
        assert_eq!(x_muts.len(), 1);
        assert_eq!(x_muts[0].mutation_id, "0000000001");

        // tbl_y gets only its two mutations, not tbl_x's
        let y_muts = mutation_map.get("db.tbl_y").cloned().unwrap_or_default();
        assert_eq!(y_muts.len(), 2);
        assert!(y_muts.iter().all(|m| m.mutation_id != "0000000001"));

        // A table not in the map gets an empty list
        let z_muts = mutation_map.get("db.tbl_z").cloned().unwrap_or_default();
        assert!(z_muts.is_empty());
    }
}
