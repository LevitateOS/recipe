//! Shared logging contract for Recipe hook events.
//!
//! This module provides stable, crate-level hooks so other crates can emit the same
//! human + machine event contract used by the Recipe CLI.

use crate::core::events;

/// Machine event envelope name used by all Recipe hook events.
pub const RECIPE_HOOK_EVENT: &str = events::RECIPE_HOOK_EVENT;

/// Canonical machine-hook event payload.
pub type RecipeHookEvent = events::RecipeHookEvent;

pub use crate::core::output::{RecipeHookSink, set_event_sink, set_event_sink_handler};
pub use events::make_machine_hook_event;

/// Emit a hook event using Recipe's default formatter and hook contract.
pub fn emit_hook_event(recipe: &str, hook: &str, status: &str, msg: &str) {
    crate::core::output::hook_event(recipe, hook, status, msg);
}

/// Emit a typed hook event using Recipe's default formatter and hook contract.
pub fn emit_hook_event_struct(event: &RecipeHookEvent) {
    crate::core::output::hook_event_struct(event);
}

/// Toggle machine-events output globally for Recipe logging.
pub fn set_machine_events(enabled: bool) {
    crate::core::output::set_machine_events(enabled);
}

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
        let directory = logs.join(invocation);
        std::fs::DirBuilder::new().mode(0o700).create(&directory)?;
        std::fs::File::open(&logs)?.sync_all()?;
        let log = Self { directory };
        log.record(&serde_json::json!({"invocation": invocation, "status":"running"}))?;
        eprintln!("recipe: diagnostics: {}", log.directory.display());
        Ok(log)
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
