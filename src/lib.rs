pub mod backup;
pub mod clickhouse;
pub mod concurrency;
pub mod config;
pub mod download;
pub mod error;
pub mod list;
pub mod lock;
pub mod logging;
pub mod manifest;
pub mod object_disk;
pub mod path_encoding;
pub mod progress;
pub mod rate_limiter;
pub mod restore;
pub mod resume;
pub mod server;
pub mod storage;
pub mod table_filter;
pub mod upload;
pub mod watch;

/// Generate a timestamp-based backup name with millisecond precision.
/// Used by CLI and API when no explicit name is provided.
pub fn generate_backup_name() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H%M%S%.3f").to_string()
}

/// Derive an incremental backup name from a `--diff-from-remote` base name.
///
/// Splits on the last `-full-` token, replaces with `-incr-`, and
/// substitutes the trailing timestamp with the current UTC time.
/// Falls back to `generate_backup_name()` if no `-full-` token found.
pub fn derive_incremental_name(base_name: &str) -> String {
    if let Some((prefix, _suffix)) = base_name.rsplit_once("-full-") {
        let now = chrono::Utc::now();
        format!("{}-incr-{}", prefix, now.format("%Y-%m-%d-%H-%M-%S"))
    } else {
        generate_backup_name()
    }
}

/// Spawn a SIGHUP handler that sends `true` on `reload_tx` for config reload.
#[cfg(unix)]
pub fn spawn_sighup_handler(reload_tx: tokio::sync::watch::Sender<bool>) {
    tokio::spawn(async move {
        use tokio::signal::unix::{signal, SignalKind};
        let mut sighup = signal(SignalKind::hangup()).expect("failed to register SIGHUP handler");
        loop {
            sighup.recv().await;
            tracing::info!("SIGHUP received, triggering config reload");
            reload_tx.send(true).ok();
        }
    });
}

/// Spawn a SIGQUIT handler that dumps the stack trace to stderr.
#[cfg(unix)]
pub fn spawn_sigquit_handler() {
    tokio::spawn(async move {
        use tokio::signal::unix::{signal, SignalKind};
        let mut sigquit = signal(SignalKind::quit()).expect("failed to register SIGQUIT handler");
        loop {
            sigquit.recv().await;
            tracing::info!("SIGQUIT received, dumping stack trace to stderr");
            let bt = std::backtrace::Backtrace::force_capture();
            eprintln!("=== SIGQUIT stack dump ===");
            eprintln!("{bt}");
            eprintln!("=== end stack dump ===");
        }
    });
}

#[cfg(test)]
mod tests {
    //! Compile-time verification that all Phase 2c public types and functions
    //! are accessible from the crate root. This test exists to catch wiring
    //! issues where a module is declared but its public API is not reachable.

    #[test]
    fn test_derive_incremental_name_from_full() {
        let name = crate::derive_incremental_name(
            "chi-ch-cluster-ch-deployment-0-0-full-2026-03-08-13-18-19",
        );
        assert!(
            name.starts_with("chi-ch-cluster-ch-deployment-0-0-incr-"),
            "expected -incr- prefix, got: {name}"
        );
        // Should match YYYY-MM-DD-HH-MM-SS suffix
        let suffix = name
            .strip_prefix("chi-ch-cluster-ch-deployment-0-0-incr-")
            .unwrap();
        assert_eq!(suffix.len(), 19, "timestamp should be 19 chars: {suffix}");
    }

    #[test]
    fn test_derive_incremental_name_no_full_token() {
        // No "-full-" token → falls back to generate_backup_name()
        let name = crate::derive_incremental_name("my-backup-2026-03-08");
        // Should be a plain timestamp (no -incr-)
        assert!(
            !name.contains("-incr-"),
            "should fall back to generate_backup_name, got: {name}"
        );
    }

    #[test]
    fn test_derive_incremental_name_rsplit_once() {
        // Multiple "-full-" tokens → splits on the LAST one
        let name = crate::derive_incremental_name("a-full-b-full-2026-01-01-00-00-00");
        assert!(
            name.starts_with("a-full-b-incr-"),
            "should split on last -full-, got: {name}"
        );
    }

    #[test]
    fn test_phase2c_types_importable() {
        // object_disk module: parser types and functions
        use crate::object_disk::{
            is_s3_disk, parse_metadata, rewrite_metadata, serialize_metadata, ObjectDiskMetadata,
            ObjectRef,
        };

        // Verify types are constructible
        let obj_ref = ObjectRef {
            relative_path: "store/abc/data.bin".to_string(),
            size: 1024,
        };
        let metadata = ObjectDiskMetadata {
            version: 2,
            objects: vec![obj_ref],
            total_size: 1024,
            ref_count: 1,
            read_only: false,
            inline_data: None,
        };

        // Verify functions are callable
        assert!(is_s3_disk("s3"));
        assert!(!is_s3_disk("local"));

        let serialized = serialize_metadata(&metadata);
        let parsed = parse_metadata(&serialized).unwrap();
        assert_eq!(parsed.version, 2);
        assert_eq!(parsed.objects.len(), 1);

        let rewritten = rewrite_metadata(&parsed, "store/new_prefix");
        assert!(!rewritten.is_empty());
    }

    #[test]
    fn test_phase2c_concurrency_helpers_importable() {
        // Concurrency helpers for object disk operations
        use crate::concurrency::{
            effective_object_disk_copy_concurrency,
            effective_object_disk_server_side_copy_concurrency,
        };
        use crate::config::Config;

        let config = Config::default();

        // Verify functions return sensible defaults
        let copy_conc = effective_object_disk_copy_concurrency(&config);
        assert!(copy_conc > 0, "object_disk_copy_concurrency must be > 0");

        let server_copy_conc = effective_object_disk_server_side_copy_concurrency(&config);
        assert!(
            server_copy_conc > 0,
            "object_disk_server_side_copy_concurrency must be > 0"
        );
    }

    #[test]
    fn test_phase2c_backup_collect_types_importable() {
        // CollectedPart with disk_name field (Phase 2c addition)
        use crate::backup::collect::CollectedPart;
        use crate::manifest::PartInfo;

        let part = CollectedPart {
            database: "default".to_string(),
            table: "test_table".to_string(),
            disk_name: "s3disk".to_string(),
            part_info: PartInfo::new("202401_1_50_3", 1024, 0),
        };
        assert_eq!(part.disk_name, "s3disk");
        assert_eq!(part.database, "default");
        assert_eq!(part.table, "test_table");
    }

    #[test]
    fn test_phase2c_restore_uuid_prefix_importable() {
        // UUID S3 prefix derivation for restore (Phase 2c addition)
        use crate::restore::attach::uuid_s3_prefix;

        let prefix = uuid_s3_prefix("a1b2c3d4-e5f6-7890-abcd-ef1234567890");
        // Should start with "store/" and include 3-char hex prefix
        assert!(prefix.starts_with("store/"));
        assert!(!prefix.is_empty());
    }

    #[test]
    fn test_phase2c_s3_client_copy_methods_exist() {
        // Verify S3Client has copy_object methods via type signature check.
        // We cannot call these without a real S3 client, but we verify the
        // method signatures compile by referencing them as function pointers.
        use crate::storage::s3::S3Client;

        // Verify S3Client type is accessible
        fn _assert_copy_object_exists(_client: &S3Client) {
            // These method references would fail to compile if the methods
            // do not exist with the expected signatures.
            let _ = S3Client::copy_object;
            let _ = S3Client::copy_object_streaming;
            let _ = S3Client::copy_object_with_retry;
        }
    }

    #[test]
    fn test_phase2c_owned_attach_params_s3_fields() {
        // OwnedAttachParams should have S3-related fields (Phase 2c addition)
        use crate::restore::attach::OwnedAttachParams;

        // Verify the struct has the expected S3 fields by checking it compiles
        // with those fields accessed. We use a helper function to avoid needing
        // to construct a full OwnedAttachParams (which requires many fields).
        fn _assert_s3_fields(params: &OwnedAttachParams) {
            let _ = &params.s3_client;
            let _ = &params.disk_type_map;
            let _ = &params.object_disk_server_side_copy_concurrency;
            let _ = &params.allow_object_disk_streaming;
            let _ = &params.disk_remote_paths;
        }
    }

    #[test]
    fn test_phase3a_deps_available() {
        // Verify Phase 3a dependencies are importable
        use axum::Router;
        use base64::Engine as _;
        use tokio_util::sync::CancellationToken;

        // Verify types are constructible
        let _router: Router = Router::new();
        let _token = CancellationToken::new();
        let _encoded = base64::engine::general_purpose::STANDARD.encode(b"test");
    }

    #[test]
    fn test_phase3b_prometheus_available() {
        use prometheus_client::encoding::text::encode;
        use prometheus_client::metrics::counter::Counter;
        use prometheus_client::metrics::gauge::Gauge;
        use prometheus_client::metrics::histogram::Histogram;
        use prometheus_client::registry::Registry;

        let mut registry = Registry::default();
        let counter = Counter::<u64>::default();
        registry.register("test_counter", "help", counter.clone());
        let mut buf = String::new();
        encode(&mut buf, &registry).unwrap();
        assert!(!buf.is_empty());

        // Verify other types are available
        let _gauge: Gauge = Gauge::default();
        let _histogram = Histogram::new([1.0, 5.0, 10.0]);
    }

    #[test]
    fn test_backup_summary_serializable() {
        use crate::list::BackupSummary;

        let summary = BackupSummary {
            name: "test-backup".to_string(),
            timestamp: Some(chrono::Utc::now()),
            size: 1024,
            compressed_size: 512,
            table_count: 3,
            metadata_size: 256,
            rbac_size: 0,
            config_size: 0,
            object_disk_size: 0,
            required: String::new(),
            is_broken: false,
            broken_reason: None,
        };

        let json = serde_json::to_string(&summary).expect("BackupSummary should serialize to JSON");
        assert!(json.contains("test-backup"));
        assert!(json.contains("1024"));
        assert!(json.contains("512"));
    }
}
