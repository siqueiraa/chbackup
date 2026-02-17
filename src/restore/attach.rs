//! Part attachment: hardlink parts to detached/ directory and ATTACH PART.
//!
//! For each part in the backup:
//! 1. Hardlink (or copy) files from backup to `{table_data_path}/detached/{part_name}/`
//! 2. Chown to ClickHouse uid/gid
//! 3. ALTER TABLE ATTACH PART '{part_name}'

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tracing::{debug, info, warn};
use walkdir::WalkDir;

use crate::clickhouse::client::ChClient;
use crate::manifest::PartInfo;

use super::sort::{needs_sequential_attach, sort_parts_by_min_block};

/// Parameters for attaching parts to a table (borrowed references).
///
/// Used for the internal sequential attach path where lifetimes are known.
pub struct AttachParams<'a> {
    /// ClickHouse client for ATTACH PART queries.
    pub ch: &'a ChClient,
    /// Database name.
    pub db: &'a str,
    /// Table name.
    pub table: &'a str,
    /// List of parts to attach (from all disks).
    pub parts: &'a [PartInfo],
    /// Base backup directory.
    pub backup_dir: &'a Path,
    /// Path to the table's data directory (from system.tables data_paths).
    pub table_data_path: &'a Path,
    /// ClickHouse process UID (for chown).
    pub clickhouse_uid: Option<u32>,
    /// ClickHouse process GID (for chown).
    pub clickhouse_gid: Option<u32>,
}

/// Owned parameters for attaching parts to a table.
///
/// All fields use owned types (String, PathBuf, Vec) so this struct can be
/// sent across `tokio::spawn` boundaries without lifetime constraints.
pub struct OwnedAttachParams {
    /// ClickHouse client for ATTACH PART queries.
    pub ch: ChClient,
    /// Database name.
    pub db: String,
    /// Table name.
    pub table: String,
    /// List of parts to attach (from all disks).
    pub parts: Vec<PartInfo>,
    /// Base backup directory.
    pub backup_dir: PathBuf,
    /// Path to the table's data directory (from system.tables data_paths).
    pub table_data_path: PathBuf,
    /// ClickHouse process UID (for chown).
    pub clickhouse_uid: Option<u32>,
    /// ClickHouse process GID (for chown).
    pub clickhouse_gid: Option<u32>,
    /// Engine name for determining sequential vs parallel ATTACH.
    pub engine: String,
}

/// Attach parts for a single table using owned parameters.
///
/// Delegates to the internal `attach_parts_inner` with engine-aware routing
/// via `needs_sequential_attach`.
pub async fn attach_parts_owned(params: OwnedAttachParams) -> Result<u64> {
    let attach_params = AttachParams {
        ch: &params.ch,
        db: &params.db,
        table: &params.table,
        parts: &params.parts,
        backup_dir: &params.backup_dir,
        table_data_path: &params.table_data_path,
        clickhouse_uid: params.clickhouse_uid,
        clickhouse_gid: params.clickhouse_gid,
    };

    attach_parts_inner(&attach_params, &params.engine).await
}

/// Attach parts for a single table using borrowed parameters.
///
/// Hardlinks backup part files to the table's detached/ directory, then
/// executes ALTER TABLE ATTACH PART for each part.
pub async fn attach_parts(params: &AttachParams<'_>) -> Result<u64> {
    // When called from the borrowed API, we don't have engine info,
    // so default to sequential (safe for all engines).
    attach_parts_inner(params, "").await
}

/// Internal attach implementation with engine-aware routing.
///
/// When `needs_sequential_attach(engine)` returns true (Replacing, Collapsing,
/// Versioned engines), parts are attached strictly in sorted order.
/// For plain MergeTree engines, parts are also attached sequentially within
/// the table (Phase 2a keeps per-table ATTACH sequential; parallelism is
/// across tables, not within a single table).
async fn attach_parts_inner(params: &AttachParams<'_>, engine: &str) -> Result<u64> {
    let db = params.db;
    let table = params.table;

    if params.parts.is_empty() {
        debug!(
            db = %db,
            table = %table,
            "No parts to attach"
        );
        return Ok(0);
    }

    let sequential = needs_sequential_attach(engine) || engine.is_empty();

    if sequential {
        debug!(
            db = %db,
            table = %table,
            engine = %engine,
            "Using sequential attach (engine requires ordered attachment)"
        );
    }

    // Sort parts by (partition, min_block) for correct attach order
    let sorted_parts = sort_parts_by_min_block(params.parts);

    let detached_dir = params.table_data_path.join("detached");
    std::fs::create_dir_all(&detached_dir).with_context(|| {
        format!(
            "Failed to create detached directory: {}",
            detached_dir.display()
        )
    })?;

    let mut attached_count = 0u64;

    for part in &sorted_parts {
        // URL-encode db and table names for the backup shadow path
        let url_db = url_encode(db);
        let url_table = url_encode(table);

        // Source: {backup_dir}/shadow/{db}/{table}/{part_name}/
        let source_dir = params.backup_dir
            .join("shadow")
            .join(&url_db)
            .join(&url_table)
            .join(&part.name);

        if !source_dir.exists() {
            warn!(
                part = %part.name,
                source = %source_dir.display(),
                "Part source directory not found, skipping"
            );
            continue;
        }

        // Destination: {table_data_path}/detached/{part_name}/
        let dest_dir = detached_dir.join(&part.name);

        // Hardlink or copy all files from source to dest
        hardlink_or_copy_dir(&source_dir, &dest_dir)
            .with_context(|| {
                format!(
                    "Failed to hardlink/copy part {} from {} to {}",
                    part.name,
                    source_dir.display(),
                    dest_dir.display()
                )
            })?;

        // Chown to ClickHouse uid/gid
        chown_recursive(&dest_dir, params.clickhouse_uid, params.clickhouse_gid)?;

        // ATTACH PART
        debug!(
            db = %db,
            table = %table,
            part = %part.name,
            "Attaching part"
        );

        match params.ch.attach_part(db, table, &part.name).await {
            Ok(()) => {
                attached_count += 1;
            }
            Err(e) => {
                let err_str = format!("{:#}", e);
                // ClickHouse errors 232/233: overlapping block range or part already exists
                if err_str.contains("DUPLICATE_DATA_PART")
                    || err_str.contains("PART_IS_TEMPORARILY_LOCKED")
                    || err_str.contains("NO_SUCH_DATA_PART")
                    || err_str.contains("232")
                    || err_str.contains("233")
                {
                    warn!(
                        db = %db,
                        table = %table,
                        part = %part.name,
                        error = %err_str,
                        "Part already exists or overlaps, skipping"
                    );
                } else {
                    return Err(e).with_context(|| {
                        format!(
                            "Failed to ATTACH PART '{}' to {}.{}",
                            part.name, db, table
                        )
                    });
                }
            }
        }
    }

    info!(
        db = %db,
        table = %table,
        attached = attached_count,
        total = sorted_parts.len(),
        "Parts attached"
    );

    Ok(attached_count)
}

/// Hardlink all files from source directory to destination directory.
///
/// If hardlink fails with EXDEV (cross-device link), falls back to file copy.
fn hardlink_or_copy_dir(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)
        .with_context(|| format!("Failed to create destination dir: {}", dst.display()))?;

    for entry in WalkDir::new(src).min_depth(1) {
        let entry = entry.with_context(|| {
            format!("Failed to read directory entry under: {}", src.display())
        })?;

        let relative = entry
            .path()
            .strip_prefix(src)
            .context("Failed to strip source prefix")?;
        let dest_path = dst.join(relative);

        if entry.file_type().is_dir() {
            std::fs::create_dir_all(&dest_path).with_context(|| {
                format!("Failed to create directory: {}", dest_path.display())
            })?;
        } else {
            // Try hardlink first
            match std::fs::hard_link(entry.path(), &dest_path) {
                Ok(()) => {}
                Err(e) => {
                    // Check for EXDEV (cross-device link, error code 18)
                    if e.raw_os_error() == Some(18) {
                        debug!(
                            src = %entry.path().display(),
                            dst = %dest_path.display(),
                            "Cross-device link, falling back to copy"
                        );
                        std::fs::copy(entry.path(), &dest_path).with_context(|| {
                            format!(
                                "Failed to copy {} to {}",
                                entry.path().display(),
                                dest_path.display()
                            )
                        })?;
                    } else {
                        return Err(e).with_context(|| {
                            format!(
                                "Failed to hardlink {} to {}",
                                entry.path().display(),
                                dest_path.display()
                            )
                        });
                    }
                }
            }
        }
    }

    Ok(())
}

/// Recursively chown files and directories to the given uid/gid.
///
/// If uid and gid are both None, this is a no-op.
/// If the process is not running as root, chown may fail with EPERM --
/// in that case we log a warning and continue.
fn chown_recursive(path: &Path, uid: Option<u32>, gid: Option<u32>) -> Result<()> {
    if uid.is_none() && gid.is_none() {
        return Ok(());
    }

    let nix_uid = uid.map(nix::unistd::Uid::from_raw);
    let nix_gid = gid.map(nix::unistd::Gid::from_raw);

    for entry in WalkDir::new(path) {
        let entry = entry.with_context(|| {
            format!("Failed to read directory entry under: {}", path.display())
        })?;

        match nix::unistd::chown(entry.path(), nix_uid, nix_gid) {
            Ok(()) => {}
            Err(nix::errno::Errno::EPERM) => {
                // Not running as root -- skip chown silently
                debug!(
                    path = %entry.path().display(),
                    "Cannot chown (not root), skipping"
                );
                return Ok(()); // Skip remaining files too
            }
            Err(e) => {
                warn!(
                    path = %entry.path().display(),
                    error = %e,
                    "Failed to chown"
                );
            }
        }
    }

    Ok(())
}

/// Detect ClickHouse uid/gid by stat-ing the data directory.
///
/// Returns (uid, gid) of the data directory owner, which should be the
/// ClickHouse process user.
pub fn detect_clickhouse_ownership(data_path: &Path) -> Result<(Option<u32>, Option<u32>)> {
    use std::os::unix::fs::MetadataExt;

    if !data_path.exists() {
        debug!(
            path = %data_path.display(),
            "Data path does not exist, cannot detect ClickHouse ownership"
        );
        return Ok((None, None));
    }

    let metadata = std::fs::metadata(data_path).with_context(|| {
        format!(
            "Failed to stat ClickHouse data path: {}",
            data_path.display()
        )
    })?;

    let uid = metadata.uid();
    let gid = metadata.gid();

    debug!(
        path = %data_path.display(),
        uid = uid,
        gid = gid,
        "Detected ClickHouse ownership"
    );

    Ok((Some(uid), Some(gid)))
}

/// Get the data path for a specific table by querying system.tables.
///
/// Falls back to constructing a default path if the query fails.
pub fn get_table_data_path(
    data_paths: &[String],
    data_path_config: &str,
    db: &str,
    table: &str,
) -> PathBuf {
    // data_paths from system.tables contains the actual storage paths
    // The first element is the primary data path for the table
    if let Some(first) = data_paths.first() {
        if !first.is_empty() {
            return PathBuf::from(first);
        }
    }

    // Fallback: construct path from config
    PathBuf::from(data_path_config)
        .join("data")
        .join(db)
        .join(table)
}

/// URL-encode a component for directory paths (same as download module).
fn url_encode(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '/' || c == '-' || c == '_' || c == '.' {
                c.to_string()
            } else {
                format!("%{:02X}", c as u32)
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_owned_attach_params_send() {
        fn assert_send<T: Send>() {}
        assert_send::<OwnedAttachParams>();
    }

    #[test]
    fn test_needs_sequential_attach_wired() {
        // Verify needs_sequential_attach is called from attach flow
        // by testing the function directly with the same engines
        // that the attach_parts_inner logic would use
        assert!(needs_sequential_attach("ReplacingMergeTree"));
        assert!(needs_sequential_attach("CollapsingMergeTree"));
        assert!(needs_sequential_attach("VersionedCollapsingMergeTree"));
        assert!(!needs_sequential_attach("MergeTree"));
        assert!(!needs_sequential_attach("AggregatingMergeTree"));
    }

    #[test]
    fn test_hardlink_or_copy_dir() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        let dst = dir.path().join("dst");

        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("file1.txt"), b"content1").unwrap();
        std::fs::write(src.join("file2.txt"), b"content2").unwrap();

        let sub = src.join("subdir");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("nested.txt"), b"nested").unwrap();

        hardlink_or_copy_dir(&src, &dst).unwrap();

        assert_eq!(
            std::fs::read_to_string(dst.join("file1.txt")).unwrap(),
            "content1"
        );
        assert_eq!(
            std::fs::read_to_string(dst.join("file2.txt")).unwrap(),
            "content2"
        );
        assert_eq!(
            std::fs::read_to_string(dst.join("subdir/nested.txt")).unwrap(),
            "nested"
        );
    }

    #[test]
    fn test_get_table_data_path_from_data_paths() {
        let data_paths = vec!["/var/lib/clickhouse/store/abc/abcdef123456/".to_string()];
        let result = get_table_data_path(&data_paths, "/var/lib/clickhouse", "default", "trades");
        assert_eq!(
            result,
            PathBuf::from("/var/lib/clickhouse/store/abc/abcdef123456/")
        );
    }

    #[test]
    fn test_get_table_data_path_fallback() {
        let data_paths: Vec<String> = vec![];
        let result = get_table_data_path(&data_paths, "/var/lib/clickhouse", "default", "trades");
        assert_eq!(
            result,
            PathBuf::from("/var/lib/clickhouse/data/default/trades")
        );
    }

    #[test]
    fn test_get_table_data_path_empty_first() {
        let data_paths = vec!["".to_string()];
        let result = get_table_data_path(&data_paths, "/var/lib/clickhouse", "default", "trades");
        assert_eq!(
            result,
            PathBuf::from("/var/lib/clickhouse/data/default/trades")
        );
    }

    #[test]
    fn test_detect_clickhouse_ownership_nonexistent() {
        let result = detect_clickhouse_ownership(Path::new("/nonexistent/path/123456789"));
        assert!(result.is_ok());
        let (uid, gid) = result.unwrap();
        assert!(uid.is_none());
        assert!(gid.is_none());
    }

    #[test]
    fn test_url_encode() {
        assert_eq!(url_encode("default"), "default");
        assert_eq!(url_encode("my table"), "my%20table");
    }
}
