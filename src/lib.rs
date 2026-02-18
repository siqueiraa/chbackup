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
pub mod rate_limiter;
pub mod restore;
pub mod resume;
pub mod storage;
pub mod table_filter;
pub mod upload;

#[cfg(test)]
mod tests {
    //! Compile-time verification that all Phase 2c public types and functions
    //! are accessible from the crate root. This test exists to catch wiring
    //! issues where a module is declared but its public API is not reachable.

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
            part_info: PartInfo {
                name: "202401_1_50_3".to_string(),
                size: 1024,
                backup_key: String::new(),
                source: "uploaded".to_string(),
                checksum_crc64: 0,
                s3_objects: None,
            },
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
}
