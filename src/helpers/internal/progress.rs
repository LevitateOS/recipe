//! Unified progress bar helpers
//!
//! Provides consistent progress bar styling across all recipe helpers.

use indicatif::{ProgressBar, ProgressStyle};
use std::time::Duration;

/// Standard spinner characters used throughout recipe
const SPINNER_CHARS: &str = "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏";

/// Standard tick interval for spinners
const TICK_INTERVAL_MS: u64 = 80;

/// Create a spinner progress bar with standard styling.
pub fn create_spinner(message: &str) -> ProgressBar {
    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::default_spinner()
            .template("     {spinner:.cyan} {msg}")
            .unwrap()
            .tick_chars(SPINNER_CHARS),
    );
    pb.set_message(message.to_string());
    pb.enable_steady_tick(Duration::from_millis(TICK_INTERVAL_MS));
    pb
}

/// Upgrade a spinner to a byte progress bar when content length becomes known.
pub fn upgrade_to_bytes(pb: &ProgressBar, total_bytes: u64) {
    pb.set_length(total_bytes);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("     {spinner:.cyan} [{bar:30.cyan/dim}] {bytes}/{total_bytes} ({eta})")
            .unwrap()
            .progress_chars("━╸━"),
    );
}

/// RAII guard that clears a progress bar when dropped.
///
/// Useful for ensuring progress bars are cleaned up even on errors.
pub struct ProgressGuard<'a>(&'a ProgressBar);

impl<'a> ProgressGuard<'a> {
    pub fn new(pb: &'a ProgressBar) -> Self {
        Self(pb)
    }
}

impl Drop for ProgressGuard<'_> {
    fn drop(&mut self) {
        self.0.finish_and_clear();
    }
}
