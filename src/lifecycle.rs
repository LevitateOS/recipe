//! The recipe lifecycle vocabulary: phases, their hook and check names, and the
//! cleanup reasons recipes receive. These names are recipe API (README,
//! `recipe-recipe.5`); the engine, the wire protocol, narration and autofix all
//! read them from here.

use serde::{Deserialize, Serialize};

/// A producing phase. Each has a hook `<name>(ctx)` and a readiness check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Acquire,
    Build,
    Install,
}

impl Phase {
    /// Execution order.
    pub const ALL: [Phase; 3] = [Phase::Acquire, Phase::Build, Phase::Install];

    /// The hook function name.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Acquire => "acquire",
            Self::Build => "build",
            Self::Install => "install",
        }
    }

    /// The readiness check that decides whether the phase runs and verifies it after.
    pub const fn check(self) -> &'static str {
        match self {
            Self::Acquire => "is_acquired",
            Self::Build => "is_built",
            Self::Install => "is_installed",
        }
    }
}

/// Every readiness check name, outermost (`is_installed`) first.
pub const CHECKS: [&str; 3] = [Phase::Install.check(), Phase::Build.check(), Phase::Acquire.check()];

/// The `reason` argument of `cleanup(ctx, reason)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CleanupReason {
    Succeeded(Phase),
    Failed(Phase),
    /// A user-requested cleanup without its own reason.
    Manual,
}

impl CleanupReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Succeeded(Phase::Acquire) => "auto.acquire.success",
            Self::Succeeded(Phase::Build) => "auto.build.success",
            Self::Succeeded(Phase::Install) => "auto.install.success",
            Self::Failed(Phase::Acquire) => "auto.acquire.failure",
            Self::Failed(Phase::Build) => "auto.build.failure",
            Self::Failed(Phase::Install) => "auto.install.failure",
            Self::Manual => "manual",
        }
    }
}
