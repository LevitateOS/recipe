//! Where human diagnostics go.
//!
//! Every human-readable line, child output chunk and progress update goes
//! through this module, so a process decides once where its output goes:
//!
//! - **The diagnostic stream** ([`write`], [`line`], [`diag!`]). The
//!   coordinator writes it to its stderr. The worker installs a [`Sink`] that
//!   frames it to the coordinator, which shows it and keeps it in `worker.log`.
//! - **Out-of-band stderr** ([`out_of_band`], [`trace`], [`progress_target`]).
//!   This is the process's own stderr, for progress redraws and worker traces
//!   that must not enter the diagnostic budget. In the worker it reaches only
//!   the supervisor log.
//!
//! This module depends on nothing else in the crate.

use std::io::Write;
use std::sync::OnceLock;

/// Origin of diagnostic bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    /// Recipe narration and helper messages.
    Recipe,
    /// A child process's stdout (stdout is reserved for the final JSON).
    Stdout,
    /// A child process's stderr.
    Stderr,
}

/// A diagnostic destination installed once per process.
pub type Sink = dyn Fn(Stream, &[u8]) -> std::io::Result<()> + Send + Sync;

static SINK: OnceLock<Box<Sink>> = OnceLock::new();

/// Route this process's diagnostic stream to `sink` (the worker does this once).
pub fn install(sink: Box<Sink>) -> anyhow::Result<()> {
    SINK.set(sink)
        .map_err(|_| anyhow::anyhow!("diagnostic sink already installed"))
}

/// Write bytes to the diagnostic stream.
pub fn write(stream: Stream, bytes: &[u8]) -> std::io::Result<()> {
    match SINK.get() {
        Some(sink) => sink(stream, bytes),
        None => std::io::stderr().write_all(bytes),
    }
}

/// Write one recipe line to the diagnostic stream. A failed write is not
/// reported here: the worker sink remembers transport failures itself and
/// refuses to report success.
pub fn line(message: &str) {
    let mut bytes = message.as_bytes().to_vec();
    bytes.push(b'\n');
    let _ = write(Stream::Recipe, &bytes);
}

/// `format!`-style [`line`].
macro_rules! diag {
    ($($args:tt)*) => { $crate::diagnostics::line(&format!($($args)*)) };
}
pub(crate) use diag;

/// Write raw bytes to this process's own stderr, outside the diagnostic stream.
pub fn out_of_band(bytes: &[u8]) {
    let mut stderr = std::io::stderr();
    let _ = stderr.write_all(bytes).and_then(|()| stderr.flush());
}

/// Write one line to this process's own stderr, outside the diagnostic stream.
pub fn trace(message: &str) {
    out_of_band(format!("{message}\n").as_bytes());
}

/// Draw target for progress bars: out-of-band stderr, hidden when not a terminal.
pub fn progress_target() -> indicatif::ProgressDrawTarget {
    indicatif::ProgressDrawTarget::stderr()
}
