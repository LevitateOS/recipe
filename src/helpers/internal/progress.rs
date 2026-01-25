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
///
/// # Example
/// ```ignore
/// let pb = create_spinner("downloading foo.tar.gz");
/// // ... do work ...
/// pb.finish_and_clear();
/// ```
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

/// Create a progress bar with byte tracking (for downloads).
///
/// # Example
/// ```ignore
/// let pb = create_byte_progress(1024 * 1024); // 1MB total
/// pb.set_position(512 * 1024); // 512KB done
/// pb.finish_and_clear();
/// ```
pub fn create_byte_progress(total_bytes: u64) -> ProgressBar {
    let pb = ProgressBar::new(total_bytes);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("     {spinner:.cyan} [{bar:30.cyan/dim}] {bytes}/{total_bytes} ({eta})")
            .unwrap()
            .progress_chars("━╸━"),
    );
    pb.enable_steady_tick(Duration::from_millis(TICK_INTERVAL_MS));
    pb
}

/// Create a spinner that can optionally upgrade to a byte progress bar.
///
/// Returns a spinner initially. Call `upgrade_to_bytes()` if content length is known.
///
/// # Example
/// ```ignore
/// let pb = create_download_progress("downloading foo.tar.gz");
/// if let Some(len) = content_length {
///     upgrade_to_bytes(&pb, len);
/// }
/// ```
pub fn create_download_progress(message: &str) -> ProgressBar {
    create_spinner(message)
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
///
/// # Example
/// ```ignore
/// let pb = create_spinner("working...");
/// let _guard = ProgressGuard::new(&pb);
/// do_fallible_work()?; // pb cleared even if this fails
/// ```
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

/// Run a closure with a spinner, clearing it when done.
///
/// # Example
/// ```ignore
/// let result = with_spinner("extracting...", || {
///     extract_archive()?;
///     Ok(())
/// })?;
/// ```
pub fn with_spinner<T, E>(
    message: &str,
    f: impl FnOnce() -> Result<T, E>,
) -> Result<T, E> {
    let pb = create_spinner(message);
    let result = f();
    pb.finish_and_clear();
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_spinner() {
        let pb = create_spinner("test message");
        assert!(!pb.is_finished());
        pb.finish_and_clear();
        assert!(pb.is_finished());
    }

    #[test]
    fn test_create_byte_progress() {
        let pb = create_byte_progress(1000);
        pb.set_position(500);
        assert_eq!(pb.position(), 500);
        pb.finish_and_clear();
    }

    #[test]
    fn test_progress_guard_clears_on_drop() {
        let pb = create_spinner("test");
        {
            let _guard = ProgressGuard::new(&pb);
            assert!(!pb.is_finished());
        }
        assert!(pb.is_finished());
    }

    #[test]
    fn test_with_spinner() {
        let result: Result<i32, &str> = with_spinner("test", || Ok(42));
        assert_eq!(result.unwrap(), 42);
    }
}
