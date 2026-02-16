use chbackup::config::Config;
use std::io::Write;
use std::sync::Mutex;
use tempfile::NamedTempFile;

/// Global lock to prevent env var tests from running in parallel.
/// Tests that modify environment variables must hold this lock.
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Clear all env vars that the config overlay reads, so tests don't
/// contaminate each other.
fn clear_config_env_vars() {
    std::env::remove_var("S3_BUCKET");
    std::env::remove_var("S3_REGION");
    std::env::remove_var("S3_ENDPOINT");
    std::env::remove_var("S3_PREFIX");
    std::env::remove_var("S3_ACCESS_KEY");
    std::env::remove_var("S3_SECRET_KEY");
    std::env::remove_var("S3_ASSUME_ROLE_ARN");
    std::env::remove_var("S3_FORCE_PATH_STYLE");
    std::env::remove_var("CLICKHOUSE_HOST");
    std::env::remove_var("CLICKHOUSE_PORT");
    std::env::remove_var("CLICKHOUSE_USERNAME");
    std::env::remove_var("CLICKHOUSE_PASSWORD");
    std::env::remove_var("CLICKHOUSE_DATA_PATH");
    std::env::remove_var("CHBACKUP_LOG_LEVEL");
    std::env::remove_var("CHBACKUP_LOG_FORMAT");
    std::env::remove_var("API_LISTEN");
}

#[test]
fn test_default_config_serializes() {
    // Default config should serialize to valid YAML
    let yaml = Config::default_yaml().expect("default_yaml should succeed");
    assert!(!yaml.is_empty(), "Default YAML should not be empty");

    // The YAML should contain all 7 sections
    assert!(yaml.contains("general:"), "Missing general section");
    assert!(yaml.contains("clickhouse:"), "Missing clickhouse section");
    assert!(yaml.contains("s3:"), "Missing s3 section");
    assert!(yaml.contains("backup:"), "Missing backup section");
    assert!(yaml.contains("retention:"), "Missing retention section");
    assert!(yaml.contains("watch:"), "Missing watch section");
    assert!(yaml.contains("api:"), "Missing api section");

    // Should be parseable back to Config
    let config: Config = serde_yaml::from_str(&yaml).expect("Should parse back from YAML");
    assert_eq!(config.general.log_level, "info");
    assert_eq!(config.clickhouse.port, 9000);
    assert_eq!(config.s3.bucket, "my-backup-bucket");
}

#[test]
fn test_config_from_yaml() {
    let _guard = ENV_LOCK.lock().unwrap();
    clear_config_env_vars();

    // Parse a minimal YAML config with a few overrides
    let yaml = r#"
general:
  log_level: debug
  upload_concurrency: 8
clickhouse:
  host: ch-server.local
  port: 9440
  secure: true
s3:
  bucket: prod-backups
  region: eu-west-1
"#;

    let mut tmpfile = NamedTempFile::new().expect("create temp file");
    tmpfile
        .write_all(yaml.as_bytes())
        .expect("write yaml to temp file");

    let config = Config::load(tmpfile.path(), &[]).expect("Config::load should succeed");

    // Overridden values
    assert_eq!(config.general.log_level, "debug");
    assert_eq!(config.general.upload_concurrency, 8);
    assert_eq!(config.clickhouse.host, "ch-server.local");
    assert_eq!(config.clickhouse.port, 9440);
    assert!(config.clickhouse.secure);
    assert_eq!(config.s3.bucket, "prod-backups");
    assert_eq!(config.s3.region, "eu-west-1");

    // Default values should be preserved for fields not in YAML
    assert_eq!(config.general.log_format, "text");
    assert_eq!(config.general.download_concurrency, 4);
    assert_eq!(config.clickhouse.username, "default");
    assert_eq!(config.s3.storage_class, "STANDARD");
    assert_eq!(config.backup.compression, "lz4");
    assert_eq!(config.watch.full_interval, "24h");
    assert_eq!(config.api.listen, "localhost:7171");
}

#[test]
fn test_env_overlay() {
    let _guard = ENV_LOCK.lock().unwrap();
    clear_config_env_vars();

    // Environment variable overlay should override config values
    std::env::set_var("S3_BUCKET", "env-bucket-override");
    std::env::set_var("CLICKHOUSE_HOST", "env-ch-host");

    let yaml = r#"
s3:
  bucket: yaml-bucket
clickhouse:
  host: yaml-host
"#;

    let mut tmpfile = NamedTempFile::new().expect("create temp file");
    tmpfile
        .write_all(yaml.as_bytes())
        .expect("write yaml to temp file");

    let config = Config::load(tmpfile.path(), &[]).expect("Config::load should succeed");

    // Env vars should override YAML values
    assert_eq!(config.s3.bucket, "env-bucket-override");
    assert_eq!(config.clickhouse.host, "env-ch-host");

    // Clean up env vars
    clear_config_env_vars();
}

#[test]
fn test_cli_env_override() {
    let _guard = ENV_LOCK.lock().unwrap();
    clear_config_env_vars();

    // CLI --env overrides should take priority over env vars
    std::env::set_var("S3_BUCKET", "env-bucket");

    let yaml = r#"
s3:
  bucket: yaml-bucket
clickhouse:
  host: yaml-host
"#;

    let mut tmpfile = NamedTempFile::new().expect("create temp file");
    tmpfile
        .write_all(yaml.as_bytes())
        .expect("write yaml to temp file");

    let overrides = vec![
        "s3.bucket=cli-bucket".to_string(),
        "clickhouse.host=cli-host".to_string(),
        "clickhouse.port=9999".to_string(),
    ];

    let config = Config::load(tmpfile.path(), &overrides).expect("Config::load should succeed");

    // CLI overrides should win
    assert_eq!(config.s3.bucket, "cli-bucket");
    assert_eq!(config.clickhouse.host, "cli-host");
    assert_eq!(config.clickhouse.port, 9999);

    // Clean up
    clear_config_env_vars();
}

#[test]
fn test_validation_full_interval() {
    let _guard = ENV_LOCK.lock().unwrap();
    clear_config_env_vars();

    // full_interval <= watch_interval should fail when watch is enabled
    let yaml = r#"
watch:
  enabled: true
  watch_interval: 24h
  full_interval: 1h
"#;

    let mut tmpfile = NamedTempFile::new().expect("create temp file");
    tmpfile
        .write_all(yaml.as_bytes())
        .expect("write yaml to temp file");

    let result = Config::load(tmpfile.path(), &[]);
    assert!(
        result.is_err(),
        "Should fail when full_interval <= watch_interval"
    );
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("full_interval") && err_msg.contains("watch_interval"),
        "Error should mention full_interval and watch_interval, got: {}",
        err_msg
    );
}
