//! Colored output and progress reporting for Recipe
//!
//! Uses owo-colors for terminal colors and indicatif for progress bars.

use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};
use owo_colors::OwoColorize;
use serde_json::json;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

static MACHINE_EVENTS_ENABLED: AtomicBool = AtomicBool::new(false);

/// Enable or disable machine-friendly hook events.
pub fn set_machine_events(enabled: bool) {
    MACHINE_EVENTS_ENABLED.store(enabled, Ordering::Relaxed);
}

fn machine_events() -> bool {
    MACHINE_EVENTS_ENABLED.load(Ordering::Relaxed)
}

/// Print an action header (blue, bold)
/// Example: "==> Installing ripgrep"
pub fn action(message: &str) {
    eprintln!("{} {}", "==>".blue().bold(), message.bold());
}

/// Print an action with package counter (blue, bold)
/// Example: "(1/5) Installing ripgrep"
pub fn action_numbered(current: usize, total: usize, message: &str) {
    eprintln!(
        "{} {}",
        format!("({}/{})", current, total).cyan(),
        message.bold()
    );
}

/// Print a sub-action (cyan arrow)
/// Example: "  -> acquire"
pub fn sub_action(phase: &str) {
    eprintln!("  {} {}", "->".cyan(), phase);
}

/// Print a detail line (dimmed prefix)
/// Example: "     downloading https://..."
pub fn detail(message: &str) {
    eprintln!("     {}", message.dimmed());
}

/// Print a success message (green)
/// Example: "==> ripgrep installed"
pub fn success(message: &str) {
    eprintln!("{} {}", "==>".green().bold(), message.green());
}

/// Print an info message (cyan)
pub fn info(message: &str) {
    eprintln!("{} {}", "::".cyan(), message);
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
    eprintln!("{} {}", "==>".dimmed(), message.dimmed());
}

/// Emit a recipe hook event.
/// When machine events are enabled, outputs JSON:
/// {"event":"recipe-hook","recipe":"...","hook":"...","status":"...","msg":"..."}
/// Otherwise outputs parser-friendly text:
/// [recipe-hook] recipe=<name> hook=<name> status=<status> msg="<msg>"
pub fn hook_event(recipe: &str, hook: &str, status: &str, msg: &str) {
    let escape = |value: &str| {
        value
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', " ")
    };

    if machine_events() {
        match serde_json::to_string(&json!({
            "event": "recipe-hook",
            "recipe": recipe,
            "hook": hook,
            "status": status,
            "msg": msg,
        })) {
            Ok(payload) => eprintln!("{}", payload),
            Err(_) => eprintln!(
                "[recipe-hook] recipe=\"{}\" hook=\"{}\" status=\"{}\" msg=\"{}\"",
                escape(recipe),
                escape(hook),
                escape(status),
                escape(msg)
            ),
        }
    } else {
        eprintln!(
            "[recipe-hook] recipe=\"{}\" hook=\"{}\" status=\"{}\" msg=\"{}\"",
            escape(recipe),
            escape(hook),
            escape(status),
            escape(msg)
        );
    }
}

/// Print package status in list output
pub fn list_item(name: &str, status: &str, is_installed: bool) {
    if is_installed {
        eprintln!("  {} {}", name.green(), status.dimmed());
    } else {
        eprintln!("  {} {}", name, status.dimmed());
    }
}

/// Create a download progress bar
pub fn download_progress(total_size: u64) -> ProgressBar {
    let pb = ProgressBar::new(total_size);
    pb.set_draw_target(ProgressDrawTarget::stderr());
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
    pb.set_draw_target(ProgressDrawTarget::stderr());
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
    pb.set_draw_target(ProgressDrawTarget::stderr());
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
