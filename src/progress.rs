//! Progress bar wrapper for upload/download pipelines.
//!
//! Uses the `indicatif` crate to display progress bars when running in a TTY.
//! The progress bar is automatically disabled in non-TTY environments (e.g.,
//! piped output, server mode) or when `config.general.disable_progress_bar`
//! is true.

use std::sync::Arc;

use indicatif::{ProgressBar, ProgressStyle};

/// Wraps an optional `indicatif::ProgressBar` for thread-safe progress tracking.
///
/// When disabled (non-TTY or config), all operations are no-ops.
#[derive(Clone)]
pub struct ProgressTracker {
    bar: Option<Arc<ProgressBar>>,
}

impl ProgressTracker {
    /// Create a new progress tracker.
    ///
    /// # Arguments
    ///
    /// * `operation` - Label for the progress bar (e.g., "Upload", "Download")
    /// * `total_parts` - Total number of parts to process
    /// * `disable` - If true, the progress bar is not displayed
    pub fn new(operation: &str, total_parts: u64, disable: bool) -> Self {
        if disable || total_parts == 0 || !std::io::IsTerminal::is_terminal(&std::io::stdout()) {
            return Self { bar: None };
        }

        let bar = ProgressBar::new(total_parts);
        let template = format!(
            "{} {{bar:40.cyan/blue}} {{percent}}% {{pos}}/{{len}} parts {{per_sec}} ETA {{eta}}",
            operation
        );
        if let Ok(style) = ProgressStyle::with_template(&template) {
            bar.set_style(style.progress_chars("##-"));
        }

        Self {
            bar: Some(Arc::new(bar)),
        }
    }

    /// Create a disabled tracker (for testing or server mode).
    pub fn disabled() -> Self {
        Self { bar: None }
    }

    /// Increment progress by one part.
    pub fn inc(&self) {
        if let Some(ref bar) = self.bar {
            bar.inc(1);
        }
    }

    /// Mark the progress bar as complete.
    pub fn finish(&self) {
        if let Some(ref bar) = self.bar {
            bar.finish_with_message("done");
        }
    }

    /// Check if the progress bar is active (not disabled).
    pub fn is_active(&self) -> bool {
        self.bar.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_progress_tracker_disabled() {
        let tracker = ProgressTracker::new("Upload", 10, true);
        assert!(!tracker.is_active());

        // Operations should be no-ops
        tracker.inc();
        tracker.finish();
    }

    #[test]
    fn test_progress_tracker_disabled_helper() {
        let tracker = ProgressTracker::disabled();
        assert!(!tracker.is_active());
        tracker.inc();
        tracker.finish();
    }

    #[test]
    fn test_progress_tracker_zero_parts() {
        let tracker = ProgressTracker::new("Download", 0, false);
        assert!(!tracker.is_active());
    }

    #[test]
    fn test_progress_tracker_clone() {
        let tracker = ProgressTracker::disabled();
        let cloned = tracker.clone();
        assert!(!cloned.is_active());
    }
}
