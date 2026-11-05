//! Explicit execution inputs, distinct from recipe-defined package facts.
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionOptions {
    pub target_os: String,
    pub target_arch: String,
    pub jobs: u16,
    /// Rerun the main recipe's acquisition, build and installation phases.
    pub force_acquire: bool,
    /// Rerun the main recipe's build and installation phases.
    pub force_build: bool,
    /// Rerun the main recipe's installation phase.
    pub force_install: bool,
}
impl Default for ExecutionOptions {
    fn default() -> Self {
        Self {
            target_os: std::env::consts::OS.into(),
            target_arch: std::env::consts::ARCH.into(),
            jobs: 1,
            force_acquire: false,
            force_build: false,
            force_install: false,
        }
    }
}
impl ExecutionOptions {
    pub fn validate(&self) -> Result<()> {
        if self.target_os != "linux"
            || self.target_arch != "x86_64"
            || self.target_os != std::env::consts::OS
            || self.target_arch != std::env::consts::ARCH
        {
            bail!(
                "unsupported target {}/{}: this Ginux worker supports native linux/x86_64",
                self.target_os,
                self.target_arch
            );
        }
        if !(1..=16).contains(&self.jobs) {
            bail!("jobs must be between 1 and 16");
        }
        Ok(())
    }
    pub(crate) fn forced(&self) -> bool {
        self.force_acquire || self.force_build || self.force_install
    }
    pub(crate) fn for_dependency(&self) -> Self {
        Self {
            force_acquire: false,
            force_build: false,
            force_install: false,
            ..self.clone()
        }
    }
}
