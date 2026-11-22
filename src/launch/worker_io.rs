//! The coordinator's side of a running worker, shared by both runtimes (the
//! configured Ginux launch, [`super::configured`], and the host run,
//! `crate::host`): reading its line records from standard output, draining
//! standard error to a capped log, reaping it, and reading its result file
//! afterwards ([`crate::worker::records`]).
//!
//! - Lines: one [`LineRecord`] per line, relayed live to `diagnostics.rs`
//!   and `worker.log` (diagnostics) or the `--progress-file` (progress). A
//!   line that is not a record is kept as plain log text (bounded like the
//!   diagnostics, never an error). A diagnostic record that breaks the
//!   budget rules is a `protocol` failure, as it always was.
//! - Stall deadline: before the first line the worker has the output timeout
//!   (`RECIPE_OUTPUT_TIMEOUT_SECS`, 300 s by default) to say something; once
//!   it has, a silent phase may run until the separate execution deadline,
//!   but a line, once its first byte arrived, must be complete within the
//!   output timeout. "No complete output line in N seconds" replaces the
//!   frame deadline of the frame protocol, with the same default and bound.
//! - The stream ends at end of file (the worker, and in Ginux the launcher,
//!   exited). Only then is the result file read: from the export archive of
//!   a Ginux run ([`result_from_archive`]), from the run directory of a host
//!   run ([`result_from_tree`]). Either is untrusted: bounded, strictly
//!   parsed, and it must name this invocation. No valid result is a
//!   [`missing_result`] `protocol` failure, which the runtime enriches with
//!   the run's outcome exactly as a stream that ended without a result frame
//!   was.

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

/// Bound on log collection: launcher/worker stderr beyond this many bytes is
/// still drained (so a chatty worker never blocks on a full pipe) but not
/// retained.
const LOG_CAP_BYTES: u64 = crate::invocation_log::MAX_LOG_BYTES;

/// Grace for teardown after failure/cancellation. Successful final export uses
/// the remaining supervised execution deadline instead.
pub(crate) const SHUTDOWN_GRACE: Duration = Duration::from_secs(10);

/// How often the line pump looks whether the launcher (or host worker) has
/// exited while it waits for output.
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
/// Once `child` (the launcher, or the host worker) has exited, a process
/// still holding its standard output gets [`SHUTDOWN_GRACE`] to close it;
/// then what is already buffered is relayed and the pump returns, and the
/// caller's [`reap_launcher`] ends the process group.
pub(crate) fn pump_lines(
    invocation: &str,
    child: &mut Child,
    stdout: std::process::ChildStdout,
    worker_diagnostics: &mut std::fs::File,
    worker_started: &mut bool,
    timeout: Duration,
    execution: Duration,
    progress: &mut Option<crate::progress_file::ProgressFile>,
) -> Result<Instant> {
    let execution_deadline = Instant::now() + execution;
    let startup_deadline = Instant::now() + timeout;
    let mut stdout = DeadlinePipe::new(stdout, timeout)?;
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
                .map_err(|err| classify_stream_error(invocation, "watch the launcher", err))?
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
    fn next(
        &mut self,
        pipe: &mut DeadlinePipe<std::process::ChildStdout>,
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
    fn set_deadline(&mut self, deadline: Instant, execution_deadline: Instant) {
        self.deadline = deadline;
        self.execution_deadline = Some(execution_deadline);
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
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    if self
                        .execution_deadline
                        .is_some_and(|deadline| Instant::now() >= deadline)
                    {
                        "supervised execution deadline expired"
                    } else {
                        "complete worker output line deadline expired"
                    },
                ));
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

/// The result file of a run whose `work` tree is in the export archive
/// `archive` (a Ginux export: members `work/…`, `out/…`). The archive is
/// untrusted: bounded like a publication (`crate::publication::prepare`),
/// the member must appear once, as a regular file within
/// [`crate::domain::MAX_RESULT_BYTES`]. No archive (the supervisor exports
/// only after a clean exit) is no result.
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

/// Await actual launcher (or host worker) completion, including a final
/// export. Failure and interruption use short teardown; successful work keeps
/// its execution budget.
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
                // A malformed launcher may leave a child holding its pipes.
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
                    return Err(error).context("cannot reap configured launcher");
                }
                bail!(
                    crate::failure::Category::Deadline
                        .error("supervised execution deadline expired during finalization")
                );
            }
        }
    }
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

/// Drain launcher stderr to a log file with a retention cap, so a chatty
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
