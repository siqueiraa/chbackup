//! Table / database remap logic for restore `--as` and `-m` flags.
//!
//! All functions are pure (no async, no I/O) for easy unit testing.
//! DDL rewriting uses string manipulation (no regex crate dependency).

use std::collections::HashMap;

use anyhow::{bail, Result};
use tracing::info;

/// Parsed remap configuration from CLI flags.
#[derive(Debug, Clone)]
pub struct RemapConfig {
    /// Table-level rename mappings: Vec of (src_db, src_table, dst_db, dst_table).
    /// Supports two formats:
    /// - Legacy single: `-t db.src --as db.dst`
    /// - Multi-table: `--as "db.src:db.dst,db2.src2:db2.dst2"`
    pub rename_as: Vec<(String, String, String, String)>,
    /// Database-level mapping: src_db -> dst_db.
    pub database_mapping: HashMap<String, String>,
    /// ZK path template from config (e.g. "/clickhouse/tables/{shard}/{database}/{table}").
    pub default_replica_path: String,
}

impl RemapConfig {
    /// Build a RemapConfig from CLI flags.
    ///
    /// - `rename_as_str`: value of `--as` flag. Supports two formats:
    ///   - Legacy single-table: `"dst_db.dst_table"` (requires `-t src_db.src_table`)
    ///   - Multi-table mapping: `"src_db.src:dst_db.dst,src_db2.src2:dst_db2.dst2"`
    /// - `table_pattern`: value of `-t` flag, e.g. "src_db.src_table" (required for legacy format)
    /// - `db_mapping_str`: value of `-m` flag, e.g. "prod:staging,logs:logs_copy"
    /// - `default_replica_path`: from config.clickhouse.default_replica_path
    ///
    /// Returns `None` if no remap flags are provided.
    pub fn new(
        rename_as_str: Option<&str>,
        table_pattern: Option<&str>,
        db_mapping_str: Option<&str>,
        default_replica_path: &str,
    ) -> Result<Option<Self>> {
        let rename_as = match rename_as_str {
            Some(as_str) => {
                if as_str.contains(':') {
                    // Multi-table format: "src_db.src:dst_db.dst[,src_db2.src2:dst_db2.dst2]"
                    parse_table_mapping(as_str)?
                } else {
                    // Legacy single-table format: -t db.src --as db.dst
                    let pattern = match table_pattern {
                        Some(p) => p,
                        None => {
                            bail!("--as flag requires -t flag to specify the source table (e.g. -t src_db.src_table --as dst_db.dst_table), or use multi-table format: --as 'src_db.src:dst_db.dst'");
                        }
                    };

                    // Parse source from -t pattern
                    let (src_db, src_table) = match pattern.split_once('.') {
                        Some((db, tbl)) => (db.to_string(), tbl.to_string()),
                        None => {
                            bail!(
                                "--as flag requires -t pattern in db.table format, got '{}'",
                                pattern
                            );
                        }
                    };

                    // Validate -t is a single table (no wildcards)
                    if src_table.contains('*')
                        || src_table.contains('?')
                        || src_db.contains('*')
                        || src_db.contains('?')
                    {
                        bail!(
                            "--as flag requires -t to specify a single table (no wildcards), got '{}'",
                            pattern
                        );
                    }

                    // Parse destination from --as value
                    let (dst_db, dst_table) = match as_str.split_once('.') {
                        Some((db, tbl)) => (db.to_string(), tbl.to_string()),
                        None => {
                            bail!("--as value must be in db.table format, got '{}'", as_str);
                        }
                    };

                    info!(
                        src = %format!("{}.{}", src_db, src_table),
                        dst = %format!("{}.{}", dst_db, dst_table),
                        "Remap: {}.{} -> {}.{}",
                        src_db, src_table, dst_db, dst_table
                    );

                    vec![(src_db, src_table, dst_db, dst_table)]
                }
            }
            None => Vec::new(),
        };

        let database_mapping = match db_mapping_str {
            Some(s) if !s.is_empty() => {
                let mapping = parse_database_mapping(s)?;
                for (src, dst) in &mapping {
                    info!(
                        src = %src,
                        dst = %dst,
                        "Database remap: {} -> {}",
                        src, dst
                    );
                }
                mapping
            }
            _ => HashMap::new(),
        };

        // If nothing is active, return None
        if rename_as.is_empty() && database_mapping.is_empty() {
            return Ok(None);
        }

        Ok(Some(Self {
            rename_as,
            database_mapping,
            default_replica_path: default_replica_path.to_string(),
        }))
    }

    /// Returns true if any remap is configured.
    pub fn is_active(&self) -> bool {
        !self.rename_as.is_empty() || !self.database_mapping.is_empty()
    }

    /// Given an original "db.table" key, return the destination (db, table).
    ///
    /// Priority: `--as` rename takes precedence over `-m` database mapping.
    pub fn remap_table_key(&self, original_key: &str) -> (String, String) {
        let (orig_db, orig_table) = original_key
            .split_once('.')
            .unwrap_or(("default", original_key));

        // Check --as rename first (exact match on src_db.src_table)
        for (src_db, src_table, dst_db, dst_table) in &self.rename_as {
            if orig_db == src_db && orig_table == src_table {
                return (dst_db.clone(), dst_table.clone());
            }
        }

        // Check database mapping
        if let Some(dst_db) = self.database_mapping.get(orig_db) {
            return (dst_db.clone(), orig_table.to_string());
        }

        // No mapping -- passthrough
        (orig_db.to_string(), orig_table.to_string())
    }
}

/// Parse multi-table mapping from `--as` value.
///
/// Format: `"src_db.src_table:dst_db.dst_table[,src_db2.src2:dst_db2.dst2]"`
///
/// Returns Vec of (src_db, src_table, dst_db, dst_table) tuples.
fn parse_table_mapping(s: &str) -> Result<Vec<(String, String, String, String)>> {
    let mut mappings = Vec::new();
    for pair in s.split(',') {
        let pair = pair.trim();
        if pair.is_empty() {
            continue;
        }
        let (src, dst) = match pair.split_once(':') {
            Some((s, d)) => (s.trim(), d.trim()),
            None => {
                bail!(
                    "Invalid table mapping '{}': expected format 'src_db.src_table:dst_db.dst_table'",
                    pair
                );
            }
        };
        let (src_db, src_table) = match src.split_once('.') {
            Some((db, tbl)) => (db.to_string(), tbl.to_string()),
            None => {
                bail!(
                    "Invalid table mapping source '{}': expected db.table format",
                    src
                );
            }
        };
        let (dst_db, dst_table) = match dst.split_once('.') {
            Some((db, tbl)) => (db.to_string(), tbl.to_string()),
            None => {
                bail!(
                    "Invalid table mapping destination '{}': expected db.table format",
                    dst
                );
            }
        };

        info!(
            src = %format!("{}.{}", src_db, src_table),
            dst = %format!("{}.{}", dst_db, dst_table),
            "Remap: {}.{} -> {}.{}",
            src_db, src_table, dst_db, dst_table
        );

        mappings.push((src_db, src_table, dst_db, dst_table));
    }

    if mappings.is_empty() {
        bail!("--as value is empty or contains no valid mappings");
    }

    Ok(mappings)
}

/// Parse "-m prod:staging,logs:logs_copy" into HashMap.
///
/// Format: comma-separated pairs of src:dst.
/// Empty string returns empty map.
pub fn parse_database_mapping(s: &str) -> Result<HashMap<String, String>> {
    let s = s.trim();
    if s.is_empty() {
        return Ok(HashMap::new());
    }

    let mut map = HashMap::new();
    for pair in s.split(',') {
        let pair = pair.trim();
        if pair.is_empty() {
            continue;
        }
        let (src, dst) = match pair.split_once(':') {
            Some((s, d)) => (s.trim(), d.trim()),
            None => {
                bail!(
                    "Invalid database mapping '{}': expected format 'src:dst', e.g. 'prod:staging'",
                    pair
                );
            }
        };
        if src.is_empty() || dst.is_empty() {
            bail!(
                "Invalid database mapping '{}': source and destination must not be empty",
                pair
            );
        }
        map.insert(src.to_string(), dst.to_string());
    }

    Ok(map)
}

/// Rewrite CREATE TABLE DDL for remap.
///
/// Transformations applied:
/// 1. Change table name (db.table) in CREATE statement
/// 2. Remove UUID clause (let ClickHouse assign a new one)
/// 3. Rewrite ZK path in ReplicatedMergeTree engine
/// 4. Update Distributed engine database/table references
pub fn rewrite_create_table_ddl(
    ddl: &str,
    src_db: &str,
    src_table: &str,
    dst_db: &str,
    dst_table: &str,
    default_replica_path: &str,
) -> String {
    info!(
        src = %format!("{}.{}", src_db, src_table),
        dst = %format!("{}.{}", dst_db, dst_table),
        "Rewriting DDL for remap"
    );

    let mut result = ddl.to_string();

    // 1. Replace table name in CREATE statement
    result = rewrite_table_name(&result, src_db, src_table, dst_db, dst_table);

    // 2. Remove UUID clause
    result = remove_uuid_clause(&result);

    // 3. Rewrite ZK path in ReplicatedMergeTree
    result = rewrite_replicated_zk_path(&result, dst_db, dst_table, default_replica_path);

    // 4. Update Distributed engine references
    result = rewrite_distributed_engine(&result, src_db, src_table, dst_db, dst_table);

    result
}

/// Rewrite CREATE DATABASE DDL for remap.
///
/// Changes the database name in CREATE DATABASE statement.
pub fn rewrite_create_database_ddl(ddl: &str, src_db: &str, dst_db: &str) -> String {
    if src_db == dst_db {
        return ddl.to_string();
    }

    // Handle backtick-quoted and unquoted database names
    let mut result = ddl.to_string();

    // Replace backtick-quoted: `src_db`
    let backtick_src = format!("`{}`", src_db);
    let backtick_dst = format!("`{}`", dst_db);
    if result.contains(&backtick_src) {
        result = result.replace(&backtick_src, &backtick_dst);
        return result;
    }

    // Replace unquoted: look for the database name after CREATE DATABASE [IF NOT EXISTS]
    // We need to be careful to only replace the database name, not random occurrences
    let patterns = [
        format!("CREATE DATABASE IF NOT EXISTS {}", src_db),
        format!("CREATE DATABASE {}", src_db),
    ];
    let replacements = [
        format!("CREATE DATABASE IF NOT EXISTS {}", dst_db),
        format!("CREATE DATABASE {}", dst_db),
    ];

    for (pat, rep) in patterns.iter().zip(replacements.iter()) {
        if result.contains(pat.as_str()) {
            result = result.replacen(pat.as_str(), rep.as_str(), 1);
            return result;
        }
    }

    result
}

// ---------------------------------------------------------------------------
// Phase 4d: DDL helpers for ON CLUSTER, ZK, Distributed cluster rewrite
// ---------------------------------------------------------------------------

/// Parse the ZK path and replica name from a Replicated*MergeTree DDL.
///
/// Returns `None` if not a Replicated engine or if using short syntax (empty parens).
/// Extracts the FIRST and SECOND single-quoted arguments after the Replicated engine `(`.
pub fn parse_replicated_params(ddl: &str) -> Option<(String, String)> {
    // Find "Replicated" engine marker
    let replicated_idx = find_case_sensitive(ddl, "Replicated")?;

    // Find the opening paren after the engine name
    let after_engine = &ddl[replicated_idx..];
    let paren_offset = after_engine.find('(')?;
    let paren_pos = replicated_idx + paren_offset;

    // Check for empty parens (short syntax)
    let after_paren = &ddl[(paren_pos + 1)..];
    let trimmed = after_paren.trim_start();
    if trimmed.starts_with(')') {
        return None;
    }

    // Find the first single-quoted string (ZK path)
    let first_quote = after_paren.find('\'')?;
    let path_start = paren_pos + 1 + first_quote;
    let remaining = &ddl[(path_start + 1)..];
    let end_quote = remaining.find('\'')?;
    let zk_path = &ddl[(path_start + 1)..(path_start + 1 + end_quote)];

    // Find the second single-quoted string (replica name)
    let after_first = &ddl[(path_start + 1 + end_quote + 1)..];
    let second_quote = after_first.find('\'')?;
    let replica_start = path_start + 1 + end_quote + 1 + second_quote;
    let remaining2 = &ddl[(replica_start + 1)..];
    let end_quote2 = remaining2.find('\'')?;
    let replica = &ddl[(replica_start + 1)..(replica_start + 1 + end_quote2)];

    Some((zk_path.to_string(), replica.to_string()))
}

/// Resolve macros in a ZK path template.
///
/// Substitutes `{key}` patterns from the provided map. Commonly used keys:
/// `{database}`, `{table}`, `{shard}`, `{replica}`, `{uuid}`.
/// Unknown macros are left as-is.
pub fn resolve_zk_macros(template: &str, macros: &HashMap<String, String>) -> String {
    let mut result = template.to_string();
    for (key, value) in macros {
        let pattern = format!("{{{}}}", key);
        result = result.replace(&pattern, value);
    }
    result
}

/// Inject ON CLUSTER clause into a DDL statement.
///
/// Works for CREATE TABLE, CREATE DATABASE, DROP TABLE, DROP DATABASE, and
/// their IF [NOT] EXISTS variants.
/// Returns the DDL unchanged if ON CLUSTER is already present.
pub fn add_on_cluster_clause(ddl: &str, cluster: &str) -> String {
    // Check if already has ON CLUSTER
    if ddl.contains("ON CLUSTER") {
        return ddl.to_string();
    }

    let on_cluster_str = format!(" ON CLUSTER '{}'", cluster);

    // Try each pattern in order: find where to insert the ON CLUSTER clause.
    // The clause goes after the object identifier (which follows CREATE/DROP and any IF [NOT] EXISTS).
    // Strategy: find the first backtick-quoted identifier or bare identifier after the keyword block.

    // Patterns we support (case-sensitive):
    // CREATE TABLE IF NOT EXISTS `db`.`table` ...
    // CREATE TABLE `db`.`table` ...
    // CREATE DATABASE IF NOT EXISTS `db` ...
    // CREATE DATABASE `db` ...
    // DROP TABLE IF EXISTS `db`.`table` ...
    // DROP TABLE `db`.`table` ...
    // DROP DATABASE IF EXISTS `db` ...
    // DROP DATABASE `db` ...
    // CREATE MATERIALIZED VIEW ...
    // CREATE VIEW ...
    // CREATE DICTIONARY ...

    // Find the keyword sequence end and then skip past the object name
    let prefixes = [
        "CREATE TABLE IF NOT EXISTS",
        "CREATE TABLE",
        "CREATE MATERIALIZED VIEW IF NOT EXISTS",
        "CREATE MATERIALIZED VIEW",
        "CREATE VIEW IF NOT EXISTS",
        "CREATE VIEW",
        "CREATE DICTIONARY IF NOT EXISTS",
        "CREATE DICTIONARY",
        "CREATE DATABASE IF NOT EXISTS",
        "CREATE DATABASE",
        "DROP TABLE IF EXISTS",
        "DROP TABLE",
        "DROP DATABASE IF EXISTS",
        "DROP DATABASE",
    ];

    for prefix in &prefixes {
        if !ddl.starts_with(prefix) {
            continue;
        }

        let after_prefix = &ddl[prefix.len()..];
        let trimmed = after_prefix.trim_start();
        let spaces_len = after_prefix.len() - trimmed.len();
        let name_start = prefix.len() + spaces_len;

        // Skip the object name (could be `db`.`table` or just `db` or db.table)
        let name_end = skip_object_name(&ddl[name_start..]);
        let insert_pos = name_start + name_end;

        let mut result = String::with_capacity(ddl.len() + on_cluster_str.len());
        result.push_str(&ddl[..insert_pos]);
        result.push_str(&on_cluster_str);
        result.push_str(&ddl[insert_pos..]);
        return result;
    }

    // Fallback: return unchanged
    ddl.to_string()
}

/// Skip past an object name in DDL. Returns the number of bytes consumed.
///
/// Handles:
/// - Backtick-quoted: `` `db`.`table` ``
/// - Unquoted: `db.table` or `db`
fn skip_object_name(s: &str) -> usize {
    let mut pos = 0;
    let bytes = s.as_bytes();

    // Loop to handle `db`.`table` or db.table patterns
    loop {
        if pos >= bytes.len() {
            break;
        }

        if bytes[pos] == b'`' {
            // Backtick-quoted identifier
            pos += 1; // skip opening backtick
            while pos < bytes.len() && bytes[pos] != b'`' {
                pos += 1;
            }
            if pos < bytes.len() {
                pos += 1; // skip closing backtick
            }
        } else if bytes[pos].is_ascii_alphanumeric() || bytes[pos] == b'_' || bytes[pos] == b'.' {
            // Unquoted identifier character (including dot for db.table)
            while pos < bytes.len()
                && (bytes[pos].is_ascii_alphanumeric() || bytes[pos] == b'_' || bytes[pos] == b'.')
            {
                pos += 1;
            }
        } else {
            break;
        }

        // Check for dot separator between backtick-quoted parts
        if pos < bytes.len() && bytes[pos] == b'.' {
            pos += 1; // skip dot
            continue; // continue to next part
        }

        break;
    }

    pos
}

/// Rewrite the cluster name in a Distributed engine DDL.
///
/// Changes `Distributed('old_cluster', ...)` to `Distributed('new_cluster', ...)`.
/// Returns DDL unchanged if not a Distributed engine or cluster not found.
pub fn rewrite_distributed_cluster(ddl: &str, new_cluster: &str) -> String {
    // Find "Distributed(" marker
    let dist_idx = match find_distributed_engine(ddl) {
        Some(idx) => idx,
        None => return ddl.to_string(),
    };

    // Find the opening paren
    let after_dist = &ddl[dist_idx..];
    let paren_offset = match after_dist.find('(') {
        Some(p) => p,
        None => return ddl.to_string(),
    };
    let paren_pos = dist_idx + paren_offset;

    // The first argument after '(' is the cluster name
    let inner_start = paren_pos + 1;
    let inner = &ddl[inner_start..];

    // Find the first single-quoted argument (cluster name)
    let first_quote = match inner.find('\'') {
        Some(q) => q,
        None => return ddl.to_string(),
    };
    let cluster_start = inner_start + first_quote;
    let after_quote = &ddl[(cluster_start + 1)..];
    let end_quote = match after_quote.find('\'') {
        Some(q) => q,
        None => return ddl.to_string(),
    };
    let cluster_end = cluster_start + 1 + end_quote;

    // Replace the cluster name
    let mut result = String::with_capacity(ddl.len());
    result.push_str(&ddl[..=cluster_start]); // up to and including opening quote
    result.push_str(new_cluster);
    result.push_str(&ddl[cluster_end..]); // from closing quote onward
    result
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Replace the table name in a CREATE TABLE/VIEW/MATERIALIZED VIEW/DICTIONARY statement.
fn rewrite_table_name(
    ddl: &str,
    src_db: &str,
    src_table: &str,
    dst_db: &str,
    dst_table: &str,
) -> String {
    let mut result = ddl.to_string();

    // Backtick-quoted form: `src_db`.`src_table`
    let bt_src = format!("`{}`.`{}`", src_db, src_table);
    let bt_dst = format!("`{}`.`{}`", dst_db, dst_table);
    if result.contains(&bt_src) {
        result = result.replacen(&bt_src, &bt_dst, 1);
        return result;
    }

    // Unquoted form: src_db.src_table
    let plain_src = format!("{}.{}", src_db, src_table);
    let plain_dst = format!("{}.{}", dst_db, dst_table);
    if result.contains(&plain_src) {
        result = result.replacen(&plain_src, &plain_dst, 1);
        return result;
    }

    result
}

/// Remove UUID clause: `UUID 'hex-hex-hex-hex-hex'`.
fn remove_uuid_clause(ddl: &str) -> String {
    // Look for "UUID '" followed by hex-hex pattern and closing '
    let uuid_marker = "UUID '";
    let Some(start) = ddl.find(uuid_marker) else {
        return ddl.to_string();
    };

    let after_marker = start + uuid_marker.len();
    // Find the closing single quote
    let Some(end_quote) = ddl[after_marker..].find('\'') else {
        return ddl.to_string();
    };

    let end = after_marker + end_quote + 1; // include closing quote

    // Remove the UUID clause and any trailing whitespace
    let mut result = String::with_capacity(ddl.len());
    result.push_str(ddl[..start].trim_end());
    // Add a single space if there's content after
    let after = ddl[end..].trim_start();
    if !after.is_empty() {
        result.push(' ');
        result.push_str(after);
    }

    result
}

/// Rewrite the ZooKeeper path in ReplicatedMergeTree engine.
///
/// Replaces the first single-quoted argument with the template from config,
/// substituting {database} and {table} with the destination values.
fn rewrite_replicated_zk_path(
    ddl: &str,
    dst_db: &str,
    dst_table: &str,
    default_replica_path: &str,
) -> String {
    // Find "Replicated" engine marker (ReplicatedMergeTree, ReplicatedReplacingMergeTree, etc.)
    let replicated_idx = match find_case_sensitive(ddl, "Replicated") {
        Some(idx) => idx,
        None => return ddl.to_string(),
    };

    // Find the opening paren after the engine name
    let after_engine = &ddl[replicated_idx..];
    let Some(paren_offset) = after_engine.find('(') else {
        return ddl.to_string();
    };
    let paren_pos = replicated_idx + paren_offset;

    // Find the first single-quoted string inside the parens (the ZK path)
    let after_paren = &ddl[(paren_pos + 1)..];
    let Some(first_quote) = after_paren.find('\'') else {
        return ddl.to_string();
    };
    let path_start = paren_pos + 1 + first_quote; // position of opening quote

    let remaining = &ddl[(path_start + 1)..];
    let Some(end_quote) = remaining.find('\'') else {
        return ddl.to_string();
    };
    let path_end = path_start + 1 + end_quote; // position of closing quote

    // Build new ZK path from template
    let new_path = default_replica_path
        .replace("{database}", dst_db)
        .replace("{table}", dst_table);

    let mut result = String::with_capacity(ddl.len());
    result.push_str(&ddl[..=path_start]); // up to and including opening quote
    result.push_str(&new_path);
    result.push_str(&ddl[path_end..]); // from closing quote onward

    result
}

/// Rewrite Distributed engine database and table references.
///
/// Distributed engine format: `Distributed(cluster, database, table[, sharding_key])`
/// The second and third arguments are the database and table names.
fn rewrite_distributed_engine(
    ddl: &str,
    src_db: &str,
    src_table: &str,
    dst_db: &str,
    dst_table: &str,
) -> String {
    // Find "Distributed(" or "Distributed (" marker
    let dist_idx = match find_distributed_engine(ddl) {
        Some(idx) => idx,
        None => return ddl.to_string(),
    };

    // Find the opening paren
    let after_dist = &ddl[dist_idx..];
    let Some(paren_offset) = after_dist.find('(') else {
        return ddl.to_string();
    };
    let paren_pos = dist_idx + paren_offset;

    // Parse the arguments inside the parentheses
    // We need to find: cluster, database, table arguments
    // Arguments can be quoted or unquoted, separated by commas
    let inner_start = paren_pos + 1;
    let inner = &ddl[inner_start..];

    // Find matching closing paren (handle nested parens)
    let Some(close_paren) = find_matching_paren(inner) else {
        return ddl.to_string();
    };

    let args_str = &inner[..close_paren];

    // Split by commas (first 3 arguments: cluster, db, table)
    let args: Vec<&str> = args_str.splitn(4, ',').collect();
    if args.len() < 3 {
        return ddl.to_string();
    }

    // Arg 1 (index 1) is database, arg 2 (index 2) is table
    let db_arg = args[1].trim();
    let table_arg = args[2].trim();

    // Strip quotes from args for comparison
    let db_val = strip_quotes(db_arg);
    let table_val = strip_quotes(table_arg);

    // Only rewrite if the source matches
    if db_val != src_db || table_val != src_table {
        return ddl.to_string();
    }

    // Build new arguments preserving quoting style
    let new_db_arg = if db_arg.starts_with('\'') {
        format!(" '{}'", dst_db)
    } else {
        format!(" {}", dst_db)
    };

    let new_table_arg = if table_arg.starts_with('\'') {
        format!(" '{}'", dst_table)
    } else {
        format!(" {}", dst_table)
    };

    // Reconstruct: keep cluster arg (args[0]) and rest (args[3..]) unchanged
    let mut result = String::with_capacity(ddl.len());
    result.push_str(&ddl[..inner_start]); // everything up to first arg
    result.push_str(args[0]); // cluster (unchanged)
    result.push(',');
    result.push_str(&new_db_arg);
    result.push(',');
    result.push_str(&new_table_arg);

    // Append remaining args if any (sharding key, etc.)
    if args.len() > 3 {
        result.push(',');
        result.push_str(args[3]);
    }

    // Close paren and rest of DDL
    result.push_str(&ddl[(inner_start + close_paren)..]);

    result
}

/// Find "Distributed" engine keyword followed by '('.
fn find_distributed_engine(ddl: &str) -> Option<usize> {
    let needle = "Distributed";
    let mut search_from = 0;
    while search_from < ddl.len() {
        let idx = find_case_sensitive(&ddl[search_from..], needle)?;
        let abs_idx = search_from + idx;

        // Check that the next non-whitespace char is '(' or it's part of ENGINE = Distributed(...)
        let after = ddl[(abs_idx + needle.len())..].trim_start();
        if after.starts_with('(') {
            return Some(abs_idx);
        }
        search_from = abs_idx + needle.len();
    }
    None
}

/// Find a substring (case-sensitive).
fn find_case_sensitive(haystack: &str, needle: &str) -> Option<usize> {
    haystack.find(needle)
}

/// Find the matching closing parenthesis, handling nesting.
fn find_matching_paren(s: &str) -> Option<usize> {
    let mut depth = 0i32;
    let mut in_single_quote = false;

    for (i, ch) in s.char_indices() {
        match ch {
            '\'' if !in_single_quote => in_single_quote = true,
            '\'' if in_single_quote => in_single_quote = false,
            '(' if !in_single_quote => depth += 1,
            ')' if !in_single_quote => {
                if depth == 0 {
                    return Some(i);
                }
                depth -= 1;
            }
            _ => {}
        }
    }
    None
}

/// Strip surrounding single quotes from a string.
fn strip_quotes(s: &str) -> &str {
    let s = s.trim();
    if s.starts_with('\'') && s.ends_with('\'') && s.len() >= 2 {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // parse_database_mapping tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_parse_database_mapping_single() {
        let result = parse_database_mapping("prod:staging").unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result.get("prod").unwrap(), "staging");
    }

    #[test]
    fn test_parse_database_mapping_multiple() {
        let result = parse_database_mapping("prod:staging,logs:logs_copy").unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result.get("prod").unwrap(), "staging");
        assert_eq!(result.get("logs").unwrap(), "logs_copy");
    }

    #[test]
    fn test_parse_database_mapping_empty() {
        let result = parse_database_mapping("").unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_parse_database_mapping_invalid() {
        let result = parse_database_mapping("nocolon");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("expected format"),
            "Error should mention format: {}",
            err
        );
    }

    #[test]
    fn test_parse_database_mapping_empty_source() {
        let result = parse_database_mapping(":staging");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_database_mapping_empty_dest() {
        let result = parse_database_mapping("prod:");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_database_mapping_with_spaces() {
        let result = parse_database_mapping(" prod : staging , logs : logs_copy ").unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result.get("prod").unwrap(), "staging");
        assert_eq!(result.get("logs").unwrap(), "logs_copy");
    }

    // -----------------------------------------------------------------------
    // RemapConfig::new tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_remap_config_new_no_flags() {
        let result = RemapConfig::new(
            None,
            None,
            None,
            "/clickhouse/tables/{shard}/{database}/{table}",
        )
        .unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_remap_config_new_with_rename_as() {
        let result = RemapConfig::new(
            Some("dst_db.dst_table"),
            Some("src_db.src_table"),
            None,
            "/clickhouse/tables/{shard}/{database}/{table}",
        )
        .unwrap();
        assert!(result.is_some());
        let config = result.unwrap();
        assert!(config.is_active());
        assert_eq!(
            config.rename_as,
            vec![(
                "src_db".to_string(),
                "src_table".to_string(),
                "dst_db".to_string(),
                "dst_table".to_string()
            )]
        );
    }

    #[test]
    fn test_remap_config_new_multi_table_mapping() {
        let result = RemapConfig::new(
            Some("default.src1:default.dst1,prod.src2:staging.dst2"),
            Some("default.src1,prod.src2"),
            None,
            "/clickhouse/tables/{shard}/{database}/{table}",
        )
        .unwrap();
        assert!(result.is_some());
        let config = result.unwrap();
        assert!(config.is_active());
        assert_eq!(config.rename_as.len(), 2);
        assert_eq!(
            config.rename_as[0],
            (
                "default".to_string(),
                "src1".to_string(),
                "default".to_string(),
                "dst1".to_string()
            )
        );
        assert_eq!(
            config.rename_as[1],
            (
                "prod".to_string(),
                "src2".to_string(),
                "staging".to_string(),
                "dst2".to_string()
            )
        );
    }

    #[test]
    fn test_remap_config_new_as_without_table_pattern() {
        let result = RemapConfig::new(
            Some("dst_db.dst_table"),
            None,
            None,
            "/clickhouse/tables/{shard}/{database}/{table}",
        );
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("--as flag requires -t flag"));
    }

    #[test]
    fn test_remap_config_new_as_with_wildcard() {
        let result = RemapConfig::new(
            Some("dst_db.dst_table"),
            Some("src_db.*"),
            None,
            "/clickhouse/tables/{shard}/{database}/{table}",
        );
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("no wildcards"));
    }

    #[test]
    fn test_remap_config_new_as_bad_format() {
        let result = RemapConfig::new(
            Some("just_table"),
            Some("src_db.src_table"),
            None,
            "/clickhouse/tables/{shard}/{database}/{table}",
        );
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("db.table format"));
    }

    #[test]
    fn test_remap_config_new_with_database_mapping() {
        let result = RemapConfig::new(
            None,
            None,
            Some("prod:staging"),
            "/clickhouse/tables/{shard}/{database}/{table}",
        )
        .unwrap();
        assert!(result.is_some());
        let config = result.unwrap();
        assert!(config.is_active());
        assert_eq!(config.database_mapping.get("prod").unwrap(), "staging");
    }

    // -----------------------------------------------------------------------
    // remap_table_key tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_remap_table_key_with_rename_as() {
        let config = RemapConfig {
            rename_as: vec![(
                "src_db".to_string(),
                "src_table".to_string(),
                "dst_db".to_string(),
                "dst_table".to_string(),
            )],
            database_mapping: HashMap::new(),
            default_replica_path: String::new(),
        };

        let (db, table) = config.remap_table_key("src_db.src_table");
        assert_eq!(db, "dst_db");
        assert_eq!(table, "dst_table");
    }

    #[test]
    fn test_remap_table_key_with_database_mapping() {
        let mut mapping = HashMap::new();
        mapping.insert("prod".to_string(), "staging".to_string());

        let config = RemapConfig {
            rename_as: Vec::new(),
            database_mapping: mapping,
            default_replica_path: String::new(),
        };

        let (db, table) = config.remap_table_key("prod.users");
        assert_eq!(db, "staging");
        assert_eq!(table, "users");
    }

    #[test]
    fn test_remap_table_key_no_mapping() {
        let config = RemapConfig {
            rename_as: Vec::new(),
            database_mapping: HashMap::new(),
            default_replica_path: String::new(),
        };

        let (db, table) = config.remap_table_key("prod.users");
        assert_eq!(db, "prod");
        assert_eq!(table, "users");
    }

    #[test]
    fn test_remap_table_key_database_not_in_mapping() {
        let mut mapping = HashMap::new();
        mapping.insert("prod".to_string(), "staging".to_string());

        let config = RemapConfig {
            rename_as: Vec::new(),
            database_mapping: mapping,
            default_replica_path: String::new(),
        };

        let (db, table) = config.remap_table_key("logs.events");
        assert_eq!(db, "logs");
        assert_eq!(table, "events");
    }

    #[test]
    fn test_remap_table_key_rename_as_takes_priority() {
        let mut mapping = HashMap::new();
        mapping.insert("src_db".to_string(), "mapped_db".to_string());

        let config = RemapConfig {
            rename_as: vec![(
                "src_db".to_string(),
                "src_table".to_string(),
                "dst_db".to_string(),
                "dst_table".to_string(),
            )],
            database_mapping: mapping,
            default_replica_path: String::new(),
        };

        // --as takes priority over -m for the specific table
        let (db, table) = config.remap_table_key("src_db.src_table");
        assert_eq!(db, "dst_db");
        assert_eq!(table, "dst_table");

        // Other tables in the same database still use -m mapping
        let (db, table) = config.remap_table_key("src_db.other_table");
        assert_eq!(db, "mapped_db");
        assert_eq!(table, "other_table");
    }

    // -----------------------------------------------------------------------
    // rewrite_create_table_ddl tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_rewrite_ddl_simple_mergetree() {
        let ddl =
            "CREATE TABLE src_db.src_table (id UInt64, name String) ENGINE = MergeTree ORDER BY id";
        let result = rewrite_create_table_ddl(
            ddl,
            "src_db",
            "src_table",
            "dst_db",
            "dst_table",
            "/clickhouse/tables/{shard}/{database}/{table}",
        );
        assert!(
            result.contains("dst_db.dst_table"),
            "Should contain dst_db.dst_table: {}",
            result
        );
        assert!(
            !result.contains("src_db.src_table"),
            "Should not contain src_db.src_table: {}",
            result
        );
        assert!(
            result.contains("ENGINE = MergeTree ORDER BY id"),
            "Engine and ORDER BY unchanged: {}",
            result
        );
    }

    #[test]
    fn test_rewrite_ddl_removes_uuid() {
        let ddl = "CREATE TABLE src_db.src_table UUID 'abc12345-1234-5678-9abc-def012345678' (id UInt64) ENGINE = MergeTree ORDER BY id";
        let result = rewrite_create_table_ddl(
            ddl,
            "src_db",
            "src_table",
            "dst_db",
            "dst_table",
            "/clickhouse/tables/{shard}/{database}/{table}",
        );
        assert!(
            !result.contains("UUID"),
            "UUID should be removed: {}",
            result
        );
        assert!(
            !result.contains("abc12345"),
            "UUID value should be removed: {}",
            result
        );
        assert!(
            result.contains("dst_db.dst_table"),
            "Table name should be rewritten: {}",
            result
        );
    }

    #[test]
    fn test_rewrite_ddl_replicated_zk_path() {
        let ddl = "CREATE TABLE src_db.src_table (id UInt64) ENGINE = ReplicatedMergeTree('/clickhouse/tables/{shard}/src_db/src_table', '{replica}') ORDER BY id";
        let result = rewrite_create_table_ddl(
            ddl,
            "src_db",
            "src_table",
            "dst_db",
            "dst_table",
            "/clickhouse/tables/{shard}/{database}/{table}",
        );
        assert!(
            result.contains("'/clickhouse/tables/{shard}/dst_db/dst_table'"),
            "ZK path should use dst db/table: {}",
            result
        );
        assert!(
            result.contains("'{replica}'"),
            "Replica name should be unchanged: {}",
            result
        );
    }

    #[test]
    fn test_rewrite_ddl_replicated_replacing() {
        let ddl = "CREATE TABLE src_db.src_table (id UInt64, ver UInt64) ENGINE = ReplicatedReplacingMergeTree('/clickhouse/tables/{shard}/src_db/src_table', '{replica}', ver) ORDER BY id";
        let result = rewrite_create_table_ddl(
            ddl,
            "src_db",
            "src_table",
            "dst_db",
            "dst_table",
            "/clickhouse/tables/{shard}/{database}/{table}",
        );
        assert!(
            result.contains(
                "ReplicatedReplacingMergeTree('/clickhouse/tables/{shard}/dst_db/dst_table'"
            ),
            "ZK path should use dst values: {}",
            result
        );
    }

    #[test]
    fn test_rewrite_ddl_distributed_table() {
        let ddl = "CREATE TABLE src_db.src_table_dist (id UInt64) ENGINE = Distributed('my_cluster', src_db, src_table, rand())";
        let result = rewrite_create_table_ddl(
            ddl,
            "src_db",
            "src_table",
            "dst_db",
            "dst_table",
            "/clickhouse/tables/{shard}/{database}/{table}",
        );
        // The table name in CREATE is src_db.src_table_dist, but the Distributed engine
        // references src_db and src_table -- only the engine references should be updated
        assert!(
            result.contains("dst_db"),
            "Distributed db arg should be updated: {}",
            result
        );
        assert!(
            result.contains("dst_table"),
            "Distributed table arg should be updated: {}",
            result
        );
    }

    #[test]
    fn test_rewrite_ddl_distributed_quoted() {
        let ddl = "CREATE TABLE src_db.dist (id UInt64) ENGINE = Distributed('cluster', 'src_db', 'src_table', rand())";
        let result = rewrite_create_table_ddl(
            ddl,
            "src_db",
            "src_table",
            "dst_db",
            "dst_table",
            "/clickhouse/tables/{shard}/{database}/{table}",
        );
        assert!(
            result.contains("'dst_db'"),
            "Distributed quoted db arg should be updated: {}",
            result
        );
        assert!(
            result.contains("'dst_table'"),
            "Distributed quoted table arg should be updated: {}",
            result
        );
    }

    #[test]
    fn test_rewrite_ddl_backtick_names() {
        let ddl = "CREATE TABLE `src_db`.`src_table` (id UInt64) ENGINE = MergeTree ORDER BY id";
        let result = rewrite_create_table_ddl(
            ddl,
            "src_db",
            "src_table",
            "dst_db",
            "dst_table",
            "/clickhouse/tables/{shard}/{database}/{table}",
        );
        assert!(
            result.contains("`dst_db`.`dst_table`"),
            "Backtick names should be rewritten: {}",
            result
        );
    }

    #[test]
    fn test_rewrite_ddl_preserves_rest() {
        let ddl = "CREATE TABLE src_db.src_table (id UInt64, name String, ts DateTime) ENGINE = MergeTree PARTITION BY toYYYYMM(ts) ORDER BY (id, ts) SETTINGS index_granularity = 8192";
        let result = rewrite_create_table_ddl(
            ddl,
            "src_db",
            "src_table",
            "dst_db",
            "dst_table",
            "/clickhouse/tables/{shard}/{database}/{table}",
        );
        assert!(result.contains("PARTITION BY toYYYYMM(ts)"));
        assert!(result.contains("ORDER BY (id, ts)"));
        assert!(result.contains("SETTINGS index_granularity = 8192"));
        assert!(result.contains("id UInt64, name String, ts DateTime"));
    }

    #[test]
    fn test_rewrite_ddl_no_uuid() {
        // DDL without UUID should work fine
        let ddl = "CREATE TABLE src_db.src_table (id UInt64) ENGINE = MergeTree ORDER BY id";
        let result = rewrite_create_table_ddl(
            ddl,
            "src_db",
            "src_table",
            "dst_db",
            "dst_table",
            "/clickhouse/tables/{shard}/{database}/{table}",
        );
        assert!(
            !result.contains("UUID"),
            "No UUID should be present: {}",
            result
        );
        assert!(result.contains("dst_db.dst_table"));
    }

    #[test]
    fn test_rewrite_ddl_if_not_exists() {
        let ddl = "CREATE TABLE IF NOT EXISTS src_db.src_table (id UInt64) ENGINE = MergeTree ORDER BY id";
        let result = rewrite_create_table_ddl(
            ddl,
            "src_db",
            "src_table",
            "dst_db",
            "dst_table",
            "/clickhouse/tables/{shard}/{database}/{table}",
        );
        assert!(result.contains("IF NOT EXISTS dst_db.dst_table"));
    }

    // -----------------------------------------------------------------------
    // rewrite_create_database_ddl tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_rewrite_db_ddl() {
        let ddl = "CREATE DATABASE prod ENGINE = Atomic";
        let result = rewrite_create_database_ddl(ddl, "prod", "staging");
        assert_eq!(result, "CREATE DATABASE staging ENGINE = Atomic");
    }

    #[test]
    fn test_rewrite_db_ddl_if_not_exists() {
        let ddl = "CREATE DATABASE IF NOT EXISTS prod ENGINE = Atomic";
        let result = rewrite_create_database_ddl(ddl, "prod", "staging");
        assert_eq!(
            result,
            "CREATE DATABASE IF NOT EXISTS staging ENGINE = Atomic"
        );
    }

    #[test]
    fn test_rewrite_db_ddl_backtick() {
        let ddl = "CREATE DATABASE `prod` ENGINE = Atomic";
        let result = rewrite_create_database_ddl(ddl, "prod", "staging");
        assert_eq!(result, "CREATE DATABASE `staging` ENGINE = Atomic");
    }

    #[test]
    fn test_rewrite_db_ddl_same_name() {
        let ddl = "CREATE DATABASE prod ENGINE = Atomic";
        let result = rewrite_create_database_ddl(ddl, "prod", "prod");
        assert_eq!(result, ddl);
    }

    // -----------------------------------------------------------------------
    // Internal helper tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_remove_uuid_clause() {
        let ddl = "CREATE TABLE db.t UUID 'abc-123-def-456' (id UInt64) ENGINE = MergeTree";
        let result = remove_uuid_clause(ddl);
        assert!(!result.contains("UUID"));
        assert!(result.contains("CREATE TABLE db.t"));
        assert!(result.contains("(id UInt64)"));
    }

    #[test]
    fn test_remove_uuid_clause_no_uuid() {
        let ddl = "CREATE TABLE db.t (id UInt64) ENGINE = MergeTree";
        let result = remove_uuid_clause(ddl);
        assert_eq!(result, ddl);
    }

    #[test]
    fn test_find_matching_paren() {
        assert_eq!(find_matching_paren("a, b, c)"), Some(7));
        assert_eq!(find_matching_paren("a, (b), c)"), Some(9));
        assert_eq!(find_matching_paren("')', c)"), Some(6));
        assert_eq!(find_matching_paren("a, b"), None);
    }

    #[test]
    fn test_strip_quotes() {
        assert_eq!(strip_quotes("'hello'"), "hello");
        assert_eq!(strip_quotes("hello"), "hello");
        assert_eq!(strip_quotes(" 'hello' "), "hello");
    }

    // -----------------------------------------------------------------------
    // Phase 4d: parse_replicated_params tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_parse_replicated_params_standard() {
        let ddl = "CREATE TABLE default.t (id UInt64) ENGINE = ReplicatedMergeTree('/clickhouse/tables/{shard}/default/t', '{replica}') ORDER BY id";
        let result = parse_replicated_params(ddl);
        assert!(result.is_some());
        let (path, replica) = result.unwrap();
        assert_eq!(path, "/clickhouse/tables/{shard}/default/t");
        assert_eq!(replica, "{replica}");
    }

    #[test]
    fn test_parse_replicated_params_replacing() {
        let ddl = "CREATE TABLE default.t (id UInt64, ver UInt64) ENGINE = ReplicatedReplacingMergeTree('/path/to/table', 'r1', ver) ORDER BY id";
        let result = parse_replicated_params(ddl);
        assert!(result.is_some());
        let (path, replica) = result.unwrap();
        assert_eq!(path, "/path/to/table");
        assert_eq!(replica, "r1");
    }

    #[test]
    fn test_parse_replicated_params_empty_parens() {
        let ddl = "CREATE TABLE default.t (id UInt64) ENGINE = ReplicatedMergeTree() ORDER BY id";
        let result = parse_replicated_params(ddl);
        assert!(result.is_none());
    }

    #[test]
    fn test_parse_replicated_params_no_replicated() {
        let ddl = "CREATE TABLE default.t (id UInt64) ENGINE = MergeTree() ORDER BY id";
        let result = parse_replicated_params(ddl);
        assert!(result.is_none());
    }

    // -----------------------------------------------------------------------
    // Phase 4d: resolve_zk_macros tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_resolve_zk_macros() {
        let mut macros = HashMap::new();
        macros.insert("database".to_string(), "mydb".to_string());
        macros.insert("table".to_string(), "mytable".to_string());
        macros.insert("shard".to_string(), "01".to_string());
        macros.insert("replica".to_string(), "r1".to_string());
        macros.insert("uuid".to_string(), "abc-123".to_string());

        let result = resolve_zk_macros("/clickhouse/tables/{shard}/{database}/{table}", &macros);
        assert_eq!(result, "/clickhouse/tables/01/mydb/mytable");
    }

    #[test]
    fn test_resolve_zk_macros_partial() {
        let mut macros = HashMap::new();
        macros.insert("shard".to_string(), "01".to_string());
        // {database} and {table} not in map -- should be left as-is
        let result = resolve_zk_macros("/clickhouse/tables/{shard}/{database}/{table}", &macros);
        assert_eq!(result, "/clickhouse/tables/01/{database}/{table}");
    }

    #[test]
    fn test_resolve_zk_macros_empty() {
        let macros = HashMap::new();
        let result = resolve_zk_macros("/clickhouse/tables/{shard}", &macros);
        assert_eq!(result, "/clickhouse/tables/{shard}");
    }

    // -----------------------------------------------------------------------
    // Phase 4d: add_on_cluster_clause tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_add_on_cluster_clause_create_table() {
        let ddl = "CREATE TABLE `default`.`trades` (id UInt64) ENGINE = MergeTree ORDER BY id";
        let result = add_on_cluster_clause(ddl, "my_cluster");
        assert!(
            result.contains("ON CLUSTER 'my_cluster'"),
            "Should contain ON CLUSTER: {}",
            result
        );
        assert!(result.starts_with("CREATE TABLE `default`.`trades` ON CLUSTER 'my_cluster'"));
    }

    #[test]
    fn test_add_on_cluster_clause_create_table_if_not_exists() {
        let ddl = "CREATE TABLE IF NOT EXISTS `default`.`trades` (id UInt64) ENGINE = MergeTree ORDER BY id";
        let result = add_on_cluster_clause(ddl, "c1");
        assert!(result.contains("ON CLUSTER 'c1'"), "Result: {}", result);
        assert!(result.starts_with("CREATE TABLE IF NOT EXISTS `default`.`trades` ON CLUSTER 'c1'"));
    }

    #[test]
    fn test_add_on_cluster_clause_drop_table() {
        let ddl = "DROP TABLE IF EXISTS `default`.`trades` SYNC";
        let result = add_on_cluster_clause(ddl, "cluster1");
        assert_eq!(
            result,
            "DROP TABLE IF EXISTS `default`.`trades` ON CLUSTER 'cluster1' SYNC"
        );
    }

    #[test]
    fn test_add_on_cluster_clause_create_database() {
        let ddl = "CREATE DATABASE IF NOT EXISTS `mydb` ENGINE = Atomic";
        let result = add_on_cluster_clause(ddl, "c1");
        assert_eq!(
            result,
            "CREATE DATABASE IF NOT EXISTS `mydb` ON CLUSTER 'c1' ENGINE = Atomic"
        );
    }

    #[test]
    fn test_add_on_cluster_clause_drop_database() {
        let ddl = "DROP DATABASE IF EXISTS `mydb` SYNC";
        let result = add_on_cluster_clause(ddl, "c1");
        assert_eq!(
            result,
            "DROP DATABASE IF EXISTS `mydb` ON CLUSTER 'c1' SYNC"
        );
    }

    #[test]
    fn test_add_on_cluster_clause_already_present() {
        let ddl = "CREATE TABLE `db`.`t` ON CLUSTER 'existing' (id UInt64) ENGINE = MergeTree";
        let result = add_on_cluster_clause(ddl, "new_cluster");
        assert_eq!(
            result, ddl,
            "Should not modify DDL with existing ON CLUSTER"
        );
    }

    #[test]
    fn test_add_on_cluster_clause_unquoted_names() {
        let ddl = "CREATE TABLE default.trades (id UInt64) ENGINE = MergeTree ORDER BY id";
        let result = add_on_cluster_clause(ddl, "c1");
        assert!(result.contains("ON CLUSTER 'c1'"), "Result: {}", result);
        assert!(result.starts_with("CREATE TABLE default.trades ON CLUSTER 'c1'"));
    }

    // -----------------------------------------------------------------------
    // Phase 4d: rewrite_distributed_cluster tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_rewrite_distributed_cluster() {
        let ddl = "CREATE TABLE default.dist (id UInt64) ENGINE = Distributed('old_cluster', default, trades, rand())";
        let result = rewrite_distributed_cluster(ddl, "new_cluster");
        assert!(
            result.contains("'new_cluster'"),
            "Should contain new_cluster: {}",
            result
        );
        assert!(
            !result.contains("'old_cluster'"),
            "Should not contain old_cluster: {}",
            result
        );
    }

    #[test]
    fn test_rewrite_distributed_cluster_no_distributed() {
        let ddl = "CREATE TABLE default.t (id UInt64) ENGINE = MergeTree ORDER BY id";
        let result = rewrite_distributed_cluster(ddl, "new_cluster");
        assert_eq!(result, ddl, "Non-Distributed DDL should be unchanged");
    }

    #[test]
    fn test_rewrite_distributed_cluster_preserves_args() {
        let ddl = "CREATE TABLE default.dist ENGINE = Distributed('prod_cluster', 'mydb', 'mytable', rand())";
        let result = rewrite_distributed_cluster(ddl, "staging_cluster");
        assert!(result.contains("'staging_cluster'"));
        assert!(result.contains("'mydb'"));
        assert!(result.contains("'mytable'"));
        assert!(result.contains("rand()"));
    }

    #[test]
    fn test_rewrite_ddl_distributed_partial_match_db() {
        // When Distributed engine references matching table but DIFFERENT database,
        // the DDL should NOT be rewritten
        let ddl = "CREATE TABLE other_db.dist (id UInt64) ENGINE = Distributed('cluster', other_db, src_table, rand())";
        let result = rewrite_create_table_ddl(
            ddl,
            "src_db",
            "src_table",
            "dst_db",
            "dst_table",
            "/clickhouse/tables/{shard}/{database}/{table}",
        );
        // The Distributed engine args should be unchanged because db doesn't match
        assert!(
            result.contains("other_db, src_table"),
            "Distributed args should be unchanged for partial db mismatch: {}",
            result
        );
        assert!(
            !result.contains("dst_table, rand"),
            "Should NOT rewrite table arg when db doesn't match: {}",
            result
        );
    }

    #[test]
    fn test_rewrite_ddl_distributed_partial_match_table() {
        // When Distributed engine references matching database but DIFFERENT table,
        // the DDL should NOT be rewritten
        let ddl = "CREATE TABLE src_db.dist (id UInt64) ENGINE = Distributed('cluster', src_db, other_table, rand())";
        let result = rewrite_create_table_ddl(
            ddl,
            "src_db",
            "src_table",
            "dst_db",
            "dst_table",
            "/clickhouse/tables/{shard}/{database}/{table}",
        );
        // The Distributed engine args should be unchanged because table doesn't match
        assert!(
            result.contains("src_db, other_table"),
            "Distributed args should be unchanged for partial table mismatch: {}",
            result
        );
        assert!(
            !result.contains("dst_db"),
            "Should NOT rewrite db arg when table doesn't match: {}",
            result
        );
    }

    #[test]
    fn test_remap_integration_table_keys() {
        // Verify that a manifest with multiple tables gets correctly remapped
        let mut mapping = HashMap::new();
        mapping.insert("prod".to_string(), "staging".to_string());

        let config = RemapConfig {
            rename_as: Vec::new(),
            database_mapping: mapping,
            default_replica_path: String::new(),
        };

        let keys = [
            "prod.users".to_string(),
            "prod.orders".to_string(),
            "logs.events".to_string(),
        ];

        let remapped: Vec<(String, String)> =
            keys.iter().map(|k| config.remap_table_key(k)).collect();

        assert_eq!(remapped[0], ("staging".to_string(), "users".to_string()));
        assert_eq!(remapped[1], ("staging".to_string(), "orders".to_string()));
        assert_eq!(remapped[2], ("logs".to_string(), "events".to_string()));
    }
}
