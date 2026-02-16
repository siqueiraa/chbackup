//! Table filter with glob pattern matching for the `-t` flag.
//!
//! Supports comma-separated patterns matching `database.table` format.
//! Default pattern `*.*` matches all tables.
//!
//! System databases are always skipped: system, INFORMATION_SCHEMA, information_schema.

use glob::Pattern;

/// System databases that are always excluded from backups.
const SYSTEM_DATABASES: &[&str] = &["system", "INFORMATION_SCHEMA", "information_schema"];

/// Filter for selecting tables by glob patterns.
///
/// Constructed from a comma-separated pattern string (e.g. "default.*,logs.events").
/// Each sub-pattern is matched against `"{db}.{table}"` strings.
#[derive(Debug, Clone)]
pub struct TableFilter {
    patterns: Vec<Pattern>,
}

impl TableFilter {
    /// Create a new filter from a comma-separated glob pattern string.
    ///
    /// Each sub-pattern is compiled as a glob pattern and matched against
    /// `"{db}.{table}"` strings.
    ///
    /// # Examples
    ///
    /// - `"*.*"` matches all tables in all databases
    /// - `"default.*"` matches all tables in the `default` database
    /// - `"*.trades"` matches the `trades` table in any database
    /// - `"default.trades,logs.*"` matches `default.trades` and all `logs` tables
    pub fn new(pattern: &str) -> Self {
        let patterns = pattern
            .split(',')
            .map(|p| p.trim())
            .filter(|p| !p.is_empty())
            .filter_map(|p| Pattern::new(p).ok())
            .collect();
        Self { patterns }
    }

    /// Check if the given database.table combination matches any pattern.
    ///
    /// System databases are always excluded regardless of pattern.
    pub fn matches(&self, db: &str, table: &str) -> bool {
        if is_system_database(db) {
            return false;
        }
        let full_name = format!("{db}.{table}");
        self.patterns.iter().any(|p| p.matches(&full_name))
    }
}

/// Check if a database is a system database.
fn is_system_database(db: &str) -> bool {
    SYSTEM_DATABASES.contains(&db)
}

/// Check if a table matches any of the skip patterns (exclusion filter).
///
/// Used with `config.clickhouse.skip_tables` to exclude specific tables
/// from backup.
pub fn is_excluded(db: &str, table: &str, skip_patterns: &[String]) -> bool {
    let full_name = format!("{db}.{table}");
    for pattern_str in skip_patterns {
        if let Ok(pattern) = Pattern::new(pattern_str) {
            if pattern.matches(&full_name) {
                return true;
            }
        }
    }
    false
}

/// Check if a table engine is in the skip list.
pub fn is_engine_excluded(engine: &str, skip_engines: &[String]) -> bool {
    skip_engines.iter().any(|e| e == engine)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_table_filter_exact_match() {
        let filter = TableFilter::new("default.trades");
        assert!(filter.matches("default", "trades"));
        assert!(!filter.matches("default", "orders"));
        assert!(!filter.matches("logs", "trades"));
    }

    #[test]
    fn test_table_filter_wildcard_db() {
        let filter = TableFilter::new("default.*");
        assert!(filter.matches("default", "trades"));
        assert!(filter.matches("default", "orders"));
        assert!(filter.matches("default", "anything"));
        assert!(!filter.matches("logs", "events"));
    }

    #[test]
    fn test_table_filter_wildcard_table() {
        let filter = TableFilter::new("*.trades");
        assert!(filter.matches("default", "trades"));
        assert!(filter.matches("logs", "trades"));
        assert!(filter.matches("analytics", "trades"));
        assert!(!filter.matches("default", "orders"));
    }

    #[test]
    fn test_table_filter_star_star() {
        let filter = TableFilter::new("*.*");
        assert!(filter.matches("default", "trades"));
        assert!(filter.matches("logs", "events"));
        assert!(filter.matches("analytics", "data"));
    }

    #[test]
    fn test_table_filter_comma_separated() {
        let filter = TableFilter::new("default.trades,logs.*");
        assert!(filter.matches("default", "trades"));
        assert!(!filter.matches("default", "orders"));
        assert!(filter.matches("logs", "events"));
        assert!(filter.matches("logs", "errors"));
        assert!(!filter.matches("analytics", "data"));
    }

    #[test]
    fn test_table_filter_system_databases_excluded() {
        let filter = TableFilter::new("*.*");
        assert!(!filter.matches("system", "tables"));
        assert!(!filter.matches("system", "parts"));
        assert!(!filter.matches("INFORMATION_SCHEMA", "columns"));
        assert!(!filter.matches("information_schema", "tables"));
        // Non-system databases still match
        assert!(filter.matches("default", "trades"));
    }

    #[test]
    fn test_table_filter_spaces_in_pattern() {
        let filter = TableFilter::new("default.trades , logs.* ");
        assert!(filter.matches("default", "trades"));
        assert!(filter.matches("logs", "events"));
    }

    #[test]
    fn test_is_excluded() {
        let skip = vec![
            "system.*".to_string(),
            "default.internal_*".to_string(),
        ];
        assert!(is_excluded("system", "tables", &skip));
        assert!(is_excluded("system", "parts", &skip));
        assert!(is_excluded("default", "internal_queue", &skip));
        assert!(!is_excluded("default", "trades", &skip));
        assert!(!is_excluded("logs", "events", &skip));
    }

    #[test]
    fn test_is_excluded_empty_patterns() {
        let skip: Vec<String> = Vec::new();
        assert!(!is_excluded("default", "trades", &skip));
    }

    #[test]
    fn test_is_engine_excluded() {
        let skip = vec!["Kafka".to_string(), "S3Queue".to_string()];
        assert!(is_engine_excluded("Kafka", &skip));
        assert!(is_engine_excluded("S3Queue", &skip));
        assert!(!is_engine_excluded("MergeTree", &skip));
        assert!(!is_engine_excluded("ReplicatedMergeTree", &skip));
    }

    #[test]
    fn test_table_filter_question_mark_wildcard() {
        let filter = TableFilter::new("default.trade?");
        assert!(filter.matches("default", "trades"));
        assert!(filter.matches("default", "trader"));
        assert!(!filter.matches("default", "trading"));
    }
}
