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

/// Map a `ChBackupError` to a structured exit code per design doc 11.6.
///
/// Exit codes:
/// - 0: success
/// - 1: general error
/// - 2: usage error (handled by clap before `main()` runs)
/// - 3: backup/manifest not found
/// - 4: lock conflict
/// - 130: SIGINT (Ctrl+C)
/// - 143: SIGTERM
impl ChBackupError {
    pub fn exit_code(&self) -> i32 {
        match self {
            ChBackupError::LockError(_) => 4,
            ChBackupError::BackupError(msg) | ChBackupError::ManifestError(msg)
                if msg.contains("not found") =>
            {
                3
            }
            _ => 1,
        }
    }
}

/// Determine the exit code from an `anyhow::Error` by attempting to
/// downcast to `ChBackupError`. Falls back to 1 (general error).
pub fn exit_code_from_error(err: &anyhow::Error) -> i32 {
    if let Some(e) = err.downcast_ref::<ChBackupError>() {
        e.exit_code()
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_exit_code_lock_error() {
        let err = ChBackupError::LockError("already locked".to_string());
        assert_eq!(err.exit_code(), 4);
    }

    #[test]
    fn test_exit_code_backup_not_found() {
        let err = ChBackupError::BackupError("backup 'daily' not found".to_string());
        assert_eq!(err.exit_code(), 3);
    }

    #[test]
    fn test_exit_code_manifest_not_found() {
        let err = ChBackupError::ManifestError("manifest not found in S3".to_string());
        assert_eq!(err.exit_code(), 3);
    }

    #[test]
    fn test_exit_code_general_backup_error() {
        let err = ChBackupError::BackupError("disk full".to_string());
        assert_eq!(err.exit_code(), 1);
    }

    #[test]
    fn test_exit_code_general_errors() {
        assert_eq!(
            ChBackupError::ClickHouseError("timeout".to_string()).exit_code(),
            1
        );
        assert_eq!(
            ChBackupError::S3Error("access denied".to_string()).exit_code(),
            1
        );
        assert_eq!(
            ChBackupError::ConfigError("invalid".to_string()).exit_code(),
            1
        );
        assert_eq!(
            ChBackupError::RestoreError("failed".to_string()).exit_code(),
            1
        );
    }

    #[test]
    fn test_exit_code_from_anyhow_error() {
        let err: anyhow::Error = ChBackupError::LockError("locked".to_string()).into();
        assert_eq!(exit_code_from_error(&err), 4);
    }

    #[test]
    fn test_exit_code_from_non_chbackup_error() {
        let err = anyhow::anyhow!("some generic error");
        assert_eq!(exit_code_from_error(&err), 1);
    }
}
