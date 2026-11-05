//! Ginux subprocess launch and supervision (R06).
//!
//! [`run_domain`] is the R-host subprocess adapter: it spawns the configured
//! launcher (`GINUX_BIN` or backend config), hands it the R05 domain config
//! plus a fresh `--result-file` path, pumps the R01 worker protocol over the
//! launcher's stdio, and maps outcomes to the interface §1 categories with
//! the result file authoritative (§2).
//!
//! Transport discipline (matches `docs/ginux-recipe-interfaces.md` §§2/4):
//!
//! - Launcher argv: `launch <config.toml> <prepared-rootfs> --result-file <fresh host path>`.
//!   The config file is the staged `config.toml` from [`PreparedInvocation`];
//!   the result file is a fresh path in a per-invocation run directory that
//!   the launcher creates exclusively (`O_EXCL` semantics belong to the
//!   launcher; a missing or malformed result file is a `protocol` failure,
//!   never success).
//! - Worker stdin/stdout are the opaque protocol pipes: the adapter sends one
//!   `request` then one `ack` per checkpoint and receives `checkpoint`, at
//!   most one `failure`, and exactly one `result`. Worker stderr (launcher
//!   stderr, which also carries `ginux:`-prefixed supervisor diagnostics) is
//!   drained to a log file with a byte cap and never parsed: log bytes only
//!   appear in human diagnostics, never as results.
//! - Acknowledged checkpoints record `{seq → generation}` in the returned
//!   report only. Generations are `0` here (no durable storage claim);
//!   R07/G12a own durable generations and the state record.
//!
//! Before launch, the input owner snapshots and verifies the selected assembled
//! rootfs tar, creates a fresh domain root, and copies the worker plus staged
//! inputs into `/inputs`. The root is private to this invocation and is kept
//! alive until the launcher is reaped. No stub-launcher environment is supplied.
//!
//! Failure categories (§1): `startup` (bad config, spawn failure, launcher
//! that cannot speak the transport, supervisor-error result),
//! `workload` (worker-reported recipe failure, nonzero exit / terminating
//! signal per the result file), `protocol` (framing violations, timeouts,
//! missing/malformed result file, exit without `result`, contradictions
//! between stream and result file). `storage`/`cancelled` are not produced
//! here (R07/L03 own them).

use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};

use crate::inputs::PreparedInvocation;
use crate::worker::protocol::{
    AckPayload, Direction, FramePayload, FrameReceiver, FrameSender, RequestPayload, ResultPayload,
};

/// Bound on silent worker output: one frame wait lasts at most this long
/// before the run fails as a `protocol` timeout. Chosen symmetric with the
/// worker's 300 s ack bound (R02): ordinary phases answer promptly, while a
/// hung worker cannot stall the coordinator forever. L03 owns
/// cancellation/interruption refinement; a legitimate phase silent longer
/// than this is an R06 limit (named follow-up).
const FRAME_TIMEOUT: Duration = Duration::from_secs(300);

/// Bound on checkpoint chatter: more than this many checkpoints in one
/// invocation is a `protocol` failure, never unbounded acking.
const MAX_CHECKPOINTS: u64 = 1024;

/// Bound on log collection: launcher/worker stderr beyond this many bytes is
/// still drained (so a chatty worker never blocks on a full pipe) but not
/// retained.
const LOG_CAP_BYTES: u64 = 16 * 1024 * 1024;

/// Grace period for launcher shutdown after the stream ends before it is
/// killed and reaped.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(10);

/// Bytes of log tail attached to failure diagnostics.
const LOG_TAIL_BYTES: usize = 4096;

/// Placeholder durable generation acked per checkpoint until R07/G12a wire
/// real storage generations through this path.
const UNGROUNDED_GENERATION: u64 = 0;

/// Outcome of one supervised domain run.
#[derive(Debug)]
pub struct LaunchReport {
    /// The exactly-once worker result frame.
    pub result: ResultPayload,
    /// The worker `failure` frame when the worker sent one before `result`.
    pub failure: Option<crate::worker::protocol::FailurePayload>,
    /// Checkpoints acked during the run.
    pub checkpoints_acked: u64,
    /// Invocation-local `{checkpoint seq → acked generation}` association
    /// (§4; R07 persists this with the state record).
    pub generations: Vec<(u64, u64)>,
    /// Launcher exit code when it exited normally (`None` when killed or
    /// signaled).
    pub launcher_code: Option<i32>,
    /// Authoritative result-file outcome (`exited`, `signaled`, or
    /// `supervisor-error`).
    pub result_outcome: String,
    /// Authoritative result-file exit code (workload exit when `exited`).
    pub result_code: Option<i64>,
    /// Authoritative result-file signal number (when `signaled`).
    pub result_signal: Option<u64>,
    /// Authoritative result-file storage generation (R07 consumes it).
    pub result_generation: Option<u64>,
    /// Total launcher/worker stderr bytes observed (retained portion is
    /// capped at [`LOG_CAP_BYTES`]).
    pub log_total_bytes: u64,
    /// Launcher/worker stderr bytes retained beyond the cap (drained and
    /// discarded).
    pub log_discarded_bytes: u64,
    /// Tail of the retained stderr log for human diagnostics (never parsed).
    pub log_tail: String,
}

/// Run the prepared invocation through the launcher and pump the worker
/// protocol to exactly one result.
///
/// `persist_ctx` travels in the request frame; worker checkpoints are acked
/// with [`UNGROUNDED_GENERATION`] and recorded in the report. Errors carry
/// the §1 category (`startup` / `workload` / `protocol`) and refuse host
/// execution: no path here evaluates recipe code on the host.
pub fn run_domain(
    prepared: &PreparedInvocation,
    launcher: &Path,
    persist_ctx: bool,
    options: &crate::worker::protocol::InvocationOptions,
) -> Result<LaunchReport> {
    let invocation = prepared.invocation_id.clone();
    let run_dir = tempfile::tempdir().context(
        "backend startup failure: cannot create ginux run directory (refusing host fallback)",
    )?;
    let result_path = run_dir.path().join("result.json");
    let config_path = prepared.staging_dir().join("config.toml");
    if !config_path.is_file() {
        bail!(
            "backend startup failure: invocation {invocation} has no staged config.toml \
             (refusing host fallback)"
        );
    }

    let domain_root = crate::inputs::prepare_root(prepared).map_err(|error| {
        anyhow!("backend startup failure: cannot prepare Ginux root for invocation {invocation}: {error:#} (refusing host fallback)")
    })?;

    let mut command = Command::new(launcher);
    command.process_group(0);
    let parent = unsafe { libc::getpid() };
    // Killing the coordinator must not orphan the supervisor. Ginux's
    // PTRACE_O_EXITKILL then tears down the supervised tasks.
    unsafe {
        command.pre_exec(move || {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::getppid() != parent {
                libc::_exit(125);
            }
            Ok(())
        });
    }
    let mut child = command
        .arg("launch")
        .arg(&config_path)
        .arg(domain_root.path())
        .arg("--result-file")
        .arg(&result_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| {
            anyhow!(
                "backend startup failure: cannot spawn ginux launcher {} for invocation \
                 {invocation}: {err:#} (refusing host fallback)",
                launcher.display(),
            )
        })?;

    let stdin = child.stdin.take().context(
        "backend startup failure: launcher stdin was not piped (refusing host fallback)",
    )?;
    let stdout = child.stdout.take().context(
        "backend startup failure: launcher stdout was not piped (refusing host fallback)",
    )?;
    let stderr = child.stderr.take().context(
        "backend startup failure: launcher stderr was not piped (refusing host fallback)",
    )?;
    let log_path = run_dir.path().join("worker-stderr.log");
    let drain = spawn_stderr_drain(stderr, log_path.clone());

    let pump = pump_protocol(
        &invocation,
        &prepared.operation,
        &prepared.recipe_digest,
        persist_ctx,
        options,
        stdin,
        stdout,
    );
    let stream = match pump {
        Ok(stream) => stream,
        Err(err) => {
            let exit = reap_launcher(&mut child);
            let drained = join_drain(drain);
            let file_note = read_result_file_note(&result_path);
            return Err(enrich_stream_error(
                &invocation,
                err,
                exit,
                &drained,
                file_note.as_deref(),
            ));
        }
    };

    drop(stream.stdin);
    let exit = reap_launcher(&mut child);
    let drained = join_drain(drain);

    let record = read_result_record(&result_path, &invocation)?;
    check_agreement(&invocation, &stream.result, &record)?;
    check_launcher_exit(&invocation, &stream.result, &record, exit)?;

    let log_tail = read_log_tail(&log_path);
    Ok(LaunchReport {
        result: stream.result,
        failure: stream.failure,
        checkpoints_acked: stream.checkpoints,
        generations: stream.generations,
        launcher_code: exit,
        result_outcome: record.outcome,
        result_code: record.code,
        result_signal: record.signal,
        result_generation: record.generation,
        log_total_bytes: drained.total_bytes,
        log_discarded_bytes: drained.discarded_bytes,
        log_tail,
    })
}

/// Completed worker stream: exactly one result plus the relayed checkpoints.
struct WorkerStream {
    result: ResultPayload,
    failure: Option<crate::worker::protocol::FailurePayload>,
    checkpoints: u64,
    generations: Vec<(u64, u64)>,
    stdin: DeadlinePipe<std::process::ChildStdin>,
}

/// Send the request, relay checkpoint→ack, and collect the exactly-once
/// result. Transport errors before the result frame are returned with a
/// `protocol` or `startup` category (see [`classify_stream_error`]); the
/// caller enriches them with launcher/result-file state.
fn pump_protocol(
    invocation: &str,
    operation: &str,
    recipe_digest: &str,
    persist_ctx: bool,
    options: &crate::worker::protocol::InvocationOptions,
    stdin: std::process::ChildStdin,
    stdout: std::process::ChildStdout,
) -> Result<WorkerStream> {
    let timeout = frame_timeout()?;
    let mut stdin = DeadlinePipe::new(stdin, timeout)?;
    let mut stdout = DeadlinePipe::new(stdout, timeout)?;
    let mut tx = FrameSender::new();
    let mut rx = FrameReceiver::new();

    tx.send(
        &mut stdin,
        &FramePayload::Request(RequestPayload {
            invocation_id: invocation.to_string(),
            operation: operation.to_string(),
            persist_ctx,
            recipe_digest: recipe_digest.to_string(),
            options: options.clone(),
        }),
    )
    .map_err(|err| classify_stream_error(invocation, "send request", err, true))?;
    stdin
        .flush()
        .map_err(|err| classify_stream_error(invocation, "flush request", err, true))?;

    let mut checkpoints: u64 = 0;
    let mut generations = Vec::new();
    let mut failure = None;
    let mut diagnostic_bytes = 0usize;
    let mut diagnostic_frames = 0usize;
    let mut diagnostic_truncated = false;
    let result = loop {
        stdout.reset(timeout);
        let frame = rx
            .recv(&mut stdout)
            .map_err(|err| classify_stream_error(invocation, "read worker frame", err, false))?;
        frame
            .check_direction(Direction::WorkerToHost)
            .map_err(|err| {
                anyhow!(
                    "backend protocol failure: invocation {invocation}: worker stream direction \
                 violation: {err} (refusing host fallback)"
                )
            })?;
        match frame.payload {
            FramePayload::Diagnostic(payload) => {
                use crate::worker::protocol::{MAX_DIAGNOSTIC_BYTES, MAX_DIAGNOSTIC_FRAMES};
                if diagnostic_truncated {
                    bail!(
                        "backend protocol failure: diagnostic after truncation (refusing host fallback)"
                    );
                }
                if payload.truncated {
                    if diagnostic_bytes != MAX_DIAGNOSTIC_BYTES
                        && diagnostic_frames != MAX_DIAGNOSTIC_FRAMES
                    {
                        bail!(
                            "backend protocol failure: premature diagnostic truncation (refusing host fallback)"
                        );
                    }
                    diagnostic_truncated = true;
                    let _ = std::io::stderr()
                        .write_all(b"recipe: diagnostic limit reached; remaining output drained\n");
                } else {
                    diagnostic_bytes += payload.data.len();
                    diagnostic_frames += 1;
                    if diagnostic_bytes > MAX_DIAGNOSTIC_BYTES
                        || diagnostic_frames > MAX_DIAGNOSTIC_FRAMES
                    {
                        bail!(
                            "backend protocol failure: diagnostic budget exceeded (refusing host fallback)"
                        );
                    }
                    // All human bytes go to CLI stderr, never to its JSON stream.
                    // Supervisor stderr is collected independently and never parsed.
                    let _ = std::io::stderr().write_all(&payload.data);
                }
            }
            FramePayload::Checkpoint(point) => {
                checkpoints += 1;
                if checkpoints > MAX_CHECKPOINTS {
                    bail!(
                        "backend protocol failure: invocation {invocation} sent more than \
                         {MAX_CHECKPOINTS} checkpoints (refusing host fallback)"
                    );
                }
                let seq = point.seq;
                generations.push((seq, UNGROUNDED_GENERATION));
                stdin.reset(timeout);
                tx.send(
                    &mut stdin,
                    &FramePayload::Ack(AckPayload {
                        seq,
                        generation: UNGROUNDED_GENERATION,
                    }),
                )
                .map_err(|err| {
                    classify_stream_error(invocation, "send checkpoint ack", err, false)
                })?;
                stdin.flush().map_err(|err| {
                    classify_stream_error(invocation, "flush checkpoint ack", err, false)
                })?;
            }
            FramePayload::Failure(payload) => {
                if failure.is_some() {
                    bail!(
                        "backend protocol failure: invocation {invocation} sent a second failure \
                         frame (at most one failure before the exactly-once result; refusing \
                         host fallback)"
                    );
                }
                failure = Some(payload);
            }
            FramePayload::Result(payload) => break payload,
            FramePayload::Request(_) | FramePayload::Ack(_) => {
                bail!(
                    "backend protocol failure: invocation {invocation} received a host-direction \
                     frame from the worker (refusing host fallback)"
                );
            }
        }
    };

    Ok(WorkerStream {
        result,
        failure,
        checkpoints,
        generations,
        stdin,
    })
}

/// One absolute deadline covers the complete frame, including partial headers,
/// payloads and blocked writes. Nonblocking descriptors prevent a poll/read race
/// from accidentally turning the next operation into an unbounded wait.
struct DeadlinePipe<T> {
    inner: T,
    deadline: Instant,
}

fn frame_timeout() -> Result<Duration> {
    match std::env::var("RECIPE_FRAME_TIMEOUT_SECS") {
        Err(std::env::VarError::NotPresent) => Ok(FRAME_TIMEOUT),
        value => {
            let seconds: u64 = value
                .context("invalid RECIPE_FRAME_TIMEOUT_SECS")?
                .parse()
                .context("RECIPE_FRAME_TIMEOUT_SECS must be an integer from 1 to 86400")?;
            if !(1..=86400).contains(&seconds) {
                bail!("RECIPE_FRAME_TIMEOUT_SECS must be from 1 to 86400");
            }
            Ok(Duration::from_secs(seconds))
        }
    }
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
        })
    }
    fn reset(&mut self, timeout: Duration) {
        self.deadline = Instant::now() + timeout;
    }
    fn wait(&self, events: i16) -> std::io::Result<()> {
        loop {
            let left = self.deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "complete worker frame deadline expired",
                ));
            }
            let mut poll = libc::pollfd {
                fd: self.inner.as_raw_fd(),
                events,
                revents: 0,
            };
            let millis = left.as_millis().clamp(1, i32::MAX as u128) as i32;
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
        }
    }
}
impl<T: AsRawFd + Read> Read for DeadlinePipe<T> {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        loop {
            self.wait(libc::POLLIN)?;
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
impl<T: AsRawFd + Write> Write for DeadlinePipe<T> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        loop {
            self.wait(libc::POLLOUT)?;
            match self.inner.write(bytes) {
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
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// Categorize a transport error: failures delivering the request mean the
/// launcher never came up as a worker endpoint (`startup`); anything later
/// on a live stream is `protocol`.
fn classify_stream_error(
    invocation: &str,
    what: &str,
    err: impl std::fmt::Display,
    before_request_delivered: bool,
) -> anyhow::Error {
    if before_request_delivered {
        anyhow!(
            "backend startup failure: invocation {invocation}: cannot {what} through launcher \
             (launcher cannot serve the worker transport; refusing host fallback): {err}"
        )
    } else {
        anyhow!(
            "backend protocol failure: invocation {invocation}: cannot {what} (refusing host \
             fallback): {err}"
        )
    }
}

/// Attach launcher exit, drained-log, and result-file state to a stream
/// error. A `supervisor-error` result file underneath a broken stream
/// reclassifies the failure as `startup` (the domain never came up); every
/// other broken stream stays `protocol` and never success.
fn enrich_stream_error(
    invocation: &str,
    err: anyhow::Error,
    exit: Option<i32>,
    drained: &DrainedLog,
    file_note: Option<&str>,
) -> anyhow::Error {
    let text = format!("{err:#}");
    if let Some(note) = file_note
        && note.contains("\"supervisor-error\"")
        && text.starts_with("backend protocol failure")
    {
        return anyhow!(
            "backend startup failure: invocation {invocation}: worker stream broke and the \
             supervisor reports it never started the domain: {text}; launcher exit \
             {}; log tail: {} (refusing host fallback)",
            exit_message(exit),
            drained.tail,
        );
    }
    anyhow!(
        "{text}; invocation {invocation} launcher exit {}; {}; log tail: {} (refusing host \
         fallback)",
        exit_message(exit),
        file_note.unwrap_or("no result file was produced"),
        drained.tail,
    )
}

fn exit_message(exit: Option<i32>) -> String {
    match exit {
        Some(code) => format!("code {code}"),
        None => "no exit code (killed or signaled)".to_string(),
    }
}

// ---------------------------------------------------------------------------
// Launcher shutdown
// ---------------------------------------------------------------------------

/// Reap the launcher: close of stdin already signaled EOF; wait for exit
/// with a grace period, then kill and reap. Returns the exit code when the
/// launcher exited normally.
fn reap_launcher(child: &mut Child) -> Option<i32> {
    let deadline = Instant::now() + SHUTDOWN_GRACE;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                // A malformed launcher may leave a child holding its pipes.
                unsafe {
                    libc::kill(-(child.id() as i32), libc::SIGKILL);
                }
                return status.code();
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    unsafe {
                        libc::kill(-(child.id() as i32), libc::SIGKILL);
                    }
                    child.kill().ok();
                    return match child.wait() {
                        Ok(status) => status.code(),
                        Err(_) => None,
                    };
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(_) => return None,
        }
    }
}

// ---------------------------------------------------------------------------
// Stderr drain (bounded log collection)
// ---------------------------------------------------------------------------

/// Outcome of the stderr drain thread.
struct DrainedLog {
    total_bytes: u64,
    discarded_bytes: u64,
    tail: String,
}

/// Drain launcher stderr to a log file with a retention cap, so a chatty
/// worker can never block on a full stderr pipe. Bytes past the cap are
/// still consumed (and counted) but discarded. The log is never parsed.
struct Drain {
    stop: Arc<AtomicBool>,
    thread: std::thread::JoinHandle<DrainedLog>,
}

fn spawn_stderr_drain(stderr: std::process::ChildStderr, log_path: PathBuf) -> Drain {
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
        let mut file = std::fs::File::create(&log_path).ok();
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
                        if out.write_all(&buf[..take]).is_err() {
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
        if let Some(mut out) = file {
            out.flush().ok();
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
fn join_drain(handle: Drain) -> DrainedLog {
    handle.stop.store(true, Ordering::Relaxed);
    handle.thread.join().unwrap_or(DrainedLog {
        total_bytes: 0,
        discarded_bytes: 0,
        tail: "<stderr drain thread failed>".to_string(),
    })
}

/// Read the tail of the retained stderr log for diagnostics (never parsed).
fn read_log_tail(log_path: &Path) -> String {
    let bytes = std::fs::read(log_path).unwrap_or_default();
    let start = bytes.len().saturating_sub(LOG_TAIL_BYTES);
    String::from_utf8_lossy(&bytes[start..]).into_owned()
}

// ---------------------------------------------------------------------------
// Result file (authoritative per §2)
// ---------------------------------------------------------------------------

/// Parsed supervisor result record.
struct ResultRecord {
    outcome: String,
    code: Option<i64>,
    signal: Option<u64>,
    generation: Option<u64>,
}

/// JSON value kind for diagnostics.
fn json_value_kind(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "a boolean",
        serde_json::Value::Number(_) => "a number",
        serde_json::Value::String(_) => "a string",
        serde_json::Value::Array(_) => "an array",
        serde_json::Value::Object(_) => "an object",
    }
}

/// Terse note about result-file state for stream-error diagnostics.
fn read_result_file_note(result_path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(result_path).ok()?;
    let short: String = text.chars().take(300).collect();
    Some(format!("result file present: {short}"))
}

/// Read and validate the supervisor result file. Missing or malformed is a
/// `protocol` failure, never success (§2).
fn read_result_record(result_path: &Path, invocation: &str) -> Result<ResultRecord> {
    let text = std::fs::read_to_string(result_path).map_err(|err| {
        anyhow!(
            "backend protocol failure: invocation {invocation} produced no result file at {}: \
             {err:#} (missing result file is never success; refusing host fallback)",
            result_path.display(),
        )
    })?;
    let value: serde_json::Value = serde_json::from_str(&text).map_err(|err| {
        anyhow!(
            "backend protocol failure: invocation {invocation} result file is not valid JSON: \
             {err:#} (malformed result file is never success; refusing host fallback)"
        )
    })?;
    let version = value.get("version").and_then(|v| v.as_u64());
    if version != Some(1) {
        bail!(
            "backend protocol failure: invocation {invocation} result file has unsupported \
             version {version:?} (want 1; refusing host fallback)"
        );
    }
    let outcome = value
        .get("outcome")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    if !matches!(outcome, "exited" | "signaled" | "supervisor-error") {
        bail!(
            "backend protocol failure: invocation {invocation} result file has unknown outcome \
             {outcome:?} (want exited|signaled|supervisor-error; refusing host fallback)"
        );
    }
    let field = |name: &str| match value.get(name) {
        None => Ok(None),
        Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::Number(n)) => n.as_i64().map(Some).ok_or_else(|| {
            anyhow!(
                "backend protocol failure: invocation {invocation} result file field {name:?} \
                 is not an integer (refusing host fallback)"
            )
        }),
        Some(other) => bail!(
            "backend protocol failure: invocation {invocation} result file field {name:?} is {}, \
             want an integer or null (refusing host fallback)",
            json_value_kind(other),
        ),
    };
    Ok(ResultRecord {
        outcome: outcome.to_string(),
        code: field("code")?,
        signal: field("signal")?.map(|n| n as u64),
        generation: field("generation")?.map(|n| n as u64),
    })
}

/// The stream result and the authoritative result file must agree. A worker
/// `ok` requires a clean `exited`/`0` record; a worker failure requires the
/// domain to have run it. Contradictions are `protocol` failures, never
/// success.
fn check_agreement(invocation: &str, result: &ResultPayload, record: &ResultRecord) -> Result<()> {
    if result.ok {
        if record.outcome == "supervisor-error" {
            bail!(
                "backend startup failure: invocation {invocation} worker reported success but the \
                 supervisor reports it never started the domain (refusing host fallback)"
            );
        }
        if record.outcome != "exited" || record.code != Some(0) {
            bail!(
                "backend protocol failure: invocation {invocation} worker reported success but \
                 the authoritative result is {}/{} (refusing host fallback)",
                record.outcome,
                record
                    .code
                    .map(|c| c.to_string())
                    .or_else(|| record.signal.map(|s| format!("signal {s}")))
                    .unwrap_or_else(|| "no status".to_string()),
            );
        }
        return Ok(());
    }
    if record.outcome == "supervisor-error" {
        bail!(
            "backend startup failure: invocation {invocation} worker reported failure and the \
             supervisor reports the domain never started (refusing host fallback)"
        );
    }
    Ok(())
}

/// The launcher must exit cleanly once the stream completes: a nonzero or
/// signaled launcher exit under a worker success contradicts the result file
/// (already checked) and is reported here with process detail. A launcher
/// that had to be killed after a completed stream is a `protocol` failure.
fn check_launcher_exit(
    invocation: &str,
    result: &ResultPayload,
    record: &ResultRecord,
    exit: Option<i32>,
) -> Result<()> {
    if result.ok {
        return Ok(());
    }
    if record.outcome == "signaled" {
        bail!(
            "backend workload failure: invocation {invocation} worker was terminated by signal {} \
             (refusing host fallback)",
            record.signal.map(|s| s.to_string()).unwrap_or_default(),
        );
    }
    if record.outcome == "exited" && record.code != Some(0) {
        let detail = result
            .failures
            .first()
            .map(|failure| {
                format!(
                    "phase={} reason={}: {}",
                    failure.phase, failure.reason, failure.message
                )
            })
            .unwrap_or_else(|| "worker reported failure without detail".to_string());
        bail!(
            "backend workload failure: invocation {invocation} worker exited with code {} \
             (launcher exit {}): {detail} (refusing host fallback)",
            record.code.unwrap_or(-1),
            exit_message(exit),
        );
    }
    Ok(())
}
