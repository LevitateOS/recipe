//! Colored output and progress reporting for Recipe
//!
//! Uses owo-colors for terminal colors and indicatif for progress bars.

use indicatif::{ProgressBar, ProgressStyle};
use owo_colors::OwoColorize;
use std::time::Duration;

/// Print an action header (blue, bold)
/// Example: "==> Installing ripgrep"
pub fn action(message: &str) {
    println!("{} {}", "==>".blue().bold(), message.bold());
}

/// Print an action with package counter (blue, bold)
/// Example: "(1/5) Installing ripgrep"
pub fn action_numbered(current: usize, total: usize, message: &str) {
    println!(
        "{} {}",
        format!("({}/{})", current, total).cyan(),
        message.bold()
    );
}

/// Print a sub-action (cyan arrow)
/// Example: "  -> acquire"
pub fn sub_action(phase: &str) {
    println!("  {} {}", "->".cyan(), phase);
}

/// Print a detail line (dimmed prefix)
/// Example: "     downloading https://..."
pub fn detail(message: &str) {
    println!("     {}", message.dimmed());
}

/// Print a success message (green)
/// Example: "==> ripgrep installed"
pub fn success(message: &str) {
    println!("{} {}", "==>".green().bold(), message.green());
}

/// Print an info message (cyan)
pub fn info(message: &str) {
    println!("{} {}", "::".cyan(), message);
}

/// Print a warning message (yellow)
pub fn warning(message: &str) {
    eprintln!("{} {}", "warning:".yellow().bold(), message.yellow());
}

/// Print an error message (red)
pub fn error(message: &str) {
    eprintln!("{} {}", "error:".red().bold(), message.red());
}

/// Print a skip message (dimmed)
/// Example: "==> ripgrep already installed, skipping"
pub fn skip(message: &str) {
    println!("{} {}", "==>".dimmed(), message.dimmed());
}

/// Print package status in list output
pub fn list_item(name: &str, status: &str, is_installed: bool) {
    if is_installed {
        println!("  {} {}", name.green(), status.dimmed());
    } else {
        println!("  {} {}", name, status.dimmed());
    }
}

/// Create a download progress bar
pub fn download_progress(total_size: u64) -> ProgressBar {
    let pb = ProgressBar::new(total_size);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("     {spinner:.cyan} [{bar:30.cyan/dim}] {bytes}/{total_bytes} ({eta})")
            .unwrap()
            .progress_chars("━╸━"),
    );
    pb.enable_steady_tick(Duration::from_millis(100));
    pb
}

/// Create an indeterminate progress bar (spinner) for build phase
pub fn build_spinner(message: &str) -> ProgressBar {
    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::default_spinner()
            .template("  {spinner:.cyan} {msg}")
            .unwrap()
            .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"),
    );
    pb.set_message(message.to_string());
    pb.enable_steady_tick(Duration::from_millis(80));
    pb
}

/// Create a simple spinner for operations
pub fn spinner(message: &str) -> ProgressBar {
    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::default_spinner()
            .template("     {spinner:.cyan} {msg}")
            .unwrap()
            .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"),
    );
    pb.set_message(message.to_string());
    pb.enable_steady_tick(Duration::from_millis(80));
    pb
}

/// Finish a progress bar with a success message
pub fn progress_success(pb: ProgressBar, message: &str) {
    pb.finish_with_message(format!("{}", message.green()));
}

/// Finish a progress bar with a failure message
pub fn progress_fail(pb: ProgressBar, message: &str) {
    pb.finish_with_message(format!("{}", message.red()));
}

/// Finish a progress bar and clear it
pub fn progress_done(pb: ProgressBar) {
    pb.finish_and_clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    use leviso_cheat_test::cheat_reviewed;

    #[cheat_reviewed("API test - progress bar can be created")]
    #[test]
    fn test_progress_bar_creation() {
        let pb = download_progress(1000);
        pb.finish_and_clear();
    }

    #[cheat_reviewed("API test - spinner can be created")]
    #[test]
    fn test_spinner_creation() {
        let pb = build_spinner("Building");
        pb.finish_and_clear();
    }
}
