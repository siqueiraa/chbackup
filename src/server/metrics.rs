//! Prometheus metrics definitions for the chbackup API server.
//!
//! Defines a `Metrics` struct holding a custom `prometheus_client::registry::Registry` and
//! all metric instances from design doc section 9 and roadmap Phase 3b.
//!
//! Uses `prometheus-client` crate (OpenMetrics-compatible) with a custom (non-global)
//! registry for clean testing and isolation.

use prometheus_client::encoding::text::encode;
use prometheus_client::encoding::EncodeLabelSet;
use prometheus_client::metrics::counter::Counter;
use prometheus_client::metrics::family::Family;
use prometheus_client::metrics::gauge::Gauge;
use prometheus_client::metrics::histogram::Histogram;
use prometheus_client::registry::Registry;
use std::sync::atomic::AtomicI64;
use std::sync::atomic::AtomicU64;

/// Label set for per-operation metrics (duration, errors, successes).
#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct OperationLabels {
    pub operation: String,
}

impl OperationLabels {
    /// Convenience constructor for creating operation labels.
    pub fn new(operation: &str) -> Self {
        Self {
            operation: operation.to_string(),
        }
    }
}

/// Holds all Prometheus metric instances for the chbackup server.
///
/// Each metric is prefixed with `chbackup_` to avoid collisions.
/// The custom `Registry` owns all metric registrations and is used
/// during `/metrics` text encoding.
pub struct Metrics {
    /// Custom registry (not global) for clean testing.
    pub registry: Registry,

    /// Per-operation duration histogram (labels: operation = create|upload|download|restore|...).
    pub backup_duration_seconds: Family<OperationLabels, Histogram>,

    /// Last backup compressed size in bytes.
    pub backup_size_bytes: Gauge<f64, AtomicU64>,

    /// Unix timestamp of the last successful create/upload operation.
    pub backup_last_success_timestamp: Gauge<f64, AtomicU64>,

    /// Cumulative count of parts uploaded.
    pub parts_uploaded_total: Counter,

    /// Cumulative count of parts skipped via diff-from (incremental).
    pub parts_skipped_incremental_total: Counter,

    /// Error count per operation type (labels: operation).
    pub errors_total: Family<OperationLabels, Counter>,

    /// Successful operation count per type (labels: operation).
    pub successful_operations_total: Family<OperationLabels, Counter>,

    /// Current number of local backups (refreshed on scrape).
    pub number_backups_local: Gauge<i64, AtomicI64>,

    /// Current number of remote backups (refreshed on scrape).
    pub number_backups_remote: Gauge<i64, AtomicI64>,

    /// 1 if an operation is currently running, 0 otherwise.
    pub in_progress: Gauge<i64, AtomicI64>,

    /// Watch state gauge (registered, set to 0 until Phase 3d).
    pub watch_state: Gauge<i64, AtomicI64>,

    /// Unix timestamp of last full watch backup (registered, 0 until Phase 3d).
    pub watch_last_full_timestamp: Gauge<f64, AtomicU64>,

    /// Unix timestamp of last incremental watch backup (registered, 0 until Phase 3d).
    pub watch_last_incremental_timestamp: Gauge<f64, AtomicU64>,

    /// Consecutive watch errors count (registered, 0 until Phase 3d).
    pub watch_consecutive_errors: Gauge<i64, AtomicI64>,
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}

impl Metrics {
    /// Create a new `Metrics` instance with all metrics registered in a custom registry.
    pub fn new() -> Self {
        let mut registry = Registry::default();

        // Per-operation duration histogram
        let backup_duration_seconds =
            Family::<OperationLabels, Histogram>::new_with_constructor(|| {
                Histogram::new(
                    [1.0, 5.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0, 1800.0, 3600.0],
                )
            });
        registry.register(
            "chbackup_backup_duration_seconds",
            "Duration of backup operations in seconds",
            backup_duration_seconds.clone(),
        );

        // Last backup size
        let backup_size_bytes: Gauge<f64, AtomicU64> = Gauge::default();
        registry.register(
            "chbackup_backup_size_bytes",
            "Compressed size of the last backup in bytes",
            backup_size_bytes.clone(),
        );

        // Last success timestamp
        let backup_last_success_timestamp: Gauge<f64, AtomicU64> = Gauge::default();
        registry.register(
            "chbackup_backup_last_success_timestamp",
            "Unix timestamp of the last successful create or upload operation",
            backup_last_success_timestamp.clone(),
        );

        // Parts uploaded counter
        // Note: prometheus-client automatically appends _total suffix to counters
        // per OpenMetrics convention, so we register without the _total suffix.
        let parts_uploaded_total = Counter::default();
        registry.register(
            "chbackup_parts_uploaded",
            "Cumulative count of parts uploaded to S3",
            parts_uploaded_total.clone(),
        );

        // Parts skipped via incremental
        let parts_skipped_incremental_total = Counter::default();
        registry.register(
            "chbackup_parts_skipped_incremental",
            "Cumulative count of parts skipped via diff-from incremental backup",
            parts_skipped_incremental_total.clone(),
        );

        // Errors per operation
        let errors_total = Family::<OperationLabels, Counter>::default();
        registry.register(
            "chbackup_errors",
            "Cumulative count of errors per operation type",
            errors_total.clone(),
        );

        // Successful operations per type
        let successful_operations_total = Family::<OperationLabels, Counter>::default();
        registry.register(
            "chbackup_successful_operations",
            "Cumulative count of successful operations per type",
            successful_operations_total.clone(),
        );

        // Backup counts
        let number_backups_local: Gauge<i64, AtomicI64> = Gauge::default();
        registry.register(
            "chbackup_number_backups_local",
            "Current number of local backups",
            number_backups_local.clone(),
        );

        let number_backups_remote: Gauge<i64, AtomicI64> = Gauge::default();
        registry.register(
            "chbackup_number_backups_remote",
            "Current number of remote backups",
            number_backups_remote.clone(),
        );

        // In-progress gauge
        let in_progress: Gauge<i64, AtomicI64> = Gauge::default();
        registry.register(
            "chbackup_in_progress",
            "1 if a backup operation is currently running, 0 otherwise",
            in_progress.clone(),
        );

        // Watch-related gauges (Phase 3d -- registered but default to 0)
        let watch_state: Gauge<i64, AtomicI64> = Gauge::default();
        registry.register(
            "chbackup_watch_state",
            "Watch mode state: 0=inactive, 1=idle, 2=running",
            watch_state.clone(),
        );

        let watch_last_full_timestamp: Gauge<f64, AtomicU64> = Gauge::default();
        registry.register(
            "chbackup_watch_last_full_timestamp",
            "Unix timestamp of the last full watch backup",
            watch_last_full_timestamp.clone(),
        );

        let watch_last_incremental_timestamp: Gauge<f64, AtomicU64> = Gauge::default();
        registry.register(
            "chbackup_watch_last_incremental_timestamp",
            "Unix timestamp of the last incremental watch backup",
            watch_last_incremental_timestamp.clone(),
        );

        let watch_consecutive_errors: Gauge<i64, AtomicI64> = Gauge::default();
        registry.register(
            "chbackup_watch_consecutive_errors",
            "Number of consecutive watch errors",
            watch_consecutive_errors.clone(),
        );

        // Initialize label combinations so they appear in encoded output
        // even before any observations are made.
        let operations = [
            "create",
            "upload",
            "download",
            "restore",
            "create_remote",
            "restore_remote",
            "delete",
            "clean_broken_remote",
            "clean_broken_local",
            "clean",
        ];
        for op in &operations {
            let labels = OperationLabels::new(op);
            let _ = backup_duration_seconds.get_or_create(&labels);
            let _ = errors_total.get_or_create(&labels);
            let _ = successful_operations_total.get_or_create(&labels);
        }

        Self {
            registry,
            backup_duration_seconds,
            backup_size_bytes,
            backup_last_success_timestamp,
            parts_uploaded_total,
            parts_skipped_incremental_total,
            errors_total,
            successful_operations_total,
            number_backups_local,
            number_backups_remote,
            in_progress,
            watch_state,
            watch_last_full_timestamp,
            watch_last_incremental_timestamp,
            watch_consecutive_errors,
        }
    }

    /// Encode all registered metrics into OpenMetrics text exposition format.
    pub fn encode(&self) -> Result<String, std::fmt::Error> {
        let mut buf = String::new();
        encode(&mut buf, &self.registry)?;
        Ok(buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_metrics_new_registers_all() {
        let metrics = Metrics::new();
        let text = metrics.encode().expect("encode() should succeed");

        // Verify all 14 metric families are registered by checking HELP lines
        let expected = [
            "chbackup_backup_duration_seconds",
            "chbackup_backup_size_bytes",
            "chbackup_backup_last_success_timestamp",
            "chbackup_parts_uploaded_total",
            "chbackup_parts_skipped_incremental_total",
            "chbackup_errors_total",
            "chbackup_successful_operations_total",
            "chbackup_number_backups_local",
            "chbackup_number_backups_remote",
            "chbackup_in_progress",
            "chbackup_watch_state",
            "chbackup_watch_last_full_timestamp",
            "chbackup_watch_last_incremental_timestamp",
            "chbackup_watch_consecutive_errors",
        ];

        for name in &expected {
            assert!(
                text.contains(name),
                "Expected metric '{}' to be registered in encoded output, got:\n{}",
                name,
                text
            );
        }
    }

    #[test]
    fn test_metrics_encode_text() {
        let metrics = Metrics::new();
        let text = metrics.encode().expect("encode() should succeed");

        // Verify output is non-empty and contains expected metric names
        assert!(!text.is_empty(), "Encoded text should not be empty");

        // Check for HELP lines in prometheus/OpenMetrics text format
        assert!(
            text.contains("# HELP chbackup_backup_duration_seconds"),
            "Should contain HELP for duration histogram"
        );
        assert!(
            text.contains("# HELP chbackup_in_progress"),
            "Should contain HELP for in_progress gauge"
        );
        assert!(
            text.contains("chbackup_number_backups_local"),
            "Should contain number_backups_local metric"
        );
        assert!(
            text.contains("chbackup_errors_total"),
            "Should contain errors_total metric"
        );
    }

    #[test]
    fn test_metrics_counter_increment() {
        let metrics = Metrics::new();

        // Increment the errors_total counter for "create" operation
        metrics
            .errors_total
            .get_or_create(&OperationLabels::new("create"))
            .inc();

        let text = metrics.encode().expect("encode() should succeed");

        // Verify the counter shows count 1 for "create"
        assert!(
            text.contains("chbackup_errors_total{operation=\"create\"} 1"),
            "errors_total for create should be 1, got:\n{}",
            text
        );

        // Increment again
        metrics
            .errors_total
            .get_or_create(&OperationLabels::new("create"))
            .inc();

        let text2 = metrics.encode().expect("encode() should succeed");
        assert!(
            text2.contains("chbackup_errors_total{operation=\"create\"} 2"),
            "errors_total for create should be 2 after second increment, got:\n{}",
            text2
        );
    }

    #[test]
    fn test_metrics_duration_observation() {
        let metrics = Metrics::new();

        // Observe a duration for the "create" operation
        metrics
            .backup_duration_seconds
            .get_or_create(&OperationLabels::new("create"))
            .observe(42.5);

        let text = metrics.encode().expect("encode() should succeed");

        // Verify the histogram recorded 1 observation
        assert!(
            text.contains("chbackup_backup_duration_seconds_count{operation=\"create\"} 1"),
            "Duration histogram should have count 1 for create, got:\n{}",
            text
        );
        assert!(
            text.contains("chbackup_backup_duration_seconds_sum{operation=\"create\"} 42.5"),
            "Duration histogram should have sum 42.5 for create, got:\n{}",
            text
        );
    }

    #[test]
    fn test_metrics_error_increment() {
        let metrics = Metrics::new();

        // Increment error counter for "create"
        metrics
            .errors_total
            .get_or_create(&OperationLabels::new("create"))
            .inc();

        let text = metrics.encode().expect("encode() should succeed");

        assert!(
            text.contains("chbackup_errors_total{operation=\"create\"} 1"),
            "errors_total for create should be 1, got:\n{}",
            text
        );
    }

    #[test]
    fn test_metrics_success_increment() {
        let metrics = Metrics::new();

        // Increment successful operations counter for "upload"
        metrics
            .successful_operations_total
            .get_or_create(&OperationLabels::new("upload"))
            .inc();

        let text = metrics.encode().expect("encode() should succeed");

        assert!(
            text.contains("chbackup_successful_operations_total{operation=\"upload\"} 1"),
            "successful_operations_total for upload should be 1, got:\n{}",
            text
        );
    }

    #[test]
    fn test_metrics_size_gauge() {
        let metrics = Metrics::new();

        // Set backup_size_bytes gauge
        metrics.backup_size_bytes.set(123456789.0);

        let text = metrics.encode().expect("encode() should succeed");

        // OpenMetrics format may represent the value differently
        assert!(
            text.contains("chbackup_backup_size_bytes 1.23456789e8")
                || text.contains("chbackup_backup_size_bytes 123456789")
                || text.contains("chbackup_backup_size_bytes 1.23456789E8"),
            "backup_size_bytes should be 123456789, got:\n{}",
            text
        );
    }
}
