//! FreezeGuard pattern for safe FREEZE/UNFREEZE lifecycle.
//!
//! The FreezeGuard holds the metadata needed to UNFREEZE a table. Callers
//! MUST call `unfreeze()` explicitly since Drop is synchronous and cannot
//! await async operations.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::PathBuf;
use std::sync::Mutex;

use anyhow::Result;
use tracing::{debug, warn};
use walkdir::WalkDir;

use crate::clickhouse::client::ChClient;

/// What a single requested partition ID turned out to be after FREEZE PARTITION.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FreezeOutcome {
    /// Parts for the partition are staged in the shadow tree.
    Frozen,
    /// An operator-supplied `--partitions` ID matched nothing. That is a typo, and
    /// carrying on would produce a backup silently missing the requested data.
    FailExplicitZeroMatch { partition_id: String },
    /// A partition discovered from `system.parts` matched nothing. It may legitimately
    /// have been merged away between the query and the FREEZE, so this is non-fatal.
    WarnDiscoveryZeroMatch { partition_id: String },
}

/// Decide what a zero-match partition means.
///
/// `ALTER TABLE ... FREEZE PARTITION` is not a witness that anything was frozen:
/// ClickHouse's `MergeTreeData::freezePartitionsByMatcher` returns success with an empty
/// result when the matcher selects no partition. `evidence_present` must therefore come
/// from the staged shadow tree (see [`partitions_with_shadow_evidence`]), not from the
/// SQL result.
pub fn freeze_evidence_outcome(
    requested_partition: &str,
    evidence_present: bool,
    explicitly_requested: bool,
) -> FreezeOutcome {
    if evidence_present {
        FreezeOutcome::Frozen
    } else if explicitly_requested {
        FreezeOutcome::FailExplicitZeroMatch {
            partition_id: requested_partition.to_string(),
        }
    } else {
        FreezeOutcome::WarnDiscoveryZeroMatch {
            partition_id: requested_partition.to_string(),
        }
    }
}

/// Which of `requested` partition IDs have at least one part staged under this freeze.
///
/// Walks `{disk_path}/shadow/{freeze_name}` on every disk. Part directories sit at depth
/// four in both shadow layouts (`data/{db}/{table}/{part}` and
/// `store/{3char}/{uuid}/{part}`), and a part name always begins with
/// `{partition_id}_`. Blocking filesystem I/O -- call from `spawn_blocking`.
pub fn partitions_with_shadow_evidence(
    disk_paths: &BTreeMap<String, String>,
    freeze_name: &str,
    requested: &[String],
) -> HashSet<String> {
    let prefixes: Vec<(String, String)> = requested
        .iter()
        .map(|id| (id.clone(), format!("{id}_")))
        .collect();
    let mut found = HashSet::new();

    for disk_path in disk_paths.values() {
        let shadow_dir = PathBuf::from(disk_path).join("shadow").join(freeze_name);
        for entry in WalkDir::new(&shadow_dir)
            .min_depth(4)
            .max_depth(4)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_dir())
        {
            let name = entry.file_name().to_string_lossy();
            for (id, prefix) in &prefixes {
                if name.starts_with(prefix.as_str()) {
                    found.insert(id.clone());
                }
            }
        }
    }

    found
}

/// Metadata for a frozen table. Used to track what needs unfreezing.
///
/// Serializable because deferred freezes outlive the process that created them: the
/// entries are persisted in a deferred-freeze record so a later `upload` (or a recovery
/// run) can release exactly the freezes it owns. See `backup::deferred`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FreezeInfo {
    pub database: String,
    pub table: String,
    pub freeze_name: String,
}

/// Record a table's freeze in the registry the cleanup paths drain.
///
/// Call this the instant *any* FREEZE for the table succeeds -- the whole-table one, or each
/// `FREEZE PARTITION`. From that moment the table is frozen in ClickHouse regardless of what
/// the rest of the backup does, and every later step can fail: another partition's FREEZE can
/// return a non-ignorable error, the evidence scan can reject a mistyped ID, `collect_parts`
/// can fail. Recording at each success is what keeps a live freeze from being left with
/// nothing tracking it.
///
/// One entry per table -- the freeze name covers every partition -- so a repeat call for the
/// same table is a no-op rather than a duplicate the cleanup would try to unfreeze twice.
pub fn record_freeze(registry: &Mutex<Vec<FreezeInfo>>, info: &FreezeInfo) {
    let mut frozen = registry.lock().unwrap_or_else(|e| e.into_inner());
    if !frozen.contains(info) {
        frozen.push(info.clone());
    }
}

/// Verify that the partitions frozen under `info` actually staged parts.
///
/// `requested` holds only the IDs whose `FREEZE PARTITION` succeeded; the caller must already
/// have recorded the freeze with [`record_freeze`]. Returns the subset of `requested` that has
/// parts staged. Blocking filesystem I/O -- call from `spawn_blocking`.
///
/// # Why an explicit zero-match is not fatal *here*
///
/// A `--partitions` list spans the whole backup, not one table, so an ID that stages nothing in
/// *this* table is perfectly normal: `--partitions all,202401` against a partitioned and an
/// unpartitioned table can never have both IDs apply to both tables. Failing per-table made
/// mixed lists unusable.
///
/// The typo signal is an ID that stages nothing in **any** table, which only the caller can
/// determine. So this function reports evidence and the caller aggregates across tables --
/// see `unmatched_explicit_partitions`.
pub fn verify_partitions_staged(
    info: &FreezeInfo,
    disk_paths: &BTreeMap<String, String>,
    requested: &[String],
    explicitly_requested: bool,
) -> Result<BTreeSet<String>> {
    let with_evidence = partitions_with_shadow_evidence(disk_paths, &info.freeze_name, requested);

    let mut staged = BTreeSet::new();
    for partition_id in requested {
        match freeze_evidence_outcome(
            partition_id,
            with_evidence.contains(partition_id),
            explicitly_requested,
        ) {
            FreezeOutcome::Frozen => {
                staged.insert(partition_id.clone());
            }
            FreezeOutcome::FailExplicitZeroMatch { partition_id } => {
                // Not fatal per-table; the caller decides once it has seen every table.
                debug!(
                    db = %info.database,
                    table = %info.table,
                    partition = %partition_id,
                    "requested partition staged nothing in this table (may apply to another)"
                );
            }
            FreezeOutcome::WarnDiscoveryZeroMatch { partition_id } => {
                warn!(
                    db = %info.database,
                    table = %info.table,
                    partition = %partition_id,
                    "freeze_by_part: FREEZE PARTITION staged no parts (the partition was \
                     likely merged away since system.parts was queried)"
                );
            }
        }
    }

    Ok(staged)
}

/// Which explicitly requested partition IDs staged nothing anywhere in the backup.
///
/// This is the typo check, and it is deliberately backup-wide rather than per-table: an ID that
/// misses one table may legitimately match another, but an ID that matches nothing at all means
/// the operator asked for data that does not exist -- and carrying on would hand back a backup
/// silently missing what they asked for.
pub fn unmatched_explicit_partitions(
    requested: &[String],
    staged_anywhere: &BTreeSet<String>,
) -> Vec<String> {
    requested
        .iter()
        .filter(|id| !staged_anywhere.contains(*id))
        .cloned()
        .collect()
}

/// Guard holding references to frozen tables. The caller MUST call
/// `unfreeze_all()` to release the freeze. If not called, the frozen
/// data will remain in the shadow directory until manually cleaned.
pub struct FreezeGuard {
    frozen: Vec<FreezeInfo>,
}

impl Default for FreezeGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl FreezeGuard {
    /// Create a new empty FreezeGuard.
    pub fn new() -> Self {
        Self { frozen: Vec::new() }
    }

    /// Record that a table has been frozen.
    pub fn add(&mut self, info: FreezeInfo) {
        self.frozen.push(info);
    }

    /// Get the list of frozen tables.
    pub fn frozen_tables(&self) -> &[FreezeInfo] {
        &self.frozen
    }

    /// Number of frozen tables.
    pub fn len(&self) -> usize {
        self.frozen.len()
    }

    /// Whether there are any frozen tables.
    pub fn is_empty(&self) -> bool {
        self.frozen.is_empty()
    }

    /// Build a guard from an existing set of entries.
    ///
    /// Used when adopting a deferred freeze recorded by an earlier process.
    pub fn from_frozen(frozen: Vec<FreezeInfo>) -> Self {
        Self { frozen }
    }

    /// Remove and return every entry matching `pred`, leaving the rest in the guard.
    ///
    /// Used to split local-disk tables (unfrozen immediately) from S3 object-disk tables
    /// (whose freeze must be held until their remote objects have been copied).
    pub fn take_matching<F>(&mut self, mut pred: F) -> Vec<FreezeInfo>
    where
        F: FnMut(&FreezeInfo) -> bool,
    {
        let mut taken = Vec::new();
        let mut kept = Vec::with_capacity(self.frozen.len());
        for info in self.frozen.drain(..) {
            if pred(&info) {
                taken.push(info);
            } else {
                kept.push(info);
            }
        }
        self.frozen = kept;
        taken
    }

    /// Unfreeze all tables. Logs warnings on failure but does not fail
    /// the whole operation -- leftover shadow data can be cleaned later.
    ///
    /// Prefer [`FreezeGuard::unfreeze_all_checked`] when the caller needs to know whether
    /// the freeze was actually released -- e.g. before deleting a deferred-freeze record,
    /// where silently dropping the record while a table stays frozen leaks shadow data.
    pub async fn unfreeze_all(&mut self, ch: &ChClient) -> Result<()> {
        let _ = self.unfreeze_all_checked(ch).await;
        Ok(())
    }

    /// Unfreeze all tables, reporting which ones could not be released.
    ///
    /// Returns `Ok(())` when every table was unfrozen. On partial failure returns
    /// `Err(remaining)` with the entries that are still frozen; those are also retained in
    /// the guard so a caller can persist them for a later retry. Successfully unfrozen
    /// entries are always removed.
    pub async fn unfreeze_all_checked(
        &mut self,
        ch: &ChClient,
    ) -> std::result::Result<(), Vec<FreezeInfo>> {
        let mut failed = Vec::new();

        for info in self.frozen.drain(..) {
            debug!(
                db = %info.database,
                table = %info.table,
                freeze_name = %info.freeze_name,
                "Unfreezing table"
            );

            if let Err(e) = ch
                .unfreeze_table(&info.database, &info.table, &info.freeze_name)
                .await
            {
                warn!(
                    db = %info.database,
                    table = %info.table,
                    error = %e,
                    "Failed to UNFREEZE table (shadow data may need manual cleanup)"
                );
                failed.push(info);
            }
        }

        if failed.is_empty() {
            Ok(())
        } else {
            // Keep the failures in the guard so the caller can re-persist them.
            self.frozen = failed.clone();
            Err(failed)
        }
    }
}

impl Drop for FreezeGuard {
    fn drop(&mut self) {
        if !self.frozen.is_empty() {
            warn!(
                count = self.frozen.len(),
                "FreezeGuard dropped with unfrozen tables -- shadow data may remain. \
                 Run `chbackup clean` to remove leftover shadow directories"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_freeze_guard_new_is_empty() {
        let guard = FreezeGuard::new();
        assert!(guard.is_empty());
        assert_eq!(guard.len(), 0);
        assert!(guard.frozen_tables().is_empty());
    }

    #[test]
    fn test_freeze_guard_default_is_empty() {
        let guard = FreezeGuard::default();
        assert!(guard.is_empty());
    }

    #[test]
    fn test_freeze_guard_add_and_len() {
        let mut guard = FreezeGuard::new();
        guard.add(FreezeInfo {
            database: "default".to_string(),
            table: "trades".to_string(),
            freeze_name: "chbackup_test_default_trades".to_string(),
        });
        assert!(!guard.is_empty());
        assert_eq!(guard.len(), 1);

        guard.add(FreezeInfo {
            database: "default".to_string(),
            table: "users".to_string(),
            freeze_name: "chbackup_test_default_users".to_string(),
        });
        assert_eq!(guard.len(), 2);
    }

    #[test]
    fn test_freeze_guard_frozen_tables() {
        let mut guard = FreezeGuard::new();
        guard.add(FreezeInfo {
            database: "db1".to_string(),
            table: "t1".to_string(),
            freeze_name: "fn1".to_string(),
        });
        guard.add(FreezeInfo {
            database: "db2".to_string(),
            table: "t2".to_string(),
            freeze_name: "fn2".to_string(),
        });

        let tables = guard.frozen_tables();
        assert_eq!(tables.len(), 2);
        assert_eq!(tables[0].database, "db1");
        assert_eq!(tables[0].table, "t1");
        assert_eq!(tables[1].database, "db2");
        assert_eq!(tables[1].table, "t2");
    }

    #[test]
    fn test_freeze_guard_drop_when_empty_does_not_panic() {
        let guard = FreezeGuard::new();
        drop(guard);
        // No panic means success
    }

    #[test]
    fn test_freeze_guard_drop_when_non_empty_does_not_panic() {
        let mut guard = FreezeGuard::new();
        guard.add(FreezeInfo {
            database: "default".to_string(),
            table: "trades".to_string(),
            freeze_name: "test".to_string(),
        });
        // Drop should log a warning but not panic
        drop(guard);
    }

    #[test]
    fn test_take_matching_splits_and_keeps_remainder() {
        let mut guard = FreezeGuard::new();
        for (db, t) in [("a", "s3tbl"), ("a", "localtbl"), ("b", "s3tbl")] {
            guard.add(FreezeInfo {
                database: db.to_string(),
                table: t.to_string(),
                freeze_name: crate::clickhouse::freeze_name("bk", db, t),
            });
        }

        let taken = guard.take_matching(|i| i.table == "s3tbl");

        assert_eq!(taken.len(), 2, "both s3tbl entries taken");
        assert!(taken.iter().all(|i| i.table == "s3tbl"));
        assert_eq!(guard.len(), 1, "local table stays in the guard");
        assert_eq!(guard.frozen_tables()[0].table, "localtbl");
    }

    #[test]
    fn test_take_matching_none_and_all() {
        let mut guard = FreezeGuard::new();
        guard.add(FreezeInfo {
            database: "a".into(),
            table: "t".into(),
            freeze_name: "f".into(),
        });

        assert!(guard.take_matching(|_| false).is_empty());
        assert_eq!(guard.len(), 1, "nothing taken means nothing lost");

        assert_eq!(guard.take_matching(|_| true).len(), 1);
        assert!(guard.is_empty());
    }

    #[test]
    fn test_from_frozen_roundtrips() {
        let entries = vec![FreezeInfo {
            database: "db".into(),
            table: "t".into(),
            freeze_name: "f".into(),
        }];
        let guard = FreezeGuard::from_frozen(entries.clone());
        assert_eq!(guard.frozen_tables(), entries.as_slice());
    }

    #[test]
    fn freeze_evidence_present_is_frozen() {
        assert_eq!(
            freeze_evidence_outcome("202401", true, true),
            FreezeOutcome::Frozen
        );
        assert_eq!(
            freeze_evidence_outcome("202401", true, false),
            FreezeOutcome::Frozen,
            "evidence wins regardless of where the ID came from"
        );
    }

    #[test]
    fn freeze_evidence_absent_for_explicit_id_fails() {
        assert_eq!(
            freeze_evidence_outcome("20240i", false, true),
            FreezeOutcome::FailExplicitZeroMatch {
                partition_id: "20240i".to_string()
            },
            "a mistyped --partitions value must fail rather than back up nothing"
        );
    }

    #[test]
    fn freeze_evidence_absent_during_discovery_warns() {
        assert_eq!(
            freeze_evidence_outcome("202401", false, false),
            FreezeOutcome::WarnDiscoveryZeroMatch {
                partition_id: "202401".to_string()
            },
            "a discovered partition may have been merged away -- non-fatal"
        );
    }

    #[test]
    fn freeze_evidence_scan_finds_staged_partitions() {
        let tmp = tempfile::tempdir().unwrap();
        let disk = tmp.path().join("disk1");
        let shadow = disk.join("shadow").join("chbackup_bk__db__t");
        // Ordinary layout: data/{db}/{table}/{part}
        std::fs::create_dir_all(shadow.join("data/db/t/202401_1_1_0")).unwrap();
        // Atomic layout: store/{3char}/{uuid}/{part}
        std::fs::create_dir_all(shadow.join("store/abc/abcdef/202402_5_5_0_7")).unwrap();

        let disks = BTreeMap::from([("disk1".to_string(), disk.display().to_string())]);
        let found = partitions_with_shadow_evidence(
            &disks,
            "chbackup_bk__db__t",
            &[
                "202401".to_string(),
                "202402".to_string(),
                "202403".to_string(),
            ],
        );

        assert_eq!(
            found,
            HashSet::from(["202401".to_string(), "202402".to_string()]),
            "only partitions with a staged part count as evidence"
        );
    }

    #[test]
    fn freeze_evidence_scan_on_missing_shadow_is_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let disks = BTreeMap::from([("disk1".to_string(), tmp.path().display().to_string())]);

        assert!(
            partitions_with_shadow_evidence(&disks, "never_frozen", &["202401".to_string()])
                .is_empty()
        );
    }

    /// A staged part for `partition_id`, in a shadow tree the evidence scan will walk.
    fn stage_part(disk: &std::path::Path, freeze_name: &str, partition_id: &str) {
        std::fs::create_dir_all(
            disk.join("shadow")
                .join(freeze_name)
                .join("data/db/t")
                .join(format!("{partition_id}_1_1_0")),
        )
        .unwrap();
    }

    fn test_info() -> FreezeInfo {
        FreezeInfo {
            database: "db".to_string(),
            table: "t".to_string(),
            freeze_name: "chbackup__db__t".to_string(),
        }
    }

    #[test]
    fn freeze_leak_one_frozen_partition_registers_the_table_immediately() {
        let registry = Mutex::new(Vec::new());
        let info = test_info();

        // The FREEZE of the first partition succeeded. Whatever happens to the partitions
        // after it -- a non-ignorable error such as 248 INVALID_PARTITION_VALUE aborting the
        // table's task before any verification runs -- the table is frozen now and cleanup
        // has to be able to find it.
        record_freeze(&registry, &info);

        assert_eq!(
            registry.lock().unwrap().as_slice(),
            std::slice::from_ref(&info),
            "the table must be tracked from its first successful FREEZE, not once the \
             whole partition loop has finished"
        );

        // Later partitions of the same table freeze under the same name.
        record_freeze(&registry, &info);
        assert_eq!(
            registry.lock().unwrap().len(),
            1,
            "one entry per table -- a duplicate would be unfrozen twice"
        );
    }

    #[test]
    fn freeze_leak_rejected_partition_leaves_the_table_registered() {
        let tmp = tempfile::tempdir().unwrap();
        let disks = BTreeMap::from([("disk1".to_string(), tmp.path().display().to_string())]);
        let registry = Mutex::new(Vec::new());
        let info = test_info();

        record_freeze(&registry, &info);
        // Nothing staged for an operator-supplied ID. This is NO LONGER an error here: the ID
        // may still match another table, so only the backup-wide check can call it a typo (see
        // unmatched_explicit_partitions). What must hold is that the table stays registered,
        // because it is still frozen and something has to unfreeze it.
        let staged =
            verify_partitions_staged(&info, &disks, &["20240i".to_string()], true).unwrap();

        assert!(
            staged.is_empty(),
            "an ID that staged nothing must not be reported as staged"
        );
        assert_eq!(
            registry.into_inner().unwrap().as_slice(),
            std::slice::from_ref(&info),
            "verification does not unregister -- the table is still frozen afterwards, and the \
             caller unfreezes it before bailing on the backup-wide check"
        );
    }

    #[test]
    fn freeze_evidence_staged_partition_is_counted() {
        let tmp = tempfile::tempdir().unwrap();
        let info = test_info();
        stage_part(tmp.path(), &info.freeze_name, "202401");
        let disks = BTreeMap::from([("disk1".to_string(), tmp.path().display().to_string())]);

        let staged =
            verify_partitions_staged(&info, &disks, &["202401".to_string()], true).unwrap();

        assert_eq!(staged, BTreeSet::from(["202401".to_string()]));
    }

    #[test]
    fn freeze_evidence_discovered_partition_with_no_parts_is_not_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let info = test_info();
        stage_part(tmp.path(), &info.freeze_name, "202401");
        let disks = BTreeMap::from([("disk1".to_string(), tmp.path().display().to_string())]);

        let staged = verify_partitions_staged(
            &info,
            &disks,
            &["202401".to_string(), "202402".to_string()],
            false,
        )
        .unwrap();

        assert_eq!(
            staged,
            BTreeSet::from(["202401".to_string()]),
            "a discovered partition merged away since system.parts was queried only warns"
        );
    }

    /// The regression this fixes: an explicitly requested ID that misses THIS table must not be
    /// fatal, because a --partitions list spans the backup. `all,202401` against an
    /// unpartitioned table stages `all` and misses `202401`, which is normal.
    #[test]
    fn freeze_evidence_explicit_miss_on_one_table_is_not_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let info = test_info();
        stage_part(tmp.path(), &info.freeze_name, "all");
        let disks = BTreeMap::from([("disk1".to_string(), tmp.path().display().to_string())]);

        let staged = verify_partitions_staged(
            &info,
            &disks,
            &["all".to_string(), "202401".to_string()],
            true,
        )
        .expect("a per-table miss must not fail the backup");

        assert_eq!(
            staged,
            BTreeSet::from(["all".to_string()]),
            "only the ID that actually staged parts is reported"
        );
    }

    #[test]
    fn unmatched_explicit_partitions_identifies_only_global_misses() {
        let requested = vec![
            "all".to_string(),
            "202401".to_string(),
            "199001".to_string(),
        ];

        // Union across tables: the unpartitioned table staged "all", the partitioned one
        // staged "202401". Neither staged "199001" -- that is the typo.
        let staged_anywhere = BTreeSet::from(["all".to_string(), "202401".to_string()]);
        assert_eq!(
            unmatched_explicit_partitions(&requested, &staged_anywhere),
            vec!["199001".to_string()]
        );

        // Everything matched somewhere -> nothing to report.
        let all_staged = BTreeSet::from([
            "all".to_string(),
            "202401".to_string(),
            "199001".to_string(),
        ]);
        assert!(unmatched_explicit_partitions(&requested, &all_staged).is_empty());

        // Nothing matched -> every ID is reported, in request order.
        assert_eq!(
            unmatched_explicit_partitions(&requested, &BTreeSet::new()),
            requested
        );
    }

    #[test]
    fn test_freeze_info_clone() {
        let info = FreezeInfo {
            database: "mydb".to_string(),
            table: "mytable".to_string(),
            freeze_name: "myfreeze".to_string(),
        };
        let cloned = info.clone();
        assert_eq!(cloned.database, "mydb");
        assert_eq!(cloned.table, "mytable");
        assert_eq!(cloned.freeze_name, "myfreeze");
    }
}
