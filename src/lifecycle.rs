//! The step vocabulary (docs/recipe-format.md, section 6.1) and the cleanup
//! reasons `cleanup(reason)` receives (section 3.7). These names are recipe
//! API; the engine, the wire protocol and narration all read them from here.

use serde::{Deserialize, Serialize};

/// A step of a run that can fail on its own: the worker runs `download` and
/// `build`; `install` (activating or publishing) is the coordinator's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Download,
    Build,
    Install,
}

impl Phase {
    /// The step's name (`ABOUT.steps`, events, `<phase>` of a cleanup reason).
    pub const fn name(self) -> &'static str {
        match self {
            Self::Download => "download",
            Self::Build => "build",
            Self::Install => "install",
        }
    }
}

/// The `reason` argument of `cleanup(reason)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CleanupReason {
    /// The run succeeded; the step named is the last one that ran.
    Succeeded(Phase),
    /// The run failed in the step named.
    Failed(Phase),
    /// `recipe cleanup`.
    Manual,
}

impl CleanupReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Succeeded(Phase::Download) => "auto.download.success",
            Self::Succeeded(Phase::Build) => "auto.build.success",
            Self::Succeeded(Phase::Install) => "auto.install.success",
            Self::Failed(Phase::Download) => "auto.download.failure",
            Self::Failed(Phase::Build) => "auto.build.failure",
            Self::Failed(Phase::Install) => "auto.install.failure",
            Self::Manual => "manual",
        }
    }
}
