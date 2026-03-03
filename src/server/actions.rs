//! ActionLog ring buffer and ActionEntry types for tracking server operations.
//!
//! The ActionLog maintains a bounded ring buffer of recent operations.
//! Each operation has a lifecycle: Running -> Completed | Failed | Killed.

use std::collections::VecDeque;

use chrono::{DateTime, Utc};
use serde::Serialize;

/// Status of an action in the ActionLog.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionStatus {
    Running,
    Completed,
    Failed(String),
    Killed,
}

impl std::fmt::Display for ActionStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ActionStatus::Running => write!(f, "running"),
            ActionStatus::Completed => write!(f, "completed"),
            ActionStatus::Failed(e) => write!(f, "failed: {}", e),
            ActionStatus::Killed => write!(f, "killed"),
        }
    }
}

/// A single action entry in the log.
#[derive(Debug, Clone, Serialize)]
pub struct ActionEntry {
    /// Unique monotonic ID for this action.
    pub id: u64,
    /// Command name (e.g., "create", "upload", "download", "restore").
    pub command: String,
    /// When the action started.
    pub start: DateTime<Utc>,
    /// When the action finished (None if still running).
    pub finish: Option<DateTime<Utc>>,
    /// Current status of the action.
    pub status: ActionStatus,
}

/// Ring buffer of recent actions with a configurable capacity.
///
/// When the buffer is full, the oldest entry is removed to make room for new ones.
pub struct ActionLog {
    entries: VecDeque<ActionEntry>,
    capacity: usize,
    next_id: u64,
}

impl ActionLog {
    /// Create a new ActionLog with the given capacity.
    pub fn new(capacity: usize) -> Self {
        Self {
            entries: VecDeque::with_capacity(capacity),
            capacity,
            next_id: 1,
        }
    }

    /// Start a new action. Pushes a Running entry and returns the action ID.
    ///
    /// If the log is at capacity, the oldest entry is removed first.
    pub fn start(&mut self, command: String) -> u64 {
        let id = self.next_id;
        self.next_id += 1;

        if self.entries.len() >= self.capacity {
            self.entries.pop_front();
        }

        self.entries.push_back(ActionEntry {
            id,
            command,
            start: Utc::now(),
            finish: None,
            status: ActionStatus::Running,
        });

        id
    }

    /// Mark an action as completed successfully.
    ///
    /// No-op if the entry is already in a terminal `Killed` state so that
    /// auto-resume tasks (which run on a fresh token and cannot be cancelled)
    /// cannot overwrite a `Killed` status set by an explicit kill request.
    pub fn finish(&mut self, id: u64) {
        if let Some(entry) = self.entries.iter_mut().find(|e| e.id == id) {
            if matches!(entry.status, ActionStatus::Killed) {
                return;
            }
            entry.finish = Some(Utc::now());
            entry.status = ActionStatus::Completed;
        }
    }

    /// Mark an action as failed with an error message.
    ///
    /// No-op if the entry is already in a terminal `Killed` state (same
    /// reasoning as `finish`).
    pub fn fail(&mut self, id: u64, error: String) {
        if let Some(entry) = self.entries.iter_mut().find(|e| e.id == id) {
            if matches!(entry.status, ActionStatus::Killed) {
                return;
            }
            entry.finish = Some(Utc::now());
            entry.status = ActionStatus::Failed(error);
        }
    }

    /// Mark an action as killed (cancelled).
    pub fn kill(&mut self, id: u64) {
        if let Some(entry) = self.entries.iter_mut().find(|e| e.id == id) {
            entry.finish = Some(Utc::now());
            entry.status = ActionStatus::Killed;
        }
    }

    /// Get all entries in the log.
    pub fn entries(&self) -> &VecDeque<ActionEntry> {
        &self.entries
    }

    /// Find the currently running action, if any.
    pub fn running(&self) -> Option<&ActionEntry> {
        self.entries
            .iter()
            .find(|e| matches!(e.status, ActionStatus::Running))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_action_log_start_finish() {
        let mut log = ActionLog::new(10);

        let id = log.start("create".to_string());
        assert_eq!(id, 1);
        assert_eq!(log.entries().len(), 1);

        let entry = &log.entries()[0];
        assert!(matches!(entry.status, ActionStatus::Running));
        assert!(entry.finish.is_none());

        log.finish(id);

        let entry = &log.entries()[0];
        assert!(matches!(entry.status, ActionStatus::Completed));
        assert!(entry.finish.is_some());
    }

    #[test]
    fn test_action_log_capacity() {
        let mut log = ActionLog::new(3);

        let _id1 = log.start("create".to_string());
        let _id2 = log.start("upload".to_string());
        let _id3 = log.start("download".to_string());
        assert_eq!(log.entries().len(), 3);

        // Adding a 4th should drop the oldest
        let id4 = log.start("restore".to_string());
        assert_eq!(log.entries().len(), 3);

        // Oldest should be id2 (id1 was dropped)
        assert_eq!(log.entries()[0].id, 2);
        assert_eq!(log.entries()[2].id, id4);
    }

    #[test]
    fn test_action_log_fail() {
        let mut log = ActionLog::new(10);

        let id = log.start("upload".to_string());
        log.fail(id, "connection timeout".to_string());

        let entry = &log.entries()[0];
        assert!(matches!(&entry.status, ActionStatus::Failed(e) if e == "connection timeout"));
        assert!(entry.finish.is_some());
    }

    #[test]
    fn test_action_log_running() {
        let mut log = ActionLog::new(10);

        // No running action initially
        assert!(log.running().is_none());

        let id = log.start("create".to_string());
        assert!(log.running().is_some());
        assert_eq!(log.running().unwrap().id, id);

        // After finishing, no running action
        log.finish(id);
        assert!(log.running().is_none());
    }

    #[test]
    fn test_action_log_kill() {
        let mut log = ActionLog::new(10);

        let id = log.start("restore".to_string());
        log.kill(id);

        let entry = &log.entries()[0];
        assert!(matches!(entry.status, ActionStatus::Killed));
        assert!(entry.finish.is_some());
    }

    #[test]
    fn test_action_entry_serializable() {
        let entry = ActionEntry {
            id: 1,
            command: "create".to_string(),
            start: Utc::now(),
            finish: None,
            status: ActionStatus::Running,
        };

        let json = serde_json::to_string(&entry).expect("ActionEntry should serialize");
        assert!(json.contains("\"command\":\"create\""));
        assert!(json.contains("\"running\""));
    }

    // -----------------------------------------------------------------------
    // ActionStatus::Display tests -- covers lines 21-30 (~8 lines)
    // -----------------------------------------------------------------------

    #[test]
    fn test_action_status_display_running() {
        let status = ActionStatus::Running;
        assert_eq!(format!("{}", status), "running");
    }

    #[test]
    fn test_action_status_display_completed() {
        let status = ActionStatus::Completed;
        assert_eq!(format!("{}", status), "completed");
    }

    #[test]
    fn test_action_status_display_failed() {
        let status = ActionStatus::Failed("timeout".to_string());
        assert_eq!(format!("{}", status), "failed: timeout");
    }

    #[test]
    fn test_action_status_display_killed() {
        let status = ActionStatus::Killed;
        assert_eq!(format!("{}", status), "killed");
    }

    // -----------------------------------------------------------------------
    // ActionLog: finish/fail does NOT override Killed status
    // -----------------------------------------------------------------------

    #[test]
    fn test_action_log_finish_does_not_override_killed() {
        let mut log = ActionLog::new(10);
        let id = log.start("create".to_string());

        // Kill the action first
        log.kill(id);
        assert!(matches!(log.entries()[0].status, ActionStatus::Killed));

        // Now try to finish it -- should be a no-op
        log.finish(id);
        assert!(
            matches!(log.entries()[0].status, ActionStatus::Killed),
            "finish() must not override Killed status"
        );
    }

    #[test]
    fn test_action_log_fail_does_not_override_killed() {
        let mut log = ActionLog::new(10);
        let id = log.start("upload".to_string());

        // Kill the action first
        log.kill(id);

        // Now try to fail it -- should be a no-op
        log.fail(id, "should not appear".to_string());
        assert!(
            matches!(log.entries()[0].status, ActionStatus::Killed),
            "fail() must not override Killed status"
        );
    }

    // -----------------------------------------------------------------------
    // ActionLog edge cases
    // -----------------------------------------------------------------------

    #[test]
    fn test_action_log_kill_nonexistent_id() {
        let mut log = ActionLog::new(10);
        let id = log.start("create".to_string());

        // Killing a non-existent ID should be a no-op
        log.kill(id + 999);

        // Original entry should still be Running
        assert!(matches!(log.entries()[0].status, ActionStatus::Running));
    }

    #[test]
    fn test_action_log_finish_nonexistent_id() {
        let mut log = ActionLog::new(10);
        let _id = log.start("create".to_string());

        // Finishing a non-existent ID should be a no-op
        log.finish(999);

        // Original entry should still be Running
        assert!(matches!(log.entries()[0].status, ActionStatus::Running));
    }

    #[test]
    fn test_action_log_fail_nonexistent_id() {
        let mut log = ActionLog::new(10);
        let _id = log.start("create".to_string());

        // Failing a non-existent ID should be a no-op
        log.fail(999, "error".to_string());

        // Original entry should still be Running
        assert!(matches!(log.entries()[0].status, ActionStatus::Running));
    }

    #[test]
    fn test_action_log_entries_order() {
        let mut log = ActionLog::new(10);
        let id1 = log.start("create".to_string());
        let id2 = log.start("upload".to_string());
        let id3 = log.start("download".to_string());

        let entries = log.entries();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].id, id1);
        assert_eq!(entries[0].command, "create");
        assert_eq!(entries[1].id, id2);
        assert_eq!(entries[1].command, "upload");
        assert_eq!(entries[2].id, id3);
        assert_eq!(entries[2].command, "download");
    }

    #[test]
    fn test_action_log_monotonic_ids() {
        let mut log = ActionLog::new(10);
        let id1 = log.start("op1".to_string());
        let id2 = log.start("op2".to_string());
        let id3 = log.start("op3".to_string());

        assert_eq!(id1, 1);
        assert_eq!(id2, 2);
        assert_eq!(id3, 3);
    }

    #[test]
    fn test_action_log_capacity_one() {
        let mut log = ActionLog::new(1);
        let _id1 = log.start("first".to_string());
        assert_eq!(log.entries().len(), 1);

        let id2 = log.start("second".to_string());
        assert_eq!(log.entries().len(), 1);
        assert_eq!(log.entries()[0].id, id2);
        assert_eq!(log.entries()[0].command, "second");
    }

    #[test]
    fn test_action_log_running_returns_first_running() {
        let mut log = ActionLog::new(10);
        let id1 = log.start("op1".to_string());
        let _id2 = log.start("op2".to_string());

        // running() returns the FIRST running entry
        let running = log.running().unwrap();
        assert_eq!(running.id, id1);
    }

    #[test]
    fn test_action_status_serialization() {
        // Running
        let json = serde_json::to_string(&ActionStatus::Running).unwrap();
        assert_eq!(json, "\"running\"");

        // Completed
        let json = serde_json::to_string(&ActionStatus::Completed).unwrap();
        assert_eq!(json, "\"completed\"");

        // Killed
        let json = serde_json::to_string(&ActionStatus::Killed).unwrap();
        assert_eq!(json, "\"killed\"");

        // Failed (tagged enum)
        let json = serde_json::to_string(&ActionStatus::Failed("oops".to_string())).unwrap();
        assert!(json.contains("failed"));
        assert!(json.contains("oops"));
    }
}
