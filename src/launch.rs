//! Ginux subprocess launch, checkpoints, cancellation and diagnostics.
//!
//! [`run_domain`] is the R-host subprocess adapter: it spawns the configured
//! launcher (`GINUX_BIN`), hands it the prepared domain config
//! plus a fresh `--result-file` path, pumps the worker protocol over the
//! launcher's stdio, and maps outcomes to the interface §1 categories with
//! the result file authoritative (§2).
//!
//! Transport discipline (matches `docs/ginux-recipe-interfaces.md` §§2/4):
//!
//! - Launcher argv: `launch <config.toml> <prepared-rootfs> --result-file <fresh host path> --export-file <fresh tar path>`.
//!   The config file is the staged `config.toml` from [`PreparedInvocation`];
//!   the result file is a fresh path in a per-invocation run directory that
//!   the launcher creates exclusively (`O_EXCL` semantics belong to the
//!   launcher; a missing or malformed result file is a `protocol` failure,
//!   never success).
//! - Worker stdin/stdout are the opaque protocol pipes: the adapter sends one
//!   `request` then one `ack` per checkpoint and receives `checkpoint`s and
//!   exactly one `result`. Worker stderr (launcher
//!   stderr, which also carries `ginux:`-prefixed supervisor diagnostics) is
//!   drained to a log file with a byte cap and never parsed: log bytes only
//!   appear in human diagnostics, never as results.
//! - Acknowledged checkpoints record `{seq → generation}` in the returned
//!   report after the supervisor snapshot and matching state record are durable.
//!   Disabled persistence acknowledges with no generation: nothing will be recovered.
//!
//! Before launch, the input owner snapshots and verifies the selected assembled
//! rootfs tar, creates a fresh domain root, and copies the worker plus staged
//! inputs into `/inputs`. The root is private to this invocation and is kept
//! alive until the launcher is reaped.
//!
//! Every failure carries a [`crate::failure::Category`]. Storage joins retain
//! the last complete pair; cancellation is forwarded and reaped before
//! returning its distinct outcome.

use crate::ginux::{Outcome, ResultRecord};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
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

/// Startup and complete-frame transport bound. Once the worker has started,
/// a silent phase can run until the separate total execution deadline; the
/// first byte of each frame starts a bounded complete-frame read.
const FRAME_TIMEOUT: Duration = Duration::from_secs(300);

/// Bound on checkpoint chatter: more than this many checkpoints in one
/// invocation is a `protocol` failure, never unbounded acking.
const MAX_CHECKPOINTS: u64 = 1024;

/// Bound on log collection: launcher/worker stderr beyond this many bytes is
/// still drained (so a chatty worker never blocks on a full pipe) but not
/// retained.
const LOG_CAP_BYTES: u64 = 16 * 1024 * 1024;

/// Grace for teardown after failure/cancellation. Successful final export uses
/// the remaining supervised execution deadline instead.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(10);

/// Bytes of log tail attached to failure diagnostics.
const LOG_TAIL_BYTES: usize = 4096;


/// One executable operation, as the coordinator runs it.
pub(crate) struct Invocation<'a> {
    pub operation: crate::worker::protocol::Operation,
    /// Present exactly for cleanup.
    pub cleanup_reason: Option<&'a str>,
    pub recipe: &'a Path,
    pub search_path: Option<&'a Path>,
    pub persist_state: bool,
    pub options: crate::worker::protocol::InvocationOptions,
    pub inputs: &'a [crate::inputs::ExtraInput],
    /// Lock and saved-state scope key.
    pub destination: &'a Path,
    pub state_dir: Option<&'a Path>,
}

/// Outcome of one supervised domain run.
#[derive(Debug)]
pub(crate) struct LaunchReport {
    /// The exactly-once worker result frame.
    pub result: ResultPayload,
    /// Checkpoints acked during the run.
    pub checkpoints_acked: u64,
    /// Invocation-local `{checkpoint seq → acked generation}` association
    /// (§4).
    pub generations: Vec<(u64, Option<u64>)>,
    /// Launcher exit code when it exited normally (`None` when killed or
    /// signaled).
    pub launcher_code: Option<i32>,
    /// Authoritative result-file outcome (`exited`, `signaled`, or
    /// `supervisor-error`).
    pub result_outcome: crate::ginux::Outcome,
    /// Authoritative result-file exit code (workload exit when `exited`).
    pub result_code: Option<i64>,
    /// Total launcher/worker stderr bytes observed (retained portion is
    /// capped at [`LOG_CAP_BYTES`]).
    pub log_total_bytes: u64,
    /// Launcher/worker stderr bytes retained beyond the cap (drained and
    /// discarded).
    pub log_discarded_bytes: u64,
}

/// Run the prepared invocation through the launcher and pump the worker
/// protocol to exactly one result.
///
/// Restore matching state/files and acknowledge new checkpoints only after both
/// are durable. Disabled persistence acknowledges without a generation. No path
/// here evaluates recipe code on the host.
pub(crate) fn run_domain(
    prepared: &PreparedInvocation,
    invocation: &Invocation<'_>,
    launcher: &Path,
) -> Result<LaunchReport> {
    let state_dir = invocation.state_dir;
    let diagnostics = crate::retained::state_root(state_dir)
        .and_then(|root| crate::invocation_log::InvocationLog::new(&root, &prepared.invocation_id))
        .map_err(|error| crate::failure::Category::Storage.wrap(error, "cannot create the invocation log"))?;
    let outcome = run_domain_inner(prepared, invocation, launcher, &diagnostics);
    use crate::invocation_log::Status;
    let (status, details) = match &outcome {
        Ok(report) => (
            if report.result.ok {
                Status::Succeeded
            } else {
                Status::WorkloadFailed
            },
            serde_json::json!({
                "worker": report.result, "checkpoints": report.generations,
                "supervisor_outcome": report.result_outcome.as_str(), "supervisor_code": report.result_code,
                "log_bytes": report.log_total_bytes, "log_discarded_bytes": report.log_discarded_bytes}),
        ),
        Err(error) => (
            if crate::failure::Category::of(error) == Some(crate::failure::Category::Cancelled)
                || crate::cancellation::signal() != 0
            {
                Status::Cancelled
            } else {
                Status::Failed
            },
            serde_json::json!({"error": format!("{error:#}")}),
        ),
    };
    diagnostics.record(status, details)?;
    outcome
}

fn run_domain_inner(
    prepared: &PreparedInvocation,
    invocation: &Invocation<'_>,
    launcher: &Path,
    diagnostics: &crate::invocation_log::InvocationLog,
) -> Result<LaunchReport> {
    let Invocation {
        operation,
        cleanup_reason,
        recipe,
        persist_state,
        ref options,
        destination,
        state_dir,
        ..
    } = *invocation;
    let invocation = prepared.invocation_id.clone();
    let timeout = frame_timeout().map_err(|error| crate::failure::Category::Startup.wrap(error, "invalid frame timeout"))?;
    let _destination_lock = crate::retained::lock_destination(destination, timeout)
        .map_err(|error| crate::failure::Category::Storage.wrap(error, "cannot lock the destination"))?;
    let run_dir = crate::temporary::TemporaryDirectory::new().context(crate::failure::Category::Startup.error("cannot create ginux run directory"))?;
    let result_path = run_dir.path().join("result.json");
    let export_path = run_dir.path().join("export.tar");
    let config_path = prepared.config_path();

    let domain_root = crate::inputs::prepare_root(prepared).map_err(|error| {
        anyhow::Error::new(crate::failure::Category::Startup.error(format!("cannot prepare Ginux root for invocation {invocation}: {error:#}")))
    })?;

    let mut retained = if persist_state {
        Some(crate::retained::Retained::open(
            state_dir,
            destination,
            recipe,
            prepared,
            &domain_root.worker_digest,
            options,
        )
        .map_err(|error| crate::failure::Category::Storage.wrap(error, "cannot open saved state"))?)
    } else {
        None
    };
    let restore_path = run_dir.path().join("restore.tar");
    let mut restore = retained
        .as_ref()
        .map(|state| state.restore_input(&restore_path))
        .transpose()
        .map_err(|error| crate::failure::Category::Storage.wrap(error, "cannot restore saved files"))?
        .unwrap_or(false);
    if !restore {
        restore = crate::inputs::seed_target(prepared, domain_root.path(), &restore_path)
            .map_err(|error| {
                crate::failure::Category::Startup.wrap(error, "cannot seed the requested target view")
            })?;
    }
    crate::cancellation::check()?;
    let (mut control, control_server) = if persist_state {
        let (client, server) = UnixStream::pair()?;
        (Some(DeadlinePipe::new(client, timeout)?), Some(server))
    } else {
        (None, None)
    };
    let control_fd = control_server.as_ref().map(AsRawFd::as_raw_fd);

    let mut command = Command::new(launcher);
    command.process_group(0);
    let parent = unsafe { libc::getpid() };
    let leases = [
        prepared.lease_fd(),
        domain_root.lease_fd(),
        run_dir.lease_fd(),
        diagnostics.lease_fd(),
    ];
    // Killing the coordinator must not orphan the supervisor. Ginux's
    // PTRACE_O_EXITKILL then tears down the supervised tasks.
    unsafe {
        command.pre_exec(move || {
            for fd in leases.into_iter().chain(control_fd) {
                let flags = libc::fcntl(fd, libc::F_GETFD);
                if flags < 0 || libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::getppid() != parent {
                libc::_exit(125);
            }
            Ok(())
        });
    }
    command
        .arg("launch")
        .arg(&config_path)
        .arg(domain_root.path())
        .arg("--result-file")
        .arg(&result_path)
        .arg("--export-file")
        .arg(&export_path);
    if let Some(fd) = control_fd {
        command.arg("--control-fd").arg(fd.to_string());
    }
    if restore {
        command.arg("--restore-file").arg(&restore_path);
    }
    let mut worker_diagnostics = std::fs::File::create(diagnostics.file(crate::invocation_log::WORKER_LOG))?;
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| {
            anyhow::Error::new(crate::failure::Category::Startup.error(format!("cannot spawn ginux launcher {} for invocation \
                 {invocation}: {err:#}",
                launcher.display(),
            )))
        })?;

    drop(control_server);
    let stdin = child.stdin.take().context(crate::failure::Category::Startup.error("launcher stdin was not piped"))?;
    let stdout = child.stdout.take().context(crate::failure::Category::Startup.error("launcher stdout was not piped"))?;
    let stderr = child.stderr.take().context(crate::failure::Category::Startup.error("launcher stderr was not piped"))?;
    let log_path = diagnostics.file(crate::invocation_log::SUPERVISOR_LOG);
    let drain = spawn_stderr_drain(stderr, log_path.clone());

    let mut worker_started = false;
    let request = RequestPayload {
        invocation_id: invocation.clone(),
        operation,
        persist_state,
        cleanup_reason: cleanup_reason.map(str::to_owned),
        options: options.clone(),
        restored_states: retained
            .as_ref()
            .map(|state| state.states().clone())
            .unwrap_or_default(),
    };
    let pump = pump_protocol(
        request,
        stdin,
        stdout,
        &mut retained,
        &mut control,
        &mut worker_diagnostics,
        &mut worker_started,
        timeout,
    );
    let mut stream = match pump {
        Ok(stream) => stream,
        Err(err) => {
            let exit = reap_launcher(&mut child, Instant::now() + SHUTDOWN_GRACE)
                .ok()
                .flatten();
            let _ = worker_diagnostics.sync_all();
            let drained = join_drain(drain);
            return Err(enrich_stream_error(
                &invocation,
                err,
                exit,
                &drained,
                &result_path,
                worker_started,
            ));
        }
    };

    drop(stream.stdin);
    // The worker result precedes the supervisor's final archive export. Large
    // valid exports still belong to execution, not short error teardown.
    let reaped = reap_launcher(&mut child, stream.execution_deadline);
    let drained = join_drain(drain);
    worker_diagnostics.sync_all()?;
    let exit = reaped?;

    let record = crate::ginux::ResultRecord::read(&result_path, &invocation)?;
    check_agreement(&invocation, &stream.result, &record)?;
    check_launcher_exit(&invocation, &stream.result, &record, exit)?;
    if stream.result.ok {
        if let Some(state) = retained.as_mut() {
            let runtime_state = stream
                .result
                .ctx_summary
                .get("state")
                .filter(|value| value.is_object())
                .context(crate::failure::Category::Protocol.error(
                    "successful resumable recipe did not return ctx.state",
                ))?
                .clone();
            state.finish(&export_path, runtime_state).map_err(|error| {
                crate::failure::Category::Storage.wrap(error, "cannot persist final state/files pair")
            })?;
        }
        crate::publication::publish(
            &invocation,
            &export_path,
            &mut stream.result.ctx_summary,
            &options.execution,
        )
        .context(crate::failure::Category::Storage.error("cannot publish completed domain outputs"))?;
    }

    Ok(LaunchReport {
        result: stream.result,
        checkpoints_acked: stream.checkpoints,
        generations: stream.generations,
        launcher_code: exit,
        result_outcome: record.outcome,
        result_code: record.code,
        log_total_bytes: drained.total_bytes,
        log_discarded_bytes: drained.discarded_bytes,
    })
}

/// Completed worker stream: exactly one result plus the relayed checkpoints.
struct WorkerStream {
    result: ResultPayload,
    checkpoints: u64,
    generations: Vec<(u64, Option<u64>)>,
    stdin: DeadlinePipe<std::process::ChildStdin>,
    execution_deadline: Instant,
}

/// Send the request, relay checkpoint→ack, and collect the exactly-once
/// result. Transport errors before the result frame are returned with a
/// `protocol` or `startup` category (see [`classify_stream_error`]); the
/// caller enriches them with launcher/result-file state.
fn pump_protocol(
    request: RequestPayload,
    stdin: std::process::ChildStdin,
    stdout: std::process::ChildStdout,
    retained: &mut Option<crate::retained::Retained>,
    control: &mut Option<DeadlinePipe<UnixStream>>,
    worker_diagnostics: &mut std::fs::File,
    worker_started: &mut bool,
    timeout: Duration,
) -> Result<WorkerStream> {
    let mut stdin = DeadlinePipe::new(stdin, timeout)?;
    let mut stdout = DeadlinePipe::new(stdout, timeout)?;
    let mut tx = FrameSender::new();
    let mut rx = FrameReceiver::new();
    let invocation_id = request.invocation_id.clone();
    let invocation = invocation_id.as_str();
    let execution_deadline =
        Instant::now() + Duration::from_secs(request.options.execution.timeout_secs);

    tx.send(&mut stdin, &FramePayload::Request(request))
    .map_err(|err| classify_stream_error(invocation, "send request", err, true))?;
    stdin
        .flush()
        .map_err(|err| classify_stream_error(invocation, "flush request", err, true))?;

    let mut checkpoints: u64 = 0;
    let mut generations = Vec::new();
    let mut diagnostic_budget = crate::worker::protocol::DiagnosticBudget::default();
    let result = loop {
        if Instant::now() >= execution_deadline {
            bail!(crate::failure::Category::Deadline.error("supervised execution deadline expired"));
        }
        stdout.begin_frame(
            if *worker_started {
                execution_deadline.saturating_duration_since(Instant::now())
            } else {
                timeout
            },
            timeout,
            execution_deadline,
        );
        let frame = rx
            .recv(&mut stdout)
            .map_err(|err| classify_stream_error(invocation, "read worker frame", err, false))?;
        frame
            .check_direction(Direction::WorkerToHost)
            .map_err(|err| {
                anyhow::Error::new(crate::failure::Category::Protocol.error(format!("invocation {invocation}: worker stream direction \
                 violation: {err}"
                )))
            })?;
        *worker_started = true;
        match frame.payload {
            FramePayload::Diagnostic(payload) => {
                diagnostic_budget.accept(&payload).map_err(|violation| {
                    crate::failure::Category::Protocol
                        .error(violation.to_string())
                })?;
                if payload.truncated {
                    crate::diagnostics::line(
                        "recipe: diagnostic limit reached; remaining output drained",
                    );
                } else {
                    // All human bytes go to CLI stderr, never to its JSON stream.
                    // Supervisor stderr is collected independently and never parsed.
                    worker_diagnostics.write_all(&payload.data)?;
                    let _ = crate::diagnostics::write(
                        crate::diagnostics::Stream::Recipe,
                        &payload.data,
                    );
                }
            }
            FramePayload::Checkpoint(point) => {
                checkpoints += 1;
                if checkpoints > MAX_CHECKPOINTS {
                    bail!(crate::failure::Category::Protocol.error(format!("invocation {invocation} sent more than \
                         {MAX_CHECKPOINTS} checkpoints"
                    )));
                }
                let seq = point.seq;
                if seq != checkpoints {
                    bail!(crate::failure::Category::Protocol.error(format!(
                        "invocation {invocation} sent checkpoint {seq} where {checkpoints} was due"
                    )));
                }
                let generation = if let Some(state) = retained.as_mut() {
                    let control = control.as_mut().context(
                        crate::failure::Category::Startup.error("missing snapshot control socket"),
                    )?;
                    Some(capture_checkpoint(state, control, &point, timeout).map_err(|error| {
                        crate::failure::Category::Storage
                            .wrap(error, format!("cannot keep checkpoint {seq}"))
                    })?)
                } else {
                    None
                };
                generations.push((seq, generation));
                stdin.reset(timeout);
                tx.send(
                    &mut stdin,
                    &FramePayload::Ack(AckPayload { seq, generation }),
                )
                .map_err(|err| {
                    classify_stream_error(invocation, "send checkpoint ack", err, false)
                })?;
                stdin.flush().map_err(|err| {
                    classify_stream_error(invocation, "flush checkpoint ack", err, false)
                })?;
            }
            FramePayload::Result(payload) => break payload,
            FramePayload::Request(_) | FramePayload::Ack(_) => {
                bail!(crate::failure::Category::Protocol.error(format!("invocation {invocation} received a host-direction \
                     frame from the worker"
                )));
            }
        }
    };

    Ok(WorkerStream {
        result,
        checkpoints,
        generations,
        stdin,
        execution_deadline,
    })
}

fn capture_checkpoint(
    retained: &mut crate::retained::Retained,
    control: &mut DeadlinePipe<UnixStream>,
    point: &crate::worker::protocol::CheckpointPayload,
    timeout: Duration,
) -> Result<u64> {
    let pending = retained.begin()?;
    control.reset(timeout);
    crate::ginux::request_snapshot(control, pending.generation, &pending.path)?;
    retained
        .commit(pending, &point.recipe, point.state.clone())
        .context("cannot persist checkpoint state/files pair")
}

/// One absolute deadline covers the complete frame, including partial headers,
/// payloads and blocked writes. Nonblocking descriptors prevent a poll/read race
/// from accidentally turning the next operation into an unbounded wait.
struct DeadlinePipe<T> {
    inner: T,
    deadline: Instant,
    frame_start: Option<(Duration, Instant)>,
    execution_deadline: Option<Instant>,
}

fn frame_timeout() -> Result<Duration> {
    match std::env::var("RECIPE_FRAME_TIMEOUT_SECS") {
        Err(std::env::VarError::NotPresent) => Ok(FRAME_TIMEOUT),
        value => {
            let seconds: u64 = value
                .context("invalid RECIPE_FRAME_TIMEOUT_SECS")?
                .parse()
                .ok()
                .filter(|seconds| (1..=crate::domain::MAX_TIMEOUT_SECS).contains(seconds))
                .with_context(|| {
                    format!(
                        "RECIPE_FRAME_TIMEOUT_SECS must be an integer from 1 to {}",
                        crate::domain::MAX_TIMEOUT_SECS
                    )
                })?;
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
            frame_start: None,
            execution_deadline: None,
        })
    }
    fn begin_frame(&mut self, idle: Duration, frame: Duration, execution_deadline: Instant) {
        self.deadline = (Instant::now() + idle).min(execution_deadline);
        self.frame_start = Some((frame, execution_deadline));
        self.execution_deadline = Some(execution_deadline);
    }
    fn reset(&mut self, timeout: Duration) {
        self.deadline = Instant::now() + timeout;
    }
    fn wait(&self, events: i16) -> std::io::Result<()> {
        loop {
            crate::cancellation::check_io()?;
            let left = self.deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    if self
                        .execution_deadline
                        .is_some_and(|deadline| Instant::now() >= deadline)
                    {
                        "supervised execution deadline expired"
                    } else {
                        "complete worker frame deadline expired"
                    },
                ));
            }
            let mut poll = libc::pollfd {
                fd: self.inner.as_raw_fd(),
                events,
                revents: 0,
            };
            let millis = left.as_millis().clamp(1, 100) as i32;
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
                Ok(count) => {
                    if count != 0 {
                        if let Some((frame, execution)) = self.frame_start.take() {
                            self.deadline = (Instant::now() + frame).min(execution);
                        }
                    }
                    return Ok(count);
                }
                Err(error) => return Err(error),
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
    if crate::cancellation::signal() != 0 {
        return crate::cancellation::cancelled().into();
    }
    if before_request_delivered {
        anyhow::Error::new(crate::failure::Category::Startup.error(format!("invocation {invocation}: cannot {what} through launcher \
             (launcher cannot serve the worker transport): {err}"
        )))
    } else {
        anyhow::Error::new(crate::failure::Category::Protocol.error(format!("invocation {invocation}: cannot {what}: {err}"
        )))
    }
}

/// Attach launcher exit, drained-log, and result-file state to a stream
/// error. A `supervisor-error` result file underneath a broken stream
/// distinguishes an unconfirmed startup from a supervisor failure after worker traffic; every
/// other broken stream stays `protocol` and never success.
fn enrich_stream_error(
    invocation: &str,
    err: anyhow::Error,
    exit: Option<i32>,
    drained: &DrainedLog,
    result_path: &Path,
    worker_started: bool,
) -> anyhow::Error {
    let text = format!("{err:#}");
    let category = crate::failure::Category::of(&err);
    let file_note = crate::ginux::result_note(result_path);
    if crate::ginux::peek_outcome(result_path) == Some(crate::ginux::Outcome::SupervisorError)
        && category == Some(crate::failure::Category::Protocol)
    {
        let (category, timing) = if worker_started {
            (
                crate::failure::Category::Supervisor,
                "after worker execution started",
            )
        } else {
            (
                crate::failure::Category::Startup,
                "before worker startup was confirmed",
            )
        };
        return category
            .error(format!(
                "invocation {invocation}: supervisor aborted {timing}: {text}; launcher exit {}; log tail: {}",
                exit_message(exit),
                drained.tail
            ))
            .into();
    }
    let detail = format!(
        "; invocation {invocation} launcher exit {}; {}; log tail: {}",
        exit_message(exit),
        file_note.as_deref().unwrap_or("no result file was produced"),
        drained.tail,
    );
    match category {
        // Keep the category typed while extending its message.
        Some(category) => {
            let message = text
                .strip_prefix(category.prefix())
                .and_then(|rest| rest.strip_prefix(": "))
                .unwrap_or(&text);
            category.error(format!("{message}{detail}")).into()
        }
        None => anyhow!("{text}{detail}"),
    }
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

/// Await actual launcher completion, including its final export. Failure and
/// interruption use short teardown; successful work keeps its execution budget.
fn reap_launcher(child: &mut Child, mut deadline: Instant) -> Result<Option<i32>> {
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
                bail!(crate::failure::Category::Deadline.error("supervised execution deadline expired during finalization"));
            }
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

// ---------------------------------------------------------------------------
// Result file (authoritative per §2)
// ---------------------------------------------------------------------------

/// The stream result and the authoritative result file must agree. A worker
/// `ok` requires a clean `exited`/`0` record; a worker failure requires the
/// domain to have run it. Contradictions are `protocol` failures, never
/// success.
fn check_agreement(invocation: &str, result: &ResultPayload, record: &ResultRecord) -> Result<()> {
    if result.ok {
        if record.outcome == Outcome::SupervisorError {
            bail!(crate::failure::Category::Supervisor.error(format!("invocation {invocation} worker reported success but the \
                 supervisor failed during finalization (refusing publication)"
            )));
        }
        if record.outcome != Outcome::Exited || record.code != Some(0) {
            bail!(crate::failure::Category::Protocol.error(format!("invocation {invocation} worker reported success but \
                 the authoritative result is {}/{}",
                record.outcome,
                record
                    .code
                    .map(|c| c.to_string())
                    .or_else(|| record.signal.map(|s| format!("signal {s}")))
                    .unwrap_or_else(|| "no status".to_string()),
            )));
        }
        return Ok(());
    }
    if record.outcome == Outcome::SupervisorError {
        bail!(crate::failure::Category::Supervisor.error(format!("invocation {invocation} worker reported failure and the \
             supervisor also failed during finalization (refusing publication)"
        )));
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
        if exit != Some(0) {
            bail!(crate::failure::Category::Protocol.error(format!("invocation {invocation} worker reported success but launcher exited with {} (refusing publication)",
                exit_message(exit),
            )));
        }
        return Ok(());
    }
    if record.outcome == Outcome::Signaled {
        bail!(crate::failure::Category::Workload.error(format!("invocation {invocation} worker was terminated by signal {}",
            record.signal.map(|s| s.to_string()).unwrap_or_default(),
        )));
    }
    if record.outcome == Outcome::Exited && record.code != Some(0) {
        let detail = result
            .failure
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_else(|| "worker reported failure without detail".to_string());
        bail!(crate::failure::Category::Workload.error(format!("invocation {invocation} worker exited with code {} \
             (launcher exit {}): {detail}",
            record.code.unwrap_or(-1),
            exit_message(exit),
        )));
    }
    Ok(())
}
