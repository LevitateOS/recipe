//! Human-readable output rendering for recipe operations.

use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};
use owo_colors::OwoColorize;
use std::time::Duration;

use crate::core::events::RecipeHookEvent;

/// Print an action header (blue, bold)
/// Example: "==> Installing ripgrep"
pub fn action(message: &str) {
    eprintln!("{}", message.bold());
}

/// Print an action with package counter (blue, bold)
/// Example: "(1/5) Installing ripgrep"
pub fn action_numbered(_current: usize, _total: usize, message: &str) {
    eprintln!("{}", message.bold());
}

/// Print a sub-action (cyan arrow)
/// Example: "  -> acquire"
pub fn sub_action(phase: &str) {
    eprintln!(
        "{}",
        format!("Now working through the {phase} step.").cyan()
    );
}

/// Print a detail line (dimmed prefix)
/// Example: "     downloading https://..."
pub fn detail(message: &str) {
    eprintln!("{}", message.dimmed());
}

/// Print a success message (green)
/// Example: "==> ripgrep installed"
pub fn success(message: &str) {
    eprintln!("{}", message.green());
}

/// Print an info message (cyan)
pub fn info(message: &str) {
    eprintln!("{message}");
}

/// Print a warning message (yellow)
pub fn warning(message: &str) {
    eprintln!("{}", message.yellow());
}

/// Print an error message (red)
pub fn error(message: &str) {
    eprintln!("{}", message.red());
}

/// Print a skip message (dimmed)
/// Example: "==> ripgrep already installed, skipping"
pub fn skip(message: &str) {
    eprintln!("{}", message.dimmed());
}

pub(crate) fn format_hook_line(event: &RecipeHookEvent) -> String {
    let details = event.msg.trim();
    sentence_for_event(&event.hook, &event.status, details)
}

fn sentence_for_event(hook: &str, status: &str, details: &str) -> String {
    let (verb, base) = match status {
        "requested" => (
            "queue",
            "To keep the flow in order, this step will queue now.".to_string(),
        ),
        "running" => match hook {
            "prepare" | "dependency.prepare" => (
                "prepare",
                "Before anything else moves forward, we prepare the environment now.".to_string(),
            ),
            "acquire" | "dependency.acquire" => (
                "acquire",
                "Next, we acquire the source artifacts needed for this build.".to_string(),
            ),
            "build" | "dependency.build" => (
                "build",
                "With sources in place, the package will build now.".to_string(),
            ),
            "install" | "dependency.install" => (
                "install",
                "From here, the built output will install into its target location.".to_string(),
            ),
            "cleanup" => (
                "cleanup",
                "To leave things clean, we cleanup temporary files now.".to_string(),
            ),
            "remove" => (
                "remove",
                "As part of uninstall, this step will remove installed artifacts.".to_string(),
            ),
            h if h.contains("check.is_") => (
                "check",
                "At this point, we check the current state for remaining work.".to_string(),
            ),
            _ => (
                "run",
                "Everything is ready, so this step will run with current inputs.".to_string(),
            ),
        },
        "success" => (
            "complete",
            "This part is now complete, and everything here looks good.".to_string(),
        ),
        "failed" => (
            "fail",
            "A required operation reported an error, so this step must fail now.".to_string(),
        ),
        "skipped" | "satisfied" => (
            "skip",
            "Since the requirement is already satisfied, this step will skip now.".to_string(),
        ),
        "missing" => (
            "require",
            "Before we continue safely, this workflow does require that dependency.".to_string(),
        ),
        "required" => (
            "check",
            "To confirm what still needs attention, we check this again now.".to_string(),
        ),
        _ => (
            "process",
            "In the expected sequence, this operation will process now.".to_string(),
        ),
    };

    let enforced_base = enforce_verb_in_sentence(&base, verb);
    let styled_base = colorize_event_sentence(status, &enforced_base, verb);

    match passthrough_detail(details) {
        Some(extra) => format!("{styled_base} {}", extra.dimmed()),
        None => styled_base,
    }
}

fn passthrough_detail(details: &str) -> Option<&str> {
    let d = details.trim();
    if d.is_empty() { None } else { Some(d) }
}

fn enforce_verb_in_sentence(sentence: &str, verb: &str) -> String {
    if contains_word(sentence, verb) {
        sentence.to_string()
    } else {
        format!("For this step, we {verb}. {sentence}")
    }
}

fn contains_word(sentence: &str, word: &str) -> bool {
    sentence
        .split(|c: char| !c.is_ascii_alphabetic())
        .filter(|token| !token.is_empty())
        .any(|token| token.eq_ignore_ascii_case(word))
}

fn colorize_event_sentence(status: &str, sentence: &str, verb: &str) -> String {
    if let Some((start, end)) = find_word_span(sentence, verb) {
        let head = &sentence[..start];
        let token = &sentence[start..end];
        let tail = &sentence[end..];
        let gray_head = format!("{}", head.bright_black());
        let gray_tail = format!("{}", tail.bright_black());
        let bracketed = format!("[{token}]");
        let colored = colorize_by_status(status, &bracketed);
        format!("{gray_head}{colored}{gray_tail}")
    } else {
        format!("{}", sentence.bright_black())
    }
}

fn colorize_by_status(status: &str, token: &str) -> String {
    match status {
        "requested" => format!("{}", token.bright_blue()),
        "running" => format!("{}", token.cyan()),
        "success" => format!("{}", token.green()),
        "failed" => format!("{}", token.red()),
        "skipped" => format!("{}", token.yellow()),
        "satisfied" => format!("{}", token.bright_yellow()),
        "missing" => format!("{}", token.magenta()),
        "required" => format!("{}", token.bright_magenta()),
        _ => format!("{}", token.dimmed()),
    }
}

fn find_word_span(sentence: &str, word: &str) -> Option<(usize, usize)> {
    let lower_sentence = sentence.to_ascii_lowercase();
    let lower_word = word.to_ascii_lowercase();
    let mut search_from = 0usize;

    while let Some(pos) = lower_sentence[search_from..].find(&lower_word) {
        let start = search_from + pos;
        let end = start + lower_word.len();

        let prev_ok = if start == 0 {
            true
        } else {
            !sentence[..start]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_ascii_alphabetic())
        };
        let next_ok = if end >= sentence.len() {
            true
        } else {
            !sentence[end..]
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic())
        };

        if prev_ok && next_ok {
            return Some((start, end));
        }

        search_from = start + 1;
    }

    None
}

/// Print package status in list output.
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
