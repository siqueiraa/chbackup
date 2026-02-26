//! Prometheus metrics definitions for the chbackup API server.
//!
//! Defines a `Metrics` struct holding a custom `prometheus::Registry` and
//! all metric instances from design doc section 9 and roadmap Phase 3b.
//!
//! Uses a custom (non-global) registry for clean testing and isolation.

use prometheus::{
    Encoder, Gauge, HistogramOpts, HistogramVec, IntCounter, IntCounterVec, IntGauge, Registry,
    TextEncoder,
};

/// Holds all Prometheus metric instances for the chbackup server.
///
/// Each metric is prefixed with `chbackup_` to avoid collisions.
/// The custom `Registry` owns all metric registrations and is used
/// during `/metrics` text encoding.
pub struct Metrics {
    /// Custom registry (not global) for clean testing.
    pub registry: Registry,

    /// Per-operation duration histogram (labels: operation = create|upload|download|restore|...).
    pub backup_duration_seconds: HistogramVec,

    /// Last backup compressed size in bytes.
    pub backup_size_bytes: Gauge,

    /// Unix timestamp of the last successful create/upload operation.
    pub backup_last_success_timestamp: Gauge,

    /// Cumulative count of parts uploaded.
    pub parts_uploaded_total: IntCounter,

    /// Cumulative count of parts skipped via diff-from (incremental).
    pub parts_skipped_incremental_total: IntCounter,

    /// Error count per operation type (labels: operation).
    pub errors_total: IntCounterVec,

    /// Successful operation count per type (labels: operation).
    pub successful_operations_total: IntCounterVec,

    /// Current number of local backups (refreshed on scrape).
    pub number_backups_local: IntGauge,

    /// Current number of remote backups (refreshed on scrape).
    pub number_backups_remote: IntGauge,

    /// 1 if an operation is currently running, 0 otherwise.
    pub in_progress: IntGauge,

    /// Watch state gauge (registered, set to 0 until Phase 3d).
    pub watch_state: IntGauge,

    /// Unix timestamp of last full watch backup (registered, 0 until Phase 3d).
    pub watch_last_full_timestamp: Gauge,

    /// Unix timestamp of last incremental watch backup (registered, 0 until Phase 3d).
    pub watch_last_incremental_timestamp: Gauge,

    /// Consecutive watch errors count (registered, 0 until Phase 3d).
    pub watch_consecutive_errors: IntGauge,
}

impl Metrics {
    /// Create a new `Metrics` instance with all metrics registered in a custom registry.
    pub fn new() -> Result<Self, prometheus::Error> {
        let registry = Registry::new();

        // Per-operation duration histogram
        let backup_duration_seconds = HistogramVec::new(
            HistogramOpts::new(
                "chbackup_backup_duration_seconds",
                "Duration of backup operations in seconds",
            )
            .buckets(vec![
                1.0, 5.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0, 1800.0, 3600.0,
            ]),
            &["operation"],
        )?;
        registry.register(Box::new(backup_duration_seconds.clone()))?;

        // Last backup size
        let backup_size_bytes = Gauge::new(
            "chbackup_backup_size_bytes",
            "Compressed size of the last backup in bytes",
        )?;
        registry.register(Box::new(backup_size_bytes.clone()))?;

        // Last success timestamp
        let backup_last_success_timestamp = Gauge::new(
            "chbackup_backup_last_success_timestamp",
            "Unix timestamp of the last successful create or upload operation",
        )?;
        registry.register(Box::new(backup_last_success_timestamp.clone()))?;

        // Parts uploaded counter
        let parts_uploaded_total = IntCounter::new(
            "chbackup_parts_uploaded_total",
            "Cumulative count of parts uploaded to S3",
        )?;
        registry.register(Box::new(parts_uploaded_total.clone()))?;

        // Parts skipped via incremental
        let parts_skipped_incremental_total = IntCounter::new(
            "chbackup_parts_skipped_incremental_total",
            "Cumulative count of parts skipped via diff-from incremental backup",
        )?;
        registry.register(Box::new(parts_skipped_incremental_total.clone()))?;

        // Errors per operation
        let errors_total = IntCounterVec::new(
            prometheus::Opts::new(
                "chbackup_errors_total",
                "Cumulative count of errors per operation type",
            ),
            &["operation"],
        )?;
        registry.register(Box::new(errors_total.clone()))?;

        // Successful operations per type
        let successful_operations_total = IntCounterVec::new(
            prometheus::Opts::new(
                "chbackup_successful_operations_total",
                "Cumulative count of successful operations per type",
            ),
            &["operation"],
        )?;
        registry.register(Box::new(successful_operations_total.clone()))?;

        // Backup counts
        let number_backups_local = IntGauge::new(
            "chbackup_number_backups_local",
            "Current number of local backups",
        )?;
        registry.register(Box::new(number_backups_local.clone()))?;

        let number_backups_remote = IntGauge::new(
            "chbackup_number_backups_remote",
            "Current number of remote backups",
        )?;
        registry.register(Box::new(number_backups_remote.clone()))?;

        // In-progress gauge
        let in_progress = IntGauge::new(
            "chbackup_in_progress",
            "1 if a backup operation is currently running, 0 otherwise",
        )?;
        registry.register(Box::new(in_progress.clone()))?;

        // Watch-related gauges (Phase 3d -- registered but default to 0)
        let watch_state = IntGauge::new(
            "chbackup_watch_state",
            "Watch mode state: 0=inactive, 1=idle, 2=running",
        )?;
        registry.register(Box::new(watch_state.clone()))?;

        let watch_last_full_timestamp = Gauge::new(
            "chbackup_watch_last_full_timestamp",
            "Unix timestamp of the last full watch backup",
        )?;
        registry.register(Box::new(watch_last_full_timestamp.clone()))?;

        let watch_last_incremental_timestamp = Gauge::new(
            "chbackup_watch_last_incremental_timestamp",
            "Unix timestamp of the last incremental watch backup",
        )?;
        registry.register(Box::new(watch_last_incremental_timestamp.clone()))?;

        let watch_consecutive_errors = IntGauge::new(
            "chbackup_watch_consecutive_errors",
            "Number of consecutive watch errors",
        )?;
        registry.register(Box::new(watch_consecutive_errors.clone()))?;

        // Initialize label combinations so they appear in gather() output
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
            backup_duration_seconds.with_label_values(&[op]);
            errors_total.with_label_values(&[op]);
            successful_operations_total.with_label_values(&[op]);
        }

        Ok(Self {
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
        })
    }

    /// Encode all registered metrics into Prometheus text exposition format.
    pub fn encode(&self) -> Result<String, prometheus::Error> {
        let encoder = TextEncoder::new();
        let families = self.registry.gather();
        let mut buffer = Vec::new();
        encoder.encode(&families, &mut buffer)?;
        Ok(String::from_utf8(buffer).unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_metrics_new_registers_all() {
        let metrics = Metrics::new().expect("Metrics::new() should succeed");
        let families = metrics.registry.gather();

        // Collect all registered metric family names
        let names: Vec<&str> = families.iter().map(|f| f.get_name()).collect();

        // Verify all 14 metric families are registered
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
                names.contains(name),
                "Expected metric '{}' to be registered, but found: {:?}",
                name,
                names
            );
        }

        assert_eq!(
            families.len(),
            14,
            "Expected 14 metric families, got {}",
            families.len()
        );
    }

    #[test]
    fn test_metrics_encode_text() {
        let metrics = Metrics::new().expect("Metrics::new() should succeed");
        let text = metrics.encode().expect("encode() should succeed");

        // Verify output is non-empty and contains expected metric names
        assert!(!text.is_empty(), "Encoded text should not be empty");

        // Check for HELP and TYPE lines in prometheus text format
        assert!(
            text.contains("# HELP chbackup_backup_duration_seconds"),
            "Should contain HELP for duration histogram"
        );
        assert!(
            text.contains("# TYPE chbackup_backup_duration_seconds histogram"),
            "Should contain TYPE for duration histogram"
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
        let metrics = Metrics::new().expect("Metrics::new() should succeed");

        // Increment the errors_total counter for "create" operation
        metrics.errors_total.with_label_values(&["create"]).inc();

        let text = metrics.encode().expect("encode() should succeed");

        // Verify the counter shows count 1 for "create"
        assert!(
            text.contains("chbackup_errors_total{operation=\"create\"} 1"),
            "errors_total for create should be 1, got:\n{}",
            text
        );

        // Increment again
        metrics.errors_total.with_label_values(&["create"]).inc();

        let text2 = metrics.encode().expect("encode() should succeed");
        assert!(
            text2.contains("chbackup_errors_total{operation=\"create\"} 2"),
            "errors_total for create should be 2 after second increment, got:\n{}",
            text2
        );
    }

    #[test]
    fn test_metrics_duration_observation() {
        let metrics = Metrics::new().expect("Metrics::new() should succeed");

        // Observe a duration for the "create" operation
        metrics
            .backup_duration_seconds
            .with_label_values(&["create"])
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
        let metrics = Metrics::new().expect("Metrics::new() should succeed");

        // Increment error counter for "create"
        metrics.errors_total.with_label_values(&["create"]).inc();

        let text = metrics.encode().expect("encode() should succeed");

        assert!(
            text.contains("chbackup_errors_total{operation=\"create\"} 1"),
            "errors_total for create should be 1, got:\n{}",
            text
        );
    }

    #[test]
    fn test_metrics_success_increment() {
        let metrics = Metrics::new().expect("Metrics::new() should succeed");

        // Increment successful operations counter for "upload"
        metrics
            .successful_operations_total
            .with_label_values(&["upload"])
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
        let metrics = Metrics::new().expect("Metrics::new() should succeed");

        // Set backup_size_bytes gauge
        metrics.backup_size_bytes.set(123456789.0);

        let text = metrics.encode().expect("encode() should succeed");

        assert!(
            text.contains("chbackup_backup_size_bytes 1.23456789e8")
                || text.contains("chbackup_backup_size_bytes 123456789"),
            "backup_size_bytes should be 123456789, got:\n{}",
            text
        );
    }
}
