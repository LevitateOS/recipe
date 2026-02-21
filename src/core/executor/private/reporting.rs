use crate::core::output;
use anyhow::Error;

pub(crate) fn friendly_reason(phase: &str, reason: &str, attempt: &rhai::Map) -> String {
    let mut snippet = String::new();
    if let Some(ctx_name) = attempt
        .get("name")
        .and_then(|v| v.clone().into_string().ok())
    {
        snippet.push_str(&format!("{ctx_name}: "));
    }
    snippet.push_str(&format!("{phase} check reported: work still needed"));
    if !reason.is_empty() {
        snippet.push_str(&format!(" ({reason})"));
    }
    snippet
}

pub(crate) fn report_phase_failure(name: &str, phase: &str, error: &Error) {
    output::hook_event(name, phase, "failed", &format!("{error}"));
    output::error(&format!("{name}: {phase} recipe step failed"));
    output::detail(&format!("  reason: {error}"));
    output::detail(
        "  action: check the corresponding recipe function, then rerun with RECIPE_TRACE_HELPERS=1 for helper-level traces.",
    );
    output::detail(
        "  action: if this fails on shell command output, reproduce that command manually and fix the underlying environment/network/path issue first.",
    );
}

pub(crate) fn report_phase_success(name: &str, phase: &str) {
    output::hook_event(name, phase, "success", "step finished");
    output::success(&format!("{name}: {phase} step finished"));
}

pub(crate) fn report_check_result(
    name: &str,
    check: &str,
    needs_phase: bool,
    reason: Option<&str>,
) {
    if needs_phase {
        if let Some(reason) = reason {
            output::detail(&format!(
                "{name}: {check} check says recipe still needs this step ({reason})"
            ));
            output::hook_event(name, &format!("check.{check}"), "required", reason);
        } else {
            output::detail(&format!(
                "{name}: {check} check says recipe still needs this step"
            ));
            output::hook_event(
                name,
                &format!("check.{check}"),
                "required",
                "check returned failure",
            );
        }
    } else {
        output::detail(&format!(
            "{name}: {check} check says recipe step is already complete"
        ));
        output::hook_event(
            name,
            &format!("check.{check}"),
            "satisfied",
            "check returned success",
        );
    }
}
