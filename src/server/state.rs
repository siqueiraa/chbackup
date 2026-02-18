//! Application state and operation management for the API server.
//!
//! `AppState` is shared across all axum handlers via `State<AppState>`.
//! It provides operation lifecycle management with concurrency control
//! via a semaphore (single-op when allow_parallel=false).

use std::sync::Arc;

use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;

use crate::clickhouse::ChClient;
use crate::config::Config;
use crate::storage::S3Client;

use super::actions::ActionLog;

/// Shared application state for all axum handlers.
///
/// Must be `Clone` for axum `State` extractor. All inner fields are
/// `Arc`-wrapped or implement `Clone`.
#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub ch: ChClient,
    pub s3: S3Client,
    pub action_log: Arc<Mutex<ActionLog>>,
    pub current_op: Arc<Mutex<Option<RunningOp>>>,
    pub op_semaphore: Arc<Semaphore>,
}

/// Tracks a currently running operation for cancellation support.
pub struct RunningOp {
    pub id: u64,
    pub command: String,
    pub cancel_token: CancellationToken,
    /// Held for the duration of the operation to enforce concurrency limits.
    _permit: OwnedSemaphorePermit,
}

impl AppState {
    /// Create a new AppState from config and client instances.
    ///
    /// The semaphore permits are set based on `config.api.allow_parallel`:
    /// - `false` (default): 1 permit -- operations are serialized
    /// - `true`: effectively unlimited permits
    pub fn new(config: Arc<Config>, ch: ChClient, s3: S3Client) -> Self {
        let permits = if config.api.allow_parallel {
            // Use a large number to approximate unlimited
            Semaphore::MAX_PERMITS
        } else {
            1
        };

        Self {
            config,
            ch,
            s3,
            action_log: Arc::new(Mutex::new(ActionLog::new(100))),
            current_op: Arc::new(Mutex::new(None)),
            op_semaphore: Arc::new(Semaphore::new(permits)),
        }
    }

    /// Try to start a new operation. Returns (action_id, cancellation_token) on success.
    ///
    /// If the semaphore cannot be acquired (another operation is running and
    /// allow_parallel=false), returns an error.
    pub async fn try_start_op(
        &self,
        command: &str,
    ) -> Result<(u64, CancellationToken), &'static str> {
        let permit = self
            .op_semaphore
            .clone()
            .try_acquire_owned()
            .map_err(|_| "operation already in progress")?;

        let token = CancellationToken::new();
        let id = {
            let mut log = self.action_log.lock().await;
            log.start(command.to_string())
        };

        {
            let mut current = self.current_op.lock().await;
            *current = Some(RunningOp {
                id,
                command: command.to_string(),
                cancel_token: token.clone(),
                _permit: permit,
            });
        }

        Ok((id, token))
    }

    /// Mark an operation as completed successfully.
    pub async fn finish_op(&self, id: u64) {
        {
            let mut log = self.action_log.lock().await;
            log.finish(id);
        }
        {
            let mut current = self.current_op.lock().await;
            if current.as_ref().is_some_and(|op| op.id == id) {
                *current = None;
            }
        }
    }

    /// Mark an operation as failed with an error message.
    pub async fn fail_op(&self, id: u64, error: String) {
        {
            let mut log = self.action_log.lock().await;
            log.fail(id, error);
        }
        {
            let mut current = self.current_op.lock().await;
            if current.as_ref().is_some_and(|op| op.id == id) {
                *current = None;
            }
        }
    }

    /// Cancel the currently running operation.
    ///
    /// Returns `true` if an operation was cancelled, `false` if no operation was running.
    pub async fn kill_current(&self) -> bool {
        let mut current = self.current_op.lock().await;
        if let Some(op) = current.take() {
            op.cancel_token.cancel();
            let mut log = self.action_log.lock().await;
            log.kill(op.id);
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    /// Helper to create an AppState for testing without real CH/S3 clients.
    /// We cannot construct real ChClient/S3Client without servers, so we test
    /// the operation management logic through the action_log and semaphore directly.
    fn test_config(allow_parallel: bool) -> Arc<Config> {
        let mut config = Config::default();
        config.api.allow_parallel = allow_parallel;
        Arc::new(config)
    }

    #[tokio::test]
    async fn test_app_state_operation_lifecycle() {
        // Test the action log and semaphore behavior directly
        let _config = test_config(false);
        let action_log = Arc::new(Mutex::new(ActionLog::new(100)));
        let op_semaphore = Arc::new(Semaphore::new(1));

        // Start an operation
        let permit = op_semaphore
            .clone()
            .try_acquire_owned()
            .expect("Should acquire permit");

        let id = {
            let mut log = action_log.lock().await;
            log.start("create".to_string())
        };
        assert_eq!(id, 1);

        // Verify running
        {
            let log = action_log.lock().await;
            assert!(log.running().is_some());
            assert_eq!(log.running().unwrap().id, 1);
        }

        // Cannot acquire another permit (allow_parallel=false, 1 permit)
        assert!(op_semaphore.clone().try_acquire_owned().is_err());

        // Finish operation
        {
            let mut log = action_log.lock().await;
            log.finish(id);
        }
        drop(permit);

        // Verify completed
        {
            let log = action_log.lock().await;
            assert!(log.running().is_none());
        }

        // Can acquire permit again
        assert!(op_semaphore.clone().try_acquire_owned().is_ok());
    }

    #[tokio::test]
    async fn test_sequential_ops_blocked() {
        let _config = test_config(false);
        let op_semaphore = Arc::new(Semaphore::new(1));

        // Acquire first permit
        let _permit1 = op_semaphore
            .clone()
            .try_acquire_owned()
            .expect("Should acquire first permit");

        // Second acquire should fail
        let result = op_semaphore.clone().try_acquire_owned();
        assert!(result.is_err(), "Should be blocked by first operation");
    }

    #[tokio::test]
    async fn test_kill_cancels_token() {
        let token = CancellationToken::new();
        let child = token.clone();

        assert!(!child.is_cancelled());
        token.cancel();
        assert!(child.is_cancelled());
    }

    #[tokio::test]
    async fn test_parallel_ops_allowed() {
        let op_semaphore = Arc::new(Semaphore::new(Semaphore::MAX_PERMITS));

        // Should be able to acquire multiple permits
        let _permit1 = op_semaphore
            .clone()
            .try_acquire_owned()
            .expect("Should acquire first permit");
        let _permit2 = op_semaphore
            .clone()
            .try_acquire_owned()
            .expect("Should acquire second permit");
        let _permit3 = op_semaphore
            .clone()
            .try_acquire_owned()
            .expect("Should acquire third permit");
    }
}
