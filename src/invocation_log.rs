//! The durable per-invocation diagnostics store: `<state>/logs/<invocation>/`.
//!
//! This module owns the directory layout, the `result.json` status vocabulary,
//! retention and reading a finished invocation back, so nothing else needs to
//! know the file names or their bounds.

use anyhow::Result;
use std::path::{Path, PathBuf};

/// Directory under the state root holding one directory per invocation.
const LOGS: &str = "logs";
const LEASE: &str = "lease";
const RESULT: &str = "result.json";
/// Prefix of the temporary file `result.json` is written through.
const RESULT_STAGING_PREFIX: &str = ".result.json.";
/// Worker diagnostic stream, as the coordinator received it.
pub(crate) const WORKER_LOG: &str = "worker.log";
/// The launcher's own stderr, which carries Ginux's audit and summary lines.
pub(crate) const SUPERVISOR_LOG: &str = "supervisor-stderr.log";
/// The declaration, effective constraints and contract the invocation ran under.
const AUTHORITY: &str = "authority.json";
/// Format version of [`AUTHORITY`].
const AUTHORITY_VERSION: u64 = 1;
/// Files an invocation directory may contain.
const FILES: [&str; 5] = [LEASE, RESULT, WORKER_LOG, SUPERVISOR_LOG, AUTHORITY];
/// Inactive invocation directories kept, including the one being created.
const RETAINED_INVOCATIONS: usize = 32;
/// Largest `result.json` or `authority.json`.
const MAX_RESULT_BYTES: u64 = 4 * 1024 * 1024;
/// Largest retained [`SUPERVISOR_LOG`]; the launcher's stderr beyond it is
/// drained but not kept.
pub(crate) const MAX_LOG_BYTES: u64 = 16 * 1024 * 1024;

/// `result.json` status of an invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Status {
    /// Started; the lease tells a live invocation from a crashed one.
    Running,
    Succeeded,
    WorkloadFailed,
    Cancelled,
    Failed,
}

impl Status {
    fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::WorkloadFailed => "workload-failed",
            Self::Cancelled => "cancelled",
            Self::Failed => "failed",
        }
    }
}

/// One invocation's diagnostics directory, leased while the invocation runs.
pub(crate) struct InvocationLog {
    directory: PathBuf,
    invocation: String,
    lease: std::fs::File,
}

impl InvocationLog {
    pub fn new(state_root: &Path, invocation: &str) -> Result<Self> {
        use fs2::FileExt;
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        std::fs::create_dir_all(state_root)?;
        let logs = state_root.join(LOGS);
        crate::fsx::private_dir(&logs, false)?;
        anyhow::ensure!(
            std::fs::symlink_metadata(&logs)?.permissions().mode() & 0o077 == 0,
            "diagnostics directory must be a private real directory"
        );
        let pool = crate::fsx::open_lease(&logs.join("prune.lock"), true)?;
        pool.lock_exclusive()?;
        // Removing old logs is housekeeping; it never fails this invocation.
        if let Err(error) = prune(&logs) {
            crate::diagnostics::diag!("recipe: warning: cannot prune old diagnostics: {error:#}");
        }
        let directory = logs.join(invocation);
        std::fs::DirBuilder::new().mode(0o700).create(&directory)?;
        std::fs::File::open(&logs)?.sync_all()?;
        let lease = crate::fsx::open_lease(&directory.join(LEASE), true)?;
        lease.lock_exclusive()?;
        let log = Self {
            directory,
            invocation: invocation.to_owned(),
            lease,
        };
        log.record(Status::Running, serde_json::json!({}))?;
        crate::diagnostics::diag!("recipe: diagnostics: {}", log.directory.display());
        Ok(log)
    }

    /// Path of one of this invocation's log files ([`WORKER_LOG`], [`SUPERVISOR_LOG`]).
    pub(crate) fn file(&self, name: &str) -> PathBuf {
        debug_assert!(FILES.contains(&name));
        self.directory.join(name)
    }

    /// Save the exact declaration, effective constraints and contract used by
    /// this instance. It survives failure and never re-reads mutable source.
    pub(crate) fn authority(&self, prepared: &crate::inputs::PreparedInvocation) -> Result<()> {
        use std::io::Write;
        let record = serde_json::json!({"version":AUTHORITY_VERSION,"declaration":prepared.declaration,
            "execution":prepared.execution,"contract":prepared.contract,
            "config":std::fs::read_to_string(prepared.config_path())?,
            "pinned_endpoints":prepared.network.endpoints, "host_endpoints":prepared.network.host_endpoints});
        let bytes = serde_json::to_vec(&record)?;
        anyhow::ensure!(
            bytes.len() <= MAX_RESULT_BYTES as usize,
            "authority receipt exceeds bound"
        );
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(self.file(AUTHORITY))?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        std::fs::File::open(&self.directory)?.sync_all()?;
        Ok(())
    }

    pub(crate) fn lease_fd(&self) -> std::os::fd::RawFd {
        use std::os::fd::AsRawFd;
        self.lease.as_raw_fd()
    }

    /// The retained launcher stderr of this invocation.
    pub(crate) fn supervisor_log(&self) -> Result<Vec<u8>> {
        read_bounded(&self.file(SUPERVISOR_LOG), MAX_LOG_BYTES)
    }

    /// Atomically replace `result.json` with `status` plus `details` fields.
    pub fn record(&self, status: Status, details: serde_json::Value) -> Result<()> {
        use std::io::Write;
        let mut record =
            serde_json::json!({"invocation": self.invocation, "status": status.as_str()});
        if let (Some(record), serde_json::Value::Object(details)) =
            (record.as_object_mut(), details)
        {
            record.extend(details);
        }
        let mut file = tempfile::Builder::new()
            .prefix(RESULT_STAGING_PREFIX)
            .tempfile_in(&self.directory)?;
        serde_json::to_writer(&mut file, &record)?;
        file.write_all(b"\n")?;
        file.as_file().sync_all()?;
        file.persist(self.directory.join(RESULT))?;
        std::fs::File::open(&self.directory)?.sync_all()?;
        Ok(())
    }
}

/// The diagnostics directory of invocation `id` under `state_dir` (or the
/// per-user application state), if one is retained.
pub fn retained(state_dir: Option<&Path>, id: &Path) -> Option<PathBuf> {
    let root = crate::retained::state_root(state_dir).ok()?;
    Some(root.join(LOGS).join(id)).filter(|directory| directory.is_dir())
}

/// What an invocation recorded, read back from its diagnostics directory.
pub(crate) struct Recorded {
    /// [`InvocationLog::authority`]'s receipt.
    pub authority: serde_json::Value,
    /// The last [`InvocationLog::record`].
    pub result: serde_json::Value,
    /// The retained launcher stderr.
    pub supervisor_log: Vec<u8>,
}

impl Recorded {
    pub(crate) fn read(directory: &Path) -> Result<Self> {
        anyhow::ensure!(
            std::fs::symlink_metadata(directory)?.is_dir(),
            "invocation must be a real diagnostics directory"
        );
        let json = |name: &str| -> Result<serde_json::Value> {
            Ok(serde_json::from_slice(&read_bounded(
                &directory.join(name),
                MAX_RESULT_BYTES,
            )?)?)
        };
        let authority = json(AUTHORITY)?;
        anyhow::ensure!(
            authority["version"] == AUTHORITY_VERSION,
            "unsupported authority receipt"
        );
        Ok(Self {
            authority,
            supervisor_log: read_bounded(&directory.join(SUPERVISOR_LOG), MAX_LOG_BYTES)?,
            result: json(RESULT)?,
        })
    }
}

fn read_bounded(path: &Path, bound: u64) -> Result<Vec<u8>> {
    use std::io::Read;
    let file = crate::fsx::open_regular(path)?;
    anyhow::ensure!(
        file.metadata()?.len() <= bound,
        "invocation diagnostic exceeds bound"
    );
    let mut bytes = Vec::new();
    file.take(bound + 1).read_to_end(&mut bytes)?;
    anyhow::ensure!(
        bytes.len() as u64 <= bound,
        "invocation diagnostic grew past bound"
    );
    Ok(bytes)
}

/// Remove the oldest unleased invocation directories beyond retention.
/// A directory is ours when its name is an invocation id, it holds a lease and
/// nothing but our files (or a staging leftover from a crash).
fn prune(root: &Path) -> Result<()> {
    use fs2::FileExt;
    use std::io::Read;
    let mut inactive = Vec::new();
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !crate::domain::valid_name(name) {
            continue;
        }
        let path = entry.path();
        let files = std::fs::read_dir(&path)?.collect::<std::io::Result<Vec<_>>>()?;
        let ours = |file: &std::fs::DirEntry| {
            let name = file.file_name();
            let name = name.to_string_lossy();
            (FILES.contains(&&*name) || name.starts_with(RESULT_STAGING_PREFIX))
                && file.file_type().is_ok_and(|kind| kind.is_file())
        };
        // Never manufacture ownership markers in foreign directories.
        if !files.iter().all(ours) || !files.iter().any(|file| file.file_name() == LEASE) {
            continue;
        }
        let lease = crate::fsx::open_lease(&path.join(LEASE), false)?;
        match lease.try_lock_exclusive() {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
            Err(e) => return Err(e.into()),
        }
        // A crash can leave the record missing or partial; such a directory
        // is still ours (named, leased, our files only) and ages by its mtime.
        let recorded = std::fs::File::open(path.join(RESULT))
            .ok()
            .and_then(|file| {
                let mut bytes = Vec::new();
                file.take(MAX_RESULT_BYTES + 1)
                    .read_to_end(&mut bytes)
                    .ok()?;
                let record: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
                (record["invocation"].as_str() == Some(name)).then_some(())
            });
        let aged = if recorded.is_some() {
            std::fs::metadata(path.join(RESULT))?.modified()?
        } else {
            std::fs::metadata(&path)?.modified()?
        };
        inactive.push((aged, path, lease, files));
    }
    inactive.sort_by_key(|(time, _, _, _)| std::cmp::Reverse(*time));
    // One slot is for the invocation being created.
    for (_, path, _lease, files) in inactive.into_iter().skip(RETAINED_INVOCATIONS - 1) {
        for file in files {
            std::fs::remove_file(file.path())?;
        }
        std::fs::remove_dir(path)?;
    }
    std::fs::File::open(root)?.sync_all()?;
    Ok(())
}
