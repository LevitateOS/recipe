use crate::core::events::StepStatus;
use crate::core::output;
use anyhow::Error;

pub(crate) fn report_phase_failure(name: &str, phase: &str, error: &Error) {
    output::step_event(name, phase, StepStatus::Failed, &format!("{error}"));
    output::error(&format!("{name}: {phase} recipe step failed"));
    output::detail(&format!("  reason: {error}"));
    output::detail("  action: check the corresponding recipe function.");
    output::detail(
        "  action: if this fails on shell command output, reproduce that command manually and fix the underlying environment/network/path issue first.",
    );
}

pub(crate) fn report_phase_success(name: &str, phase: &str) {
    output::step_event(name, phase, StepStatus::Done, "step finished");
    output::success(&format!("{name}: {phase} step finished"));
}
