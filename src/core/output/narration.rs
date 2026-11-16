//! Human-readable output rendering for recipe operations.

use owo_colors::OwoColorize;

use crate::core::events::StepStatus;

/// Print an action header (bold)
pub fn action(message: &str) {
    crate::diagnostics::diag!("{}", message.bold());
}

/// Print a sub-action (cyan)
pub fn sub_action(phase: &str) {
    crate::diagnostics::diag!(
        "{}",
        format!("Now working through the {phase} step.").cyan()
    );
}

/// Print a detail line (dimmed)
pub fn detail(message: &str) {
    crate::diagnostics::diag!("{}", message.dimmed());
}

/// Print a success message (green)
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
pub fn skip(message: &str) {
    crate::diagnostics::diag!("{}", message.dimmed());
}

pub(crate) fn sentence_for_event(step: &str, status: StepStatus, details: &str) -> String {
    let (verb, base) = match status {
        StepStatus::Running => match step {
            "download" => (
                "download",
                "Next, we download the sources needed for this build.".to_string(),
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
                "To leave things clean, we cleanup what is no longer needed now.".to_string(),
            ),
            _ => (
                "run",
                "Everything is ready, so this step will run with current inputs.".to_string(),
            ),
        },
        StepStatus::Done => (
            "done",
            format!("The {step} step is done, and everything here looks good."),
        ),
        StepStatus::Already => (
            "already",
            format!("The {step} step was done already, so nothing runs for it."),
        ),
        StepStatus::None => (
            "none",
            format!("This recipe has no {step} step: none to do."),
        ),
        StepStatus::Skipped => (
            "skipped",
            format!("The {step} step is not part of this command, so it is skipped."),
        ),
        StepStatus::Failed => (
            "failed",
            format!("A required operation reported an error, so the {step} step failed."),
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

fn colorize_event_sentence(status: StepStatus, sentence: &str, verb: &str) -> String {
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

fn colorize_by_status(status: StepStatus, token: &str) -> String {
    match status {
        StepStatus::Running => format!("{}", token.cyan()),
        StepStatus::Done | StepStatus::Already => format!("{}", token.green()),
        StepStatus::Failed => format!("{}", token.red()),
        StepStatus::None | StepStatus::Skipped => format!("{}", token.yellow()),
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
