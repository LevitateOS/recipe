//! Explicit execution inputs, distinct from recipe-defined package facts.
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionOptions {
    pub target_os: String,
    pub target_arch: String,
    pub jobs: u16,
    /// Optional restrictions on the authority declared by the recipe.
    pub narrowing: crate::needs::Narrowing,
    /// Emit lifecycle JSON events through the worker diagnostic channel.
    pub machine_events: bool,
    /// Host file the coordinator keeps current with the running recipe's
    /// `progress()` reports. Coordinator only: never sent to the worker.
    #[serde(skip)]
    pub progress_file: Option<std::path::PathBuf>,
    /// Maximum supervised execution time, separate from frame transport stalls.
    pub(crate) timeout_secs: u64,
    /// Domain memory and writable filesystem bounds.
    pub(crate) memory_mib: u64,
    pub(crate) storage_max_objects: u64,
    pub(crate) storage_max_bytes: Option<u64>,
    /// Explicit outbound TCP hosts; empty keeps networking disabled.
    pub(crate) network_hosts: Vec<String>,
    /// Cumulative transmitted and received network bytes per invocation.
    pub(crate) network_max_bytes: u64,
    /// Rerun the main recipe's acquisition, build and installation phases.
    pub force_acquire: bool,
    /// Rerun the main recipe's build and installation phases.
    pub force_build: bool,
    /// Rerun the main recipe's installation phase.
    pub force_install: bool,
    /// Name the worker's and the Ginux launcher's bytes in the saved-state
    /// identity, on top of the declared behaviour versions
    /// (`crate::identity`), so only byte-identical tools reuse a state.
    /// Release builds set it. Applies to dependencies too. Coordinator only:
    /// never sent to the worker.
    #[serde(skip)]
    pub exact_identity: bool,
    /// The folder the package will live in once activated
    /// (`PROFILE/.packages/NAME`, through the profile symlink of the store
    /// installed into), shown to the recipe as `RUNTIME.paths.prefix`. None
    /// with `--no-activate` and for dependencies, which are never activated.
    /// Part of the saved-state identity when set (`crate::retained`).
    #[serde(default)]
    pub package_prefix: Option<String>,
}
impl Default for ExecutionOptions {
    fn default() -> Self {
        Self {
            target_os: std::env::consts::OS.into(),
            target_arch: std::env::consts::ARCH.into(),
            jobs: 1,
            narrowing: crate::needs::Narrowing::default(),
            machine_events: false,
            progress_file: None,
            timeout_secs: 3600,
            memory_mib: 2048,
            storage_max_objects: 16384,
            storage_max_bytes: None,
            network_hosts: Vec::new(),
            network_max_bytes: 8 * 1024 * 1024 * 1024,
            force_acquire: false,
            force_build: false,
            force_install: false,
            exact_identity: false,
            package_prefix: None,
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
        if let Some(prefix) = &self.package_prefix
            && (!prefix.starts_with('/')
                || prefix.len() < 2
                || prefix.len() > 1024
                || prefix.ends_with('/')
                || prefix.contains("//")
                || prefix.split('/').any(|part| part == "." || part == "..")
                || prefix.contains(':')
                || prefix.chars().any(char::is_control))
        {
            bail!(
                "package prefix {prefix:?} must be a normal absolute folder of at most 1024 bytes without ':'"
            );
        }
        if !(1..=16).contains(&self.jobs) {
            bail!("jobs must be between 1 and 16");
        }
        if !(1..=crate::domain::MAX_TIMEOUT_SECS).contains(&self.timeout_secs) {
            bail!(
                "timeout must be between 1 and {} seconds",
                crate::domain::MAX_TIMEOUT_SECS
            );
        }
        if !(16..=1_048_576).contains(&self.memory_mib)
            || !(1..=1_000_000).contains(&self.storage_max_objects)
            || self
                .storage_max_bytes
                .is_some_and(|bytes| bytes == 0 || bytes > self.memory_mib * 1024 * 1024)
        {
            bail!("invalid domain memory or storage bounds");
        }
        if self.network_hosts.len() > crate::domain::MAX_NETWORK_ENDPOINTS
            || self.network_max_bytes == 0
        {
            bail!(
                "network policy requires at most {} hosts and a positive byte limit",
                crate::domain::MAX_NETWORK_ENDPOINTS
            );
        }
        Ok(())
    }
    /// Largest export archive: the stored bytes plus tar headers per object.
    pub(crate) fn archive_bound(&self) -> u64 {
        self.storage_byte_bound() + self.storage_max_objects * 3072 + 1024
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
            package_prefix: None,
            ..self.clone()
        }
    }
}
