//! The coordinator's side of a running worker, shared by both runtimes (the
//! Ginux instance, [`super::lifecycle`], and the host run, `crate::host`):
//! reading its line records, draining standard error to a capped log, reaping
//! it, and reading its result file afterwards ([`crate::worker::records`]).
//!
//! - Lines: one [`LineRecord`] per line, relayed live to `diagnostics.rs`
//!   and `worker.log` (diagnostics) or the `--progress-file` (progress). A
//!   line that is not a record is kept as plain log text (bounded like the
//!   diagnostics, never an error). A diagnostic record that breaks the
//!   budget rules is a `protocol` failure, as it always was.
//! - Where the lines come from ([`LineSource`]): the host worker's standard
//!   output pipe ([`DeadlinePipe`]), or the standard output of `ginux start
//!   --wait --stream-console` ([`ConsoleStream`]), which carries the Ginux
//!   instance's console: the worker's standard output through a
//!   pseudo-terminal (each line ends `\r\n`; the runtime's own `GINUX_` lines
//!   precede the worker's).
//! - Stall deadline: before the first line the worker has the output timeout
//!   (`RECIPE_OUTPUT_TIMEOUT_SECS`, 300 s by default) to say something; once
//!   it has, a silent phase may run until the separate execution deadline,
//!   but a line, once its first byte arrived, must be complete within the
//!   output timeout.
//! - The stream ends at end of file (the host worker, or the `ginux start
//!   --wait` process, exited). Only then is the result file read: from the
//!   extracted archive of a Ginux run ([`result_from_archive`]), from the run
//!   directory of a host run ([`result_from_tree`]). Either is untrusted:
//!   bounded, strictly parsed, and it must name this invocation. No valid
//!   result is a [`missing_result`] `protocol` failure, which the runtime
//!   enriches with the run's outcome.

use std::io::{Read, Write};
use std::os::fd::{AsFd, AsRawFd};
use std::path::{Path, PathBuf};
use std::process::Child;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use crate::worker::records::{LineRecord, MAX_DIAGNOSTIC_BYTES, MAX_LINE_BYTES, WorkerResult};

/// Startup and complete-line bound (see the module docs).
const OUTPUT_TIMEOUT: Duration = Duration::from_secs(300);

/// The environment variable that overrides [`OUTPUT_TIMEOUT`].
pub(crate) const OUTPUT_TIMEOUT_ENV: &str = "RECIPE_OUTPUT_TIMEOUT_SECS";

/// Bound on log collection: worker/supervisor log beyond this many bytes is
/// still drained (so a chatty worker never blocks on a full pipe) but not
/// retained.
const LOG_CAP_BYTES: u64 = crate::invocation_log::MAX_LOG_BYTES;

/// Grace for teardown after failure/cancellation. Successful final export uses
/// the remaining supervised execution deadline instead.
pub(crate) const SHUTDOWN_GRACE: Duration = Duration::from_secs(10);

/// How often the line pump looks whether the host worker (or `ginux start
/// --wait`) has exited while it waits for output.
const EXIT_CHECK: Duration = Duration::from_millis(250);

/// Bytes of log tail attached to failure diagnostics.
const LOG_TAIL_BYTES: usize = 4096;

/// The output timeout: [`OUTPUT_TIMEOUT`], or [`OUTPUT_TIMEOUT_ENV`].
pub(crate) fn output_timeout() -> Result<Duration> {
    match std::env::var(OUTPUT_TIMEOUT_ENV) {
        Err(std::env::VarError::NotPresent) => Ok(OUTPUT_TIMEOUT),
        value => {
            let seconds: u64 = value
                .with_context(|| format!("invalid {OUTPUT_TIMEOUT_ENV}"))?
                .parse()
                .ok()
                .filter(|seconds| (1..=crate::domain::MAX_TIMEOUT_SECS).contains(seconds))
                .with_context(|| {
                    format!(
                        "{OUTPUT_TIMEOUT_ENV} must be an integer from 1 to {}",
                        crate::domain::MAX_TIMEOUT_SECS
                    )
                })?;
            Ok(Duration::from_secs(seconds))
        }
    }
}

/// The `--progress-file` of a run, when asked for.
pub(crate) fn progress_file(
    options: &crate::ExecutionOptions,
    invocation: &str,
    recipe: &Path,
    operation: crate::worker::records::Operation,
) -> Option<crate::progress_file::ProgressFile> {
    options.progress_file.as_deref().map(|path| {
        crate::progress_file::ProgressFile::start(
            path,
            invocation,
            recipe
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or_default(),
            &operation.to_string(),
        )
    })
}

// ---------------------------------------------------------------------------
// Line records on standard output
// ---------------------------------------------------------------------------

/// Relay the worker's line records until its standard output ends; returns
/// the execution deadline the stream ran under. Read errors are returned
/// with a `protocol` category (or cancellation); the caller enriches them
/// with the run's outcome.
///
/// Once `child` (the host worker, or `ginux start --wait`) has exited, a
/// process still holding the pipe gets [`SHUTDOWN_GRACE`] to close it; then
/// what is already buffered is relayed and the pump returns, and the caller
/// ends the process group.
#[allow(clippy::too_many_arguments)]
pub(crate) fn pump_lines<I: LineInput>(
    invocation: &str,
    child: &mut Child,
    stdout: I,
    worker_diagnostics: &mut std::fs::File,
    worker_started: &mut bool,
    timeout: Duration,
    execution: Duration,
    progress: &mut Option<crate::progress_file::ProgressFile>,
) -> Result<Instant> {
    let execution_deadline = Instant::now() + execution;
    let startup_deadline = Instant::now() + timeout;
    let mut stdout = stdout.into_source(timeout)?;
    let mut lines = LineReader::default();
    let mut budget = crate::worker::records::DiagnosticBudget::default();
    let mut plain_bytes = 0usize;
    // When the child exited, plus [`SHUTDOWN_GRACE`].
    let mut grace_end: Option<Instant> = None;
    loop {
        if Instant::now() >= execution_deadline {
            bail!(
                crate::failure::Category::Deadline.error("supervised execution deadline expired")
            );
        }
        if grace_end.is_none()
            && child
                .try_wait()
                .map_err(|err| classify_stream_error(invocation, "watch the worker process", err))?
                .is_some()
        {
            grace_end = Some(Instant::now() + SHUTDOWN_GRACE);
        }
        let grace_over = grace_end.is_some_and(|end| Instant::now() >= end);
        let idle = if *worker_started {
            execution_deadline
        } else {
            startup_deadline
        };
        // Wait in short slices so the child's exit is seen; past the grace,
        // read only what is already buffered.
        let slice = if grace_over {
            Instant::now()
        } else {
            Instant::now() + EXIT_CHECK
        };
        let (line, cut, last) =
            match lines.next(&mut stdout, idle, timeout, execution_deadline, slice) {
                Ok(Some((line, cut))) => (line, cut, false),
                Ok(None) => return Ok(execution_deadline),
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    if !grace_over {
                        continue;
                    }
                    match lines.rest() {
                        Some(rest) => (rest, false, true),
                        None => return Ok(execution_deadline),
                    }
                }
                Err(err) => {
                    return Err(classify_stream_error(invocation, "read worker output", err));
                }
            };
        // Runtime text (a Ginux console's `GINUX_` lines) is the source's,
        // not the worker's.
        let line = if cut {
            line
        } else {
            match stdout.accept(line) {
                Some(line) => line,
                None if last => return Ok(execution_deadline),
                None => continue,
            }
        };
        *worker_started = true;
        let record = if cut { None } else { LineRecord::parse(&line) };
        match record {
            Some(LineRecord {
                diagnostic: Some(record),
                ..
            }) => {
                budget.accept(&record).map_err(|violation| {
                    crate::failure::Category::Protocol.error(violation.to_string())
                })?;
                if record.truncated {
                    crate::diagnostics::line(
                        "recipe: diagnostic limit reached; remaining output drained",
                    );
                } else {
                    // All human bytes go to CLI stderr, never to its JSON stream.
                    // Supervisor stderr is collected independently and never parsed.
                    worker_diagnostics.write_all(&record.data)?;
                    let _ =
                        crate::diagnostics::write(crate::diagnostics::Stream::Recipe, &record.data);
                }
            }
            Some(LineRecord {
                progress: Some(report),
                ..
            }) => {
                if let Some(progress) = progress.as_mut() {
                    progress.report(&report);
                }
            }
            // Plain text: kept, within its own bound, never an error.
            _ => {
                if plain_bytes < MAX_DIAGNOSTIC_BYTES {
                    let mut text = line;
                    text.truncate(MAX_DIAGNOSTIC_BYTES - plain_bytes);
                    text.push(b'\n');
                    plain_bytes += text.len();
                    worker_diagnostics.write_all(&text)?;
                    let _ = crate::diagnostics::write(crate::diagnostics::Stream::Recipe, &text);
                }
            }
        }
        if last {
            return Ok(execution_deadline);
        }
    }
}

/// Splits a byte stream into lines of at most [`MAX_LINE_BYTES`]. A longer
/// line is handed out once, cut at the bound (plain text), and the rest of
/// it up to its newline is dropped.
#[derive(Default)]
struct LineReader {
    buffer: Vec<u8>,
    /// When the first byte of the line being read arrived.
    started: Option<Instant>,
    /// Dropping the rest of an overlong line.
    discarding: bool,
}

impl LineReader {
    /// The next line (without its newline) and whether it was cut, or None
    /// at end of file. `idle` bounds the wait for a line's first byte; once
    /// it arrived, the line must be complete within `timeout`. A wait that
    /// reaches `slice` before those deadlines returns `WouldBlock`, keeping
    /// the partial line for the next call.
    fn next<S: LineSource>(
        &mut self,
        pipe: &mut S,
        idle: Instant,
        timeout: Duration,
        execution_deadline: Instant,
        slice: Instant,
    ) -> std::io::Result<Option<(Vec<u8>, bool)>> {
        let mut chunk = [0u8; 8192];
        loop {
            if let Some(end) = self.buffer.iter().position(|byte| *byte == b'\n') {
                let mut line: Vec<u8> = self.buffer.drain(..=end).collect();
                line.pop();
                self.started = (!self.buffer.is_empty()).then(Instant::now);
                if std::mem::take(&mut self.discarding) {
                    continue;
                }
                return Ok(Some((line, false)));
            }
            if self.buffer.len() >= MAX_LINE_BYTES {
                let line = std::mem::take(&mut self.buffer);
                if !std::mem::replace(&mut self.discarding, true) {
                    return Ok(Some((line, true)));
                }
            }
            let deadline = match self.started {
                Some(started) => started + timeout,
                None => idle,
            }
            .min(execution_deadline);
            pipe.set_deadline(deadline.min(slice), execution_deadline);
            let read = match pipe.read(&mut chunk) {
                Err(error)
                    if error.kind() == std::io::ErrorKind::TimedOut
                        && Instant::now() < deadline =>
                {
                    return Err(std::io::ErrorKind::WouldBlock.into());
                }
                read => read?,
            };
            if read == 0 {
                return Ok(self.rest().map(|rest| (rest, false)));
            }
            if self.started.is_none() {
                self.started = Some(Instant::now());
            }
            self.buffer.extend_from_slice(&chunk[..read]);
        }
    }

    /// The incomplete last line, if any, as the stream ends.
    fn rest(&mut self) -> Option<Vec<u8>> {
        let rest = std::mem::take(&mut self.buffer);
        (!rest.is_empty() && !std::mem::take(&mut self.discarding)).then_some(rest)
    }
}

/// A source of worker output lines for [`pump_lines`]: a read waits at most
/// until the deadline last set and then fails with `TimedOut`; `Ok(0)` is the
/// end of the stream.
pub(crate) trait LineSource: Read {
    /// The absolute deadline of the next reads, and the execution deadline
    /// (which names the timeout in its message).
    fn set_deadline(&mut self, deadline: Instant, execution_deadline: Instant);
    /// One complete line as read; None when it is the source's own text and
    /// no worker output.
    fn accept(&mut self, line: Vec<u8>) -> Option<Vec<u8>> {
        Some(line)
    }
}

impl<T: LineSource + ?Sized> LineSource for &mut T {
    fn set_deadline(&mut self, deadline: Instant, execution_deadline: Instant) {
        (**self).set_deadline(deadline, execution_deadline);
    }
    fn accept(&mut self, line: Vec<u8>) -> Option<Vec<u8>> {
        (**self).accept(line)
    }
}

/// What [`pump_lines`] reads from: the host worker's standard output, or a
/// source the caller keeps (`&mut ConsoleStream`).
pub(crate) trait LineInput {
    type Source: LineSource;
    fn into_source(self, timeout: Duration) -> std::io::Result<Self::Source>;
}

impl LineInput for std::process::ChildStdout {
    type Source = DeadlinePipe<std::process::ChildStdout>;
    fn into_source(self, timeout: Duration) -> std::io::Result<Self::Source> {
        DeadlinePipe::new(self, timeout)
    }
}

impl<'a, T: LineSource> LineInput for &'a mut T {
    type Source = &'a mut T;
    fn into_source(self, _timeout: Duration) -> std::io::Result<Self::Source> {
        Ok(self)
    }
}

impl<T: AsRawFd + Read> LineSource for DeadlinePipe<T> {
    fn set_deadline(&mut self, deadline: Instant, execution_deadline: Instant) {
        self.deadline = deadline;
        self.execution_deadline = Some(execution_deadline);
    }
}

/// A nonblocking pipe read under one absolute deadline. Nonblocking
/// descriptors prevent a poll/read race from turning the next read into an
/// unbounded wait.
pub(crate) struct DeadlinePipe<T> {
    inner: T,
    deadline: Instant,
    execution_deadline: Option<Instant>,
}

impl<T: AsRawFd> DeadlinePipe<T> {
    fn new(inner: T, timeout: Duration) -> std::io::Result<Self> {
        let fd = inner.as_raw_fd();
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Self {
            inner,
            deadline: Instant::now() + timeout,
            execution_deadline: None,
        })
    }
    fn reset(&mut self, timeout: Duration) {
        self.deadline = Instant::now() + timeout;
    }
    fn wait(&mut self) -> std::io::Result<()> {
        loop {
            crate::cancellation::check_io()?;
            let left = self.deadline.saturating_duration_since(Instant::now());
            // A deadline already past still takes what is buffered: one
            // poll with no wait, then the timeout.
            let mut poll = libc::pollfd {
                fd: self.inner.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let millis = left.as_millis().min(100) as i32;
            let result = unsafe { libc::poll(&mut poll, 1, millis) };
            if result < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error);
            }
            if result > 0 {
                if poll.revents & libc::POLLNVAL != 0 {
                    return Err(std::io::Error::from_raw_os_error(libc::EBADF));
                }
                return Ok(());
            }
            if left.is_zero() {
                return Err(timed_out(self.execution_deadline));
            }
        }
    }
}

impl<T: AsRawFd + Read> Read for DeadlinePipe<T> {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        loop {
            self.wait()?;
            match self.inner.read(bytes) {
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                    ) =>
                {
                    continue;
                }
                result => return result,
            }
        }
    }
}

/// The `TimedOut` error of a source whose deadline passed.
fn timed_out(execution_deadline: Option<Instant>) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::TimedOut,
        if execution_deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            "supervised execution deadline expired"
        } else {
            "complete worker output line deadline expired"
        },
    )
}

/// Bytes of a Ginux console's own `GINUX_` lines kept for the supervisor log.
const MAX_RUNTIME_TEXT: usize = 64 * 1024;

/// How often [`ConsoleStream`] looks at the free space of the Ginux state
/// root, at most.
const DISK_CHECK: Duration = Duration::from_secs(1);

/// The longest single wait of a [`ConsoleStream`] read, so the free-space
/// check runs while the console is silent.
const DISK_SLICE: Duration = Duration::from_millis(250);

/// The worker's line records as a Ginux instance writes them: the standard
/// output of `ginux start --wait --stream-console`, where the helper copies
/// the instance console (the worker's standard output through the
/// pseudo-terminal of `script(1)`) and nothing else. The instance directory
/// and its logs are root-private; nothing is read from them.
///
/// - The terminal turns each `\n` into `\r\n`; one trailing `\r` is removed
///   from every line. Nothing is ever written to the console's input, so
///   there is no echo.
/// - Lines starting `GINUX_` are the runtime's (its start summary precedes
///   the worker's first line): kept for the supervisor log
///   ([`ConsoleStream::runtime_text`]), never relayed.
/// - While it waits (in slices of at most [`DISK_SLICE`]) it watches, at
///   most every [`DISK_CHECK`], the free space of the filesystem holding the
///   instance roots: below 2 GiB or 5 %, whichever is larger, or when the
///   free space cannot be read, the read fails and
///   [`ConsoleStream::low_disk`] says why. Ginux bounds no instance storage,
///   so this is the only brake on a run filling the disk.
pub(crate) struct ConsoleStream {
    pipe: DeadlinePipe<std::process::ChildStdout>,
    deadline: Instant,
    execution_deadline: Option<Instant>,
    runtime: Vec<u8>,
    watch: PathBuf,
    next_check: Instant,
    low_disk: Option<String>,
}

impl ConsoleStream {
    /// Read `stdout` (of `ginux start --wait --stream-console`), watching the
    /// free space of `watch` (the state root).
    pub(crate) fn new(stdout: std::process::ChildStdout, watch: PathBuf) -> std::io::Result<Self> {
        Ok(Self {
            pipe: DeadlinePipe::new(stdout, Duration::ZERO)?,
            deadline: Instant::now(),
            execution_deadline: None,
            runtime: Vec::new(),
            watch,
            next_check: Instant::now(),
            low_disk: None,
        })
    }

    /// The runtime's own lines seen on the console, each ending in `\n`.
    pub(crate) fn runtime_text(&self) -> &[u8] {
        &self.runtime
    }

    /// Why the run was stopped for lack of disk space, if it was.
    pub(crate) fn low_disk(&self) -> Option<&str> {
        self.low_disk.as_deref()
    }

    fn check_disk(&mut self) -> std::io::Result<()> {
        if let Some(message) = &self.low_disk {
            return Err(std::io::Error::other(message.clone()));
        }
        if Instant::now() < self.next_check {
            return Ok(());
        }
        self.next_check = Instant::now() + DISK_CHECK;
        let path = self.watch.display();
        let message = match free_space(&self.watch) {
            Ok((free, total)) => {
                let floor = (2u64 << 30).max(total / 20);
                if free >= floor {
                    return Ok(());
                }
                format!(
                    "the host disk holding {path} is nearly full ({} MiB free, the run stops \
                     below {} MiB); the run was stopped so it cannot fill it",
                    free >> 20,
                    floor >> 20
                )
            }
            Err(error) => format!(
                "cannot read the free space of the host disk holding {path} ({error}); the run \
                 was stopped because Recipe cannot keep it from filling that disk"
            ),
        };
        self.low_disk = Some(message.clone());
        Err(std::io::Error::other(message))
    }
}

/// Free and total bytes of the filesystem holding `path` (`statvfs`).
fn free_space(path: &Path) -> std::io::Result<(u64, u64)> {
    let name = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())
        .map_err(std::io::Error::other)?;
    let mut stats: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(name.as_ptr(), &mut stats) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    let unit = stats.f_frsize as u64;
    Ok((
        (stats.f_bavail as u64).saturating_mul(unit),
        (stats.f_blocks as u64).saturating_mul(unit),
    ))
}

impl Read for ConsoleStream {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        loop {
            self.check_disk()?;
            // Wait in slices so the disk check runs while nothing arrives;
            // the pipe checks cancellation while it waits.
            self.pipe.deadline = (Instant::now() + DISK_SLICE).min(self.deadline);
            self.pipe.execution_deadline = self.execution_deadline;
            match self.pipe.read(bytes) {
                Err(error)
                    if error.kind() == std::io::ErrorKind::TimedOut
                        && Instant::now() < self.deadline => {}
                Err(error) if error.kind() == std::io::ErrorKind::TimedOut => {
                    return Err(timed_out(self.execution_deadline));
                }
                read => return read,
            }
        }
    }
}

impl LineSource for ConsoleStream {
    fn set_deadline(&mut self, deadline: Instant, execution_deadline: Instant) {
        self.deadline = deadline;
        self.execution_deadline = Some(execution_deadline);
    }

    fn accept(&mut self, mut line: Vec<u8>) -> Option<Vec<u8>> {
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        if line.starts_with(b"GINUX_") {
            if self.runtime.len() < MAX_RUNTIME_TEXT {
                let room = MAX_RUNTIME_TEXT - self.runtime.len();
                self.runtime
                    .extend_from_slice(&line[..line.len().min(room.saturating_sub(1))]);
                self.runtime.push(b'\n');
            }
            return None;
        }
        Some(line)
    }
}

/// Categorize a read error on the worker's output: cancellation, or a
/// `protocol` failure.
fn classify_stream_error(
    invocation: &str,
    what: &str,
    err: impl std::fmt::Display,
) -> anyhow::Error {
    if crate::cancellation::signal() != 0 {
        return crate::cancellation::cancelled().into();
    }
    anyhow::Error::new(
        crate::failure::Category::Protocol
            .error(format!("invocation {invocation}: cannot {what}: {err}")),
    )
}

pub(crate) fn exit_message(exit: Option<i32>) -> String {
    match exit {
        Some(code) => format!("code {code}"),
        None => "no exit code (killed or signaled)".to_string(),
    }
}

// ---------------------------------------------------------------------------
// The result file
// ---------------------------------------------------------------------------

/// The `protocol` failure of a run that ended without a valid result file;
/// `reason` says what was found instead.
pub(crate) fn missing_result(invocation: &str, reason: &str) -> anyhow::Error {
    if crate::cancellation::signal() != 0 {
        return crate::cancellation::cancelled().into();
    }
    anyhow::Error::new(crate::failure::Category::Protocol.error(format!(
        "invocation {invocation}: the worker ended without a valid result: {reason}"
    )))
}

/// The result file of a run whose `work` tree is in the archive `archive`
/// (a Ginux `extract` of `/work` and `/out`: members `work/…`, `out/…`). The
/// archive is untrusted: bounded like a publication
/// (`crate::publication::prepare`), the member must appear once, as a regular
/// file within [`crate::domain::MAX_RESULT_BYTES`]. No archive (nothing is
/// extracted unless the workload exited) is no result.
pub(crate) fn result_from_archive(
    archive: &Path,
    invocation: &str,
    options: &crate::ExecutionOptions,
) -> std::result::Result<WorkerResult, String> {
    let file =
        crate::fsx::open_regular(archive).map_err(|error| format!("no export ({error:#})"))?;
    let length = file
        .metadata()
        .map_err(|error| format!("cannot read the export: {error}"))?
        .len();
    if length > options.archive_bound() {
        return Err("the export exceeds its bound".into());
    }
    let max_entries = (options.storage_max_objects as usize).saturating_mul(3);
    let mut archive = tar::Archive::new(file);
    let mut found = None;
    let entries = archive
        .entries_with_seek()
        .map_err(|error| format!("cannot read the export: {error}"))?;
    for (count, entry) in entries.enumerate() {
        if count >= max_entries {
            return Err("the export has excessive entries".into());
        }
        let mut entry = entry.map_err(|error| format!("cannot read the export: {error}"))?;
        if entry.path_bytes().as_ref() != crate::domain::RESULT_MEMBER.as_bytes() {
            continue;
        }
        if found.is_some() {
            return Err("the export holds the result twice".into());
        }
        if !entry.header().entry_type().is_file() {
            return Err("the result in the export is not a regular file".into());
        }
        if entry.size() > crate::domain::MAX_RESULT_BYTES {
            return Err("the result in the export exceeds its bound".into());
        }
        found = Some(
            crate::worker::records::read_bounded(&mut entry, crate::domain::MAX_RESULT_BYTES)
                .map_err(|error| format!("cannot read the result: {error}"))?,
        );
    }
    let bytes = found.ok_or_else(|| "the export holds no result file".to_string())?;
    WorkerResult::decode(&bytes, invocation).map_err(|error| error.to_string())
}

/// The result file of a host run, read from its run directory `root` once
/// the worker has exited: walked by descriptor, never through a link, and
/// bounded by [`crate::domain::MAX_RESULT_BYTES`].
pub(crate) fn result_from_tree(
    root: &Path,
    invocation: &str,
) -> std::result::Result<WorkerResult, String> {
    let open = || -> std::io::Result<std::fs::File> {
        let root = crate::fsx::open_dir(root)?;
        let work = crate::fsx::open_dir_at(
            root.as_fd(),
            &crate::fsx::cstring(crate::domain::WORK.trim_start_matches('/').as_bytes())?,
        )?;
        crate::fsx::open_regular_at(
            work.as_fd(),
            &crate::fsx::cstring(crate::domain::RESULT_NAME.as_bytes())?,
        )
    };
    let file = match open() {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err("the worker wrote no result file".into());
        }
        Err(error) => return Err(format!("cannot open the result file: {error}")),
    };
    let bytes = crate::worker::records::read_bounded(file, crate::domain::MAX_RESULT_BYTES)
        .map_err(|error| format!("cannot read the result file: {error}"))?;
    WorkerResult::decode(&bytes, invocation).map_err(|error| error.to_string())
}

// ---------------------------------------------------------------------------
// Process shutdown
// ---------------------------------------------------------------------------

/// Await the host worker's completion. Failure and interruption use short
/// teardown; successful work keeps its execution budget. (A Ginux run ends
/// its `ginux start --wait` process with `crate::ginux::stop_group`.)
pub(crate) fn reap_launcher(child: &mut Child, mut deadline: Instant) -> Result<Option<i32>> {
    let mut interrupted = false;
    loop {
        let signal = crate::cancellation::signal();
        if signal != 0 && !interrupted {
            unsafe {
                libc::kill(-(child.id() as i32), signal);
            }
            interrupted = true;
            deadline = deadline.min(Instant::now() + SHUTDOWN_GRACE);
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                // A malformed worker may leave a child holding its pipes.
                unsafe {
                    libc::kill(-(child.id() as i32), libc::SIGKILL);
                }
                crate::cancellation::check()?;
                return Ok(status.code());
            }
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(25));
            }
            result => {
                unsafe {
                    libc::kill(-(child.id() as i32), libc::SIGKILL);
                }
                let _ = child.kill();
                let _ = child.wait();
                crate::cancellation::check()?;
                if let Err(error) = result {
                    return Err(error).context("cannot reap the worker");
                }
                bail!(
                    crate::failure::Category::Deadline
                        .error("supervised execution deadline expired during finalization")
                );
            }
        }
    }
}

/// The line `ginux start --wait --stream-console` writes to its standard
/// error, after everything else, before the audit log's tail on a non-zero
/// outcome: `ginux: audit log (last N bytes):`.
const AUDIT_MARKER: (&[u8], &[u8]) = (b"ginux: audit log (last ", b" bytes):");

/// Split the drained standard error of `ginux start --wait --stream-console`
/// (`cli_log`, written by [`spawn_stderr_drain`]) at the audit marker
/// ([`AUDIT_MARKER`], the last such line): what follows it (the tail of the
/// instance's audit log: the worker's standard error and the runtime's audit
/// lines) goes to `supervisor_log` followed by `extra` (the console's
/// runtime lines) within the retention cap, and `cli_log` keeps what
/// precedes it. Returns the supervisor log with its tail for failure
/// messages, and the tail of the CLI's own text. Without a marker (a clean
/// outcome, or a CLI that died first) the supervisor log holds only `extra`.
pub(crate) fn split_cli_log(
    cli_log: &Path,
    supervisor_log: &Path,
    extra: &[u8],
) -> (DrainedLog, String) {
    let mut bytes = Vec::new();
    if let Ok(file) = std::fs::File::open(cli_log) {
        let _ = file.take(LOG_CAP_BYTES).read_to_end(&mut bytes);
    }
    let marker = audit_marker(&bytes);
    let (cli, audit) = match marker {
        Some((start, end)) => (&bytes[..start], &bytes[end..]),
        None => (&bytes[..], &[][..]),
    };
    if marker.is_some()
        && let Err(error) = std::fs::write(cli_log, cli)
    {
        crate::diagnostics::diag!(
            "recipe: warning: cannot rewrite {}: {error}",
            cli_log.display()
        );
    }
    let mut retained: u64 = 0;
    match std::fs::File::create(supervisor_log) {
        Ok(mut out) => {
            for part in [audit, extra] {
                let take = part
                    .len()
                    .min(LOG_CAP_BYTES.saturating_sub(retained) as usize);
                if take > 0 && out.write_all(&part[..take]).is_ok() {
                    retained += take as u64;
                }
            }
        }
        Err(error) => crate::diagnostics::diag!(
            "recipe: warning: supervisor log {} is incomplete: {error}",
            supervisor_log.display()
        ),
    }
    let tail = |part: &[u8]| {
        String::from_utf8_lossy(&part[part.len().saturating_sub(LOG_TAIL_BYTES)..])
            .trim()
            .to_owned()
    };
    let total = (audit.len() + extra.len()) as u64;
    (
        DrainedLog {
            total_bytes: total,
            discarded_bytes: total.saturating_sub(retained),
            tail: tail(audit),
        },
        tail(cli),
    )
}

/// The byte range of the last [`AUDIT_MARKER`] line in `bytes`, newline
/// included.
fn audit_marker(bytes: &[u8]) -> Option<(usize, usize)> {
    let (prefix, suffix) = AUDIT_MARKER;
    let mut found = None;
    let mut start = 0;
    while start < bytes.len() {
        let end = bytes[start..]
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(bytes.len(), |at| start + at + 1);
        let line = &bytes[start..end];
        let line = line.strip_suffix(b"\n").unwrap_or(line);
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if let Some(rest) = line.strip_prefix(prefix)
            && let Some(count) = rest.strip_suffix(suffix)
            && !count.is_empty()
            && count.iter().all(u8::is_ascii_digit)
        {
            found = Some((start, end));
        }
        start = end;
    }
    found
}

// ---------------------------------------------------------------------------
// Stderr drain (bounded log collection)
// ---------------------------------------------------------------------------

/// Outcome of the stderr drain thread.
pub(crate) struct DrainedLog {
    pub total_bytes: u64,
    pub discarded_bytes: u64,
    pub tail: String,
}

/// Drain a child's stderr to a log file with a retention cap, so a chatty
/// worker can never block on a full stderr pipe. Bytes past the cap are
/// still consumed (and counted) but discarded. The log is never parsed.
pub(crate) struct Drain {
    stop: Arc<AtomicBool>,
    thread: std::thread::JoinHandle<DrainedLog>,
}

pub(crate) fn spawn_stderr_drain(stderr: std::process::ChildStderr, log_path: PathBuf) -> Drain {
    let stop = Arc::new(AtomicBool::new(false));
    let stopping = stop.clone();
    let thread = std::thread::spawn(move || {
        let mut stderr = match DeadlinePipe::new(stderr, Duration::from_millis(250)) {
            Ok(stderr) => stderr,
            Err(error) => {
                return DrainedLog {
                    total_bytes: 0,
                    discarded_bytes: 0,
                    tail: format!("<cannot drain stderr: {error}>"),
                };
            }
        };
        let mut shutdown = None;
        let lost = |error: std::io::Error| {
            crate::diagnostics::diag!(
                "recipe: warning: supervisor log {} is incomplete: {error}",
                log_path.display()
            );
        };
        let mut file = std::fs::File::create(&log_path).map_err(lost).ok();
        let mut total: u64 = 0;
        let mut retained: u64 = 0;
        let mut tail: Vec<u8> = Vec::new();
        let mut buf = [0u8; 8192];
        loop {
            if stopping.load(Ordering::Relaxed) {
                let deadline =
                    shutdown.get_or_insert_with(|| Instant::now() + Duration::from_secs(1));
                if Instant::now() >= *deadline {
                    break;
                }
            }
            stderr.reset(Duration::from_millis(250));
            match stderr.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    total += n as u64;
                    if let Some(out) = file.as_mut()
                        && retained < LOG_CAP_BYTES
                    {
                        let room = (LOG_CAP_BYTES - retained) as usize;
                        let take = room.min(n);
                        if let Err(error) = out.write_all(&buf[..take]) {
                            lost(error);
                            file = None;
                        }
                        retained += take as u64;
                    }
                    tail.extend_from_slice(&buf[..n]);
                    if tail.len() > LOG_TAIL_BYTES * 2 {
                        let drop = tail.len() - LOG_TAIL_BYTES * 2;
                        tail.drain(..drop);
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::TimedOut => {
                    if shutdown.is_some() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        if let Some(mut out) = file
            && let Err(error) = out.flush()
        {
            lost(error);
        }
        let start = tail.len().saturating_sub(LOG_TAIL_BYTES);
        DrainedLog {
            total_bytes: total,
            discarded_bytes: total.saturating_sub(retained),
            tail: String::from_utf8_lossy(&tail[start..]).into_owned(),
        }
    });
    Drain { stop, thread }
}

/// Join the drain thread; a dead drain is reported, never silent.
pub(crate) fn join_drain(handle: Drain) -> DrainedLog {
    handle.stop.store(true, Ordering::Relaxed);
    handle.thread.join().unwrap_or(DrainedLog {
        total_bytes: 0,
        discarded_bytes: 0,
        tail: "<stderr drain thread failed>".to_string(),
    })
}
