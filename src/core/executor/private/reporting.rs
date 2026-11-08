use crate::core::events::HookStatus;
use crate::core::output;
use anyhow::Error;

pub(crate) fn report_phase_failure(name: &str, phase: &str, error: &Error) {
    output::hook_event(name, phase, HookStatus::Failed, &format!("{error}"));
    output::error(&format!("{name}: {phase} recipe step failed"));
    output::detail(&format!("  reason: {error}"));
    output::detail("  action: check the corresponding recipe function.");
    output::detail(
        "  action: if this fails on shell command output, reproduce that command manually and fix the underlying environment/network/path issue first.",
    );
}

pub(crate) fn report_phase_success(name: &str, phase: &str) {
    output::hook_event(name, phase, HookStatus::Success, "step finished");
    output::success(&format!("{name}: {phase} step finished"));
}

/// Report a check's result, and whether a force option overrides it.
pub(crate) fn report_check(
    name: &str,
    check: &str,
    readiness: &crate::core::invocation::Readiness,
    forced: bool,
) {
    use crate::core::invocation::Readiness;
    let event = format!("check.{check}");
    match readiness {
        Readiness::NeedsWork(_, reason) => {
            output::detail(&format!(
                "{name}: {check} check says recipe still needs this step ({reason})"
            ));
            output::hook_event(name, &event, HookStatus::Required, reason);
        }
        Readiness::Ready(_) if forced => {
            output::detail(&format!(
                "{name}: {check} check says recipe step is already complete; forced to run again"
            ));
            output::hook_event(name, &event, HookStatus::Required, "forced by option");
        }
        Readiness::Ready(_) => {
            output::detail(&format!(
                "{name}: {check} check says recipe step is already complete"
            ));
            output::hook_event(
                name,
                &event,
                HookStatus::Satisfied,
                "check returned success",
            );
        }
    }
}
