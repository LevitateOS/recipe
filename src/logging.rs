//! The durable per-invocation diagnostics store.

/// Durable bounded diagnostics live outside disposable invocation scratch.
pub(crate) struct InvocationLog {
    pub directory: std::path::PathBuf,
    lease: std::fs::File,
}
impl InvocationLog {
    pub fn new(state: Option<&std::path::Path>, invocation: &str) -> anyhow::Result<Self> {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        let root = match state {
            Some(path) => path.to_owned(),
            None => crate::retained::default_directory()?,
        };
        std::fs::create_dir_all(&root)?;
        let logs = root.join("logs");
        crate::fsx::private_dir(&logs, false)?;
        anyhow::ensure!(
            std::fs::symlink_metadata(&logs)?.permissions().mode() & 0o077 == 0,
            "diagnostics directory must be a private real directory"
        );
        use fs2::FileExt;
        let pool = crate::fsx::open_lease(&logs.join("prune.lock"), true)?;
        pool.lock_exclusive()?;
        prune_logs(&logs)?;
        let directory = logs.join(invocation);
        std::fs::DirBuilder::new().mode(0o700).create(&directory)?;
        std::fs::File::open(&logs)?.sync_all()?;
        let lease = crate::fsx::open_lease(&directory.join("lease"), true)?;
        lease.lock_exclusive()?;
        let log = Self { directory, lease };
        log.record(&serde_json::json!({"invocation": invocation, "status":"running"}))?;
        crate::diagnostics::diag!("recipe: diagnostics: {}", log.directory.display());
        Ok(log)
    }
    pub(crate) fn lease_fd(&self) -> std::os::fd::RawFd {
        use std::os::fd::AsRawFd;
        self.lease.as_raw_fd()
    }
    pub fn record(&self, value: &serde_json::Value) -> anyhow::Result<()> {
        use std::io::Write;
        let mut file = tempfile::NamedTempFile::new_in(&self.directory)?;
        serde_json::to_writer(&mut file, value)?;
        file.write_all(b"\n")?;
        file.as_file().sync_all()?;
        file.persist(self.directory.join("result.json"))?;
        std::fs::File::open(&self.directory)?.sync_all()?;
        Ok(())
    }
}

fn prune_logs(root: &std::path::Path) -> anyhow::Result<()> {
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
        if !crate::worker::protocol::valid_invocation_id(name) {
            continue;
        }
        let path = entry.path();
        let files = std::fs::read_dir(&path)?.collect::<std::io::Result<Vec<_>>>()?;
        if files.iter().any(|file| {
            !matches!(
                file.file_name().to_str(),
                Some("lease" | "result.json" | "supervisor-stderr.log" | "worker.log")
            ) || !file.file_type().is_ok_and(|kind| kind.is_file())
        }) {
            continue;
        }
        // Do not manufacture ownership markers in foreign directories.
        if !files.iter().any(|file| file.file_name() == "lease") {
            continue;
        }
        let lease = crate::fsx::open_lease(&path.join("lease"), true)?;
        match lease.try_lock_exclusive() {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
            Err(e) => return Err(e.into()),
        }
        let record = match std::fs::File::open(path.join("result.json")) {
            Ok(file) => file,
            Err(_) => continue,
        };
        let mut bytes = Vec::new();
        record.take(4 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
        if bytes.len() > 4 * 1024 * 1024 {
            continue;
        }
        let Ok(record) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            continue;
        };
        if record["invocation"].as_str() != Some(name) {
            continue;
        }
        inactive.push((
            std::fs::metadata(path.join("result.json"))?.modified()?,
            path,
            lease,
            files,
        ));
    }
    inactive.sort_by_key(|(time, _, _, _)| std::cmp::Reverse(*time));
    // Reserve a slot for the invocation being created; active leases never
    // count against this inactive history bound.
    for (_, path, _lease, files) in inactive.into_iter().skip(31) {
        for file in files {
            std::fs::remove_file(file.path())?;
        }
        std::fs::remove_dir(path)?;
    }
    std::fs::File::open(root)?.sync_all()?;
    Ok(())
}
