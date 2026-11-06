//! Diagnostic sink selection and the durable per-invocation diagnostics store.

// Worker installation is explicit and never selected through environment flags.
// The shared callback serializes diagnostics with lifecycle protocol frames.
type DiagnosticSink =
    dyn Fn(crate::worker::protocol::DiagnosticStream, &[u8]) -> std::io::Result<()> + Send + Sync;
static DIAGNOSTIC_SINK: std::sync::OnceLock<Box<DiagnosticSink>> = std::sync::OnceLock::new();

pub(crate) fn install_diagnostic_sink(sink: Box<DiagnosticSink>) -> anyhow::Result<()> {
    DIAGNOSTIC_SINK
        .set(sink)
        .map_err(|_| anyhow::anyhow!("worker diagnostic sink already installed"))
}

pub(crate) fn diagnostic(
    stream: crate::worker::protocol::DiagnosticStream,
    bytes: &[u8],
) -> std::io::Result<()> {
    use std::io::Write;
    match DIAGNOSTIC_SINK.get() {
        Some(sink) => sink(stream, bytes),
        None => std::io::stderr().write_all(bytes),
    }
}

pub(crate) fn diagnostic_line(message: &str) {
    let mut bytes = message.as_bytes().to_vec();
    bytes.push(b'\n');
    let _ = diagnostic(crate::worker::protocol::DiagnosticStream::Recipe, &bytes);
}

macro_rules! diag {
    ($($args:tt)*) => { $crate::logging::diagnostic_line(&format!($($args)*)) };
}
pub(crate) use diag;

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
        match std::fs::DirBuilder::new().mode(0o700).create(&logs) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.into()),
        }
        let metadata = std::fs::symlink_metadata(&logs)?;
        anyhow::ensure!(
            metadata.is_dir() && metadata.permissions().mode() & 0o077 == 0,
            "diagnostics directory must be a private real directory"
        );
        use fs2::FileExt;
        let pool = log_lease(&logs.join("prune.lock"))?;
        pool.lock_exclusive()?;
        prune_logs(&logs)?;
        let directory = logs.join(invocation);
        std::fs::DirBuilder::new().mode(0o700).create(&directory)?;
        std::fs::File::open(&logs)?.sync_all()?;
        let lease = log_lease(&directory.join("lease"))?;
        lease.lock_exclusive()?;
        let log = Self { directory, lease };
        log.record(&serde_json::json!({"invocation": invocation, "status":"running"}))?;
        eprintln!("recipe: diagnostics: {}", log.directory.display());
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

fn log_lease(path: &std::path::Path) -> anyhow::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    anyhow::ensure!(
        file.metadata()?.is_file(),
        "diagnostic lease is not regular"
    );
    Ok(file)
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
        if crate::inputs::validate_invocation_id(name).is_err() {
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
        let lease = log_lease(&path.join("lease"))?;
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
