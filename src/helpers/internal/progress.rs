//! The one progress indicator style. Bars draw to out-of-band stderr
//! ([`crate::diagnostics::progress_target`]) and clear themselves when dropped.

use indicatif::{ProgressBar, ProgressStyle};
use std::time::Duration;

/// A spinner that can become a byte bar; cleared on drop, including on error paths.
pub struct Spinner(ProgressBar);

impl Spinner {
    pub fn new(message: &str) -> Self {
        let bar = ProgressBar::with_draw_target(None, crate::diagnostics::progress_target());
        bar.set_style(
            ProgressStyle::default_spinner()
                .template("     {spinner:.cyan} {msg}")
                .expect("static progress template")
                .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"),
        );
        bar.set_message(message.to_string());
        bar.enable_steady_tick(Duration::from_millis(80));
        Self(bar)
    }

    /// Show byte progress toward `total_bytes`.
    pub fn bytes(&self, total_bytes: u64) {
        self.0.set_length(total_bytes);
        self.0.set_style(
            ProgressStyle::default_bar()
                .template("     {spinner:.cyan} [{bar:30.cyan/dim}] {bytes}/{total_bytes} ({eta})")
                .expect("static progress template")
                .progress_chars("━╸━"),
        );
    }

    pub fn set_position(&self, bytes: u64) {
        self.0.set_position(bytes);
    }
}

impl Drop for Spinner {
    fn drop(&mut self) {
        self.0.finish_and_clear();
    }
}
