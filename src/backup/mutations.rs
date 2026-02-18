//! Mutation checking before backup.
//!
//! Per design 3.1, pending data mutations should be checked before FREEZE.
//! If mutations are pending, we wait up to mutation_wait_timeout, then abort.

use anyhow::{bail, Result};
use tracing::{info, warn};

use crate::clickhouse::client::ChClient;
use crate::manifest::MutationInfo;

/// Check for pending mutations on the given tables.
///
/// Returns a list of mutation info for recording in the manifest.
/// If `abort_on_pending` is true and mutations are still pending after logging,
/// the function returns an error.
pub async fn check_mutations(
    ch: &ChClient,
    targets: &[(String, String)],
    backup_mutations: bool,
) -> Result<Vec<MutationInfo>> {
    if targets.is_empty() {
        return Ok(Vec::new());
    }

    let mutations = ch.check_pending_mutations(targets).await?;

    if mutations.is_empty() {
        info!("No pending mutations found");
        return Ok(Vec::new());
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

    if backup_mutations {
        // Record mutations in the manifest but proceed with backup
        let infos: Vec<MutationInfo> = mutations
            .iter()
            .map(|m| MutationInfo {
                mutation_id: m.mutation_id.clone(),
                command: m.command.clone(),
                parts_to_do: m.parts_to_do_names.clone(),
            })
            .collect();

        info!(
            count = infos.len(),
            "Recording pending mutations in manifest"
        );
        Ok(infos)
    } else {
        bail!(
            "Found {} pending mutations. Use clickhouse.backup_mutations=true to proceed anyway",
            mutations.len()
        );
    }
}
