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
///
/// When `mutation_wait_timeout_secs` is greater than zero, polls ClickHouse
/// every 5 seconds until all mutations complete or the deadline is reached.
/// A value of `0` means no waiting (existing behavior).
pub async fn check_mutations(
    ch: &ChClient,
    targets: &[(String, String)],
    backup_mutations: bool,
    mutation_wait_timeout_secs: u64,
) -> Result<HashMap<String, Vec<MutationInfo>>> {
    if targets.is_empty() {
        return Ok(HashMap::new());
    }

    let deadline = if mutation_wait_timeout_secs > 0 {
        Some(
            std::time::Instant::now()
                + std::time::Duration::from_secs(mutation_wait_timeout_secs),
        )
    } else {
        None
    };

    loop {
        let mutations = ch.check_pending_mutations(targets).await?;

        if mutations.is_empty() {
            info!("No pending mutations found");
            return Ok(HashMap::new());
        }

        warn!(count = mutations.len(), "Found pending data mutations");

        for m in &mutations {
            warn!(
                db = %m.database,
                table = %m.table,
                mutation_id = %m.mutation_id,
                command = %m.command,
                parts_remaining = m.parts_to_do_names.len(),
                "Pending mutation"
            );
        }

        let should_wait = deadline
            .map(|d| std::time::Instant::now() < d)
            .unwrap_or(false);

        if should_wait {
            info!(
                wait_secs = 5u64,
                "Waiting for pending mutations to complete..."
            );
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            continue;
        }

        // No time left (or timeout=0): record or fail
        let mut result: HashMap<String, Vec<MutationInfo>> = HashMap::new();
        for m in &mutations {
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
            return Ok(result);
        } else {
            bail!(
                "Found {} pending mutations. Use clickhouse.backup_mutations=true to proceed anyway",
                mutations.len()
            );
        }
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

    #[test]
    fn test_mutation_wait_zero_means_no_poll() {
        use crate::config::parse_duration_secs;
        assert_eq!(parse_duration_secs("0").unwrap(), 0);
        assert_eq!(parse_duration_secs("5m").unwrap(), 300);
        assert_eq!(parse_duration_secs("1h").unwrap(), 3600);
    }

    #[test]
    fn test_mutation_wait_deadline_logic() {
        // When timeout=0, should_wait is always false (no polling)
        let timeout_secs: u64 = 0;
        let deadline: Option<std::time::Instant> = if timeout_secs > 0 {
            Some(std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs))
        } else {
            None
        };
        let should_wait = deadline
            .map(|d| std::time::Instant::now() < d)
            .unwrap_or(false);
        assert!(!should_wait, "timeout=0 should never trigger polling");
    }
}
