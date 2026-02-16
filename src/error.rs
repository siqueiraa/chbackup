use thiserror::Error;

/// Top-level error type for chbackup operations.
#[derive(Debug, Error)]
pub enum ChBackupError {
    #[error("ClickHouse error: {0}")]
    ClickHouseError(String),

    #[error("S3 error: {0}")]
    S3Error(String),

    #[error("Configuration error: {0}")]
    ConfigError(String),

    #[error("Lock error: {0}")]
    LockError(String),

    #[error("Backup error: {0}")]
    BackupError(String),

    #[error("Restore error: {0}")]
    RestoreError(String),

    #[error("Manifest error: {0}")]
    ManifestError(String),

    #[error("I/O error: {0}")]
    IoError(#[from] std::io::Error),
}
