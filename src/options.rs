//! Explicit execution inputs, distinct from recipe-defined package facts.
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionOptions {
    pub target_os: String,
    pub target_arch: String,
    pub jobs: u16,
    /// Emit lifecycle JSON events through the worker diagnostic channel.
    pub machine_events: bool,
    /// Maximum supervised execution time, separate from frame transport stalls.
    pub timeout_secs: u64,
    /// Domain memory and writable filesystem bounds.
    pub memory_mib: u64,
    pub storage_max_objects: u64,
    pub storage_max_bytes: Option<u64>,
    /// Explicit outbound TCP hosts; empty keeps networking disabled.
    pub network_hosts: Vec<String>,
    /// Cumulative transmitted and received network bytes per invocation.
    pub network_max_bytes: u64,
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
            machine_events: false,
            timeout_secs: 3600,
            memory_mib: 2048,
            storage_max_objects: 16384,
            storage_max_bytes: None,
            network_hosts: Vec::new(),
            network_max_bytes: 8 * 1024 * 1024 * 1024,
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
        if !(1..=86400).contains(&self.timeout_secs) {
            bail!("timeout must be between 1 and 86400 seconds");
        }
        if !(16..=1_048_576).contains(&self.memory_mib)
            || !(1..=1_000_000).contains(&self.storage_max_objects)
            || self
                .storage_max_bytes
                .is_some_and(|bytes| bytes == 0 || bytes > self.memory_mib * 1024 * 1024)
        {
            bail!("invalid domain memory or storage bounds");
        }
        if self.network_hosts.len() > 8 || self.network_max_bytes == 0 {
            bail!("network policy requires at most eight hosts and a positive byte limit");
        }
        Ok(())
    }
    pub(crate) fn archive_bound(&self) -> u64 {
        self.storage_max_bytes
            .unwrap_or(self.memory_mib * 1024 * 1024)
            + self.storage_max_objects * 3072
            + 1024
    }
    pub(crate) fn storage_byte_bound(&self) -> u64 {
        self.storage_max_bytes
            .unwrap_or(self.memory_mib * 1024 * 1024)
    }
    pub(crate) fn forced(&self) -> bool {
        self.force_acquire || self.force_build || self.force_install
    }
    /// Forcing a phase also forces every later phase: acquire ⇒ build ⇒ install.
    pub(crate) fn forces(&self, phase: crate::lifecycle::Phase) -> bool {
        use crate::lifecycle::Phase;
        match phase {
            Phase::Acquire => self.force_acquire,
            Phase::Build => self.force_acquire || self.force_build,
            Phase::Install => self.forced(),
        }
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
