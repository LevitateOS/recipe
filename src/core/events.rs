//! Step events: the machine-readable record of a run's steps behind
//! `--machine-events` (docs/recipe-format.md, section 8.7), and the status
//! of each step in a finished run's `steps` (section 8.6).

use serde::{Deserialize, Serialize};

/// The status of one step. This is the whole set a `step` event or a
/// finished run's `steps` carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    /// The step has started.
    Running,
    /// The step ran in this run and finished.
    Done,
    /// The step was done before, by Recipe's own records; nothing ran.
    Already,
    /// The recipe has no such step.
    None,
    /// The recipe has the step, but this command does not do it.
    Skipped,
    /// The step failed.
    Failed,
}

impl StepStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Done => "done",
            Self::Already => "already",
            Self::None => "none",
            Self::Skipped => "skipped",
            Self::Failed => "failed",
        }
    }
}

/// One step of a finished run: `{"step": "download", "status": "done"}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StepReport {
    pub step: String,
    pub status: StepStatus,
}

impl StepReport {
    pub fn new(step: &str, status: StepStatus) -> Self {
        Self {
            step: step.to_owned(),
            status,
        }
    }
}

/// The `step` event: `{"event":"step","recipe":..,"step":..,"status":..,"msg":..}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StepEvent {
    pub event: &'static str,
    pub recipe: String,
    pub step: String,
    pub status: &'static str,
    pub msg: String,
}

impl StepEvent {
    pub fn new(recipe: &str, step: &str, status: StepStatus, msg: &str) -> Self {
        Self {
            event: "step",
            recipe: recipe.to_string(),
            step: step.to_string(),
            status: status.as_str(),
            msg: msg.to_string(),
        }
    }

    /// Serialize as one JSON line for machine streams.
    pub fn as_json(&self) -> String {
        serde_json::to_string(self).expect("step event fields always serialize")
    }
}
