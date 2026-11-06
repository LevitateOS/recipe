//! Human-readable output rendering for recipe operations.

use owo_colors::OwoColorize;

use crate::core::events::HookStatus;

/// Print an action header (blue, bold)
/// Example: "==> Installing ripgrep"
pub fn action(message: &str) {
    crate::diagnostics::diag!("{}", message.bold());
}

/// Print a sub-action (cyan arrow)
/// Example: "  -> acquire"
pub fn sub_action(phase: &str) {
    crate::diagnostics::diag!(
        "{}",
        format!("Now working through the {phase} step.").cyan()
    );
}

/// Print a detail line (dimmed prefix)
/// Example: "     downloading https://..."
pub fn detail(message: &str) {
    crate::diagnostics::diag!("{}", message.dimmed());
}

/// Print a success message (green)
/// Example: "==> ripgrep installed"
pub fn success(message: &str) {
    crate::diagnostics::diag!("{}", message.green());
}

/// Print an info message (cyan)
pub fn info(message: &str) {
    crate::diagnostics::diag!("{message}");
}

/// Print a warning message (yellow)
pub fn warning(message: &str) {
    crate::diagnostics::diag!("{}", message.yellow());
}

/// Print an error message (red)
pub fn error(message: &str) {
    crate::diagnostics::diag!("{}", message.red());
}

/// Print a skip message (dimmed)
/// Example: "==> ripgrep already installed, skipping"
pub fn skip(message: &str) {
    crate::diagnostics::diag!("{}", message.dimmed());
}

pub(crate) fn sentence_for_event(hook: &str, status: HookStatus, details: &str) -> String {
    let (verb, base) = match status {
        HookStatus::Requested => (
            "queue",
            "To keep the flow in order, this step will queue now.".to_string(),
        ),
        HookStatus::Running => match hook {
            "prepare" => (
                "prepare",
                "Before anything else moves forward, we prepare the environment now.".to_string(),
            ),
            "acquire" => (
                "acquire",
                "Next, we acquire the source artifacts needed for this build.".to_string(),
            ),
            "build" => (
                "build",
                "With sources in place, the package will build now.".to_string(),
            ),
            "install" => (
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
            _ => (
                "run",
                "Everything is ready, so this step will run with current inputs.".to_string(),
            ),
        },
        HookStatus::Success => (
            "complete",
            "This part is now complete, and everything here looks good.".to_string(),
        ),
        HookStatus::Failed => (
            "fail",
            "A required operation reported an error, so this step must fail now.".to_string(),
        ),
        HookStatus::Skipped | HookStatus::Satisfied => (
            "skip",
            "Since the requirement is already satisfied, this step will skip now.".to_string(),
        ),
        HookStatus::Missing => (
            "require",
            "Before we continue safely, this workflow does require that dependency.".to_string(),
        ),
        HookStatus::Required => (
            "check",
            "To confirm what still needs attention, we check this again now.".to_string(),
        ),
        HookStatus::Manual => (
            "process",
            "In the expected sequence, this operation will process now.".to_string(),
        ),
    };

    let styled_base = colorize_event_sentence(status, &base, verb);

    match passthrough_detail(details) {
        Some(extra) => format!("{styled_base} {}", extra.dimmed()),
        None => styled_base,
    }
}

fn passthrough_detail(details: &str) -> Option<&str> {
    let d = details.trim();
    if d.is_empty() { None } else { Some(d) }
}

fn colorize_event_sentence(status: HookStatus, sentence: &str, verb: &str) -> String {
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

fn colorize_by_status(status: HookStatus, token: &str) -> String {
    match status {
        HookStatus::Requested => format!("{}", token.bright_blue()),
        HookStatus::Running => format!("{}", token.cyan()),
        HookStatus::Success => format!("{}", token.green()),
        HookStatus::Failed => format!("{}", token.red()),
        HookStatus::Skipped => format!("{}", token.yellow()),
        HookStatus::Satisfied => format!("{}", token.bright_yellow()),
        HookStatus::Missing => format!("{}", token.magenta()),
        HookStatus::Required => format!("{}", token.bright_magenta()),
        HookStatus::Manual => format!("{}", token.dimmed()),
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
