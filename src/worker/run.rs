//! Worker entry point: run the engine behind the coordinator protocol
//! ([`crate::worker::protocol`]).
//!
//! The worker is the same `recipe` binary entered with a private argv marker (see
//! the dispatch in `src/bin/`). It reads exactly one request frame from
//! stdin, runs the requested operation, reporting each recovery point as a
//! checkpoint instead of rewriting recipe sources, then terminates the stream
//! with exactly one result frame on stdout and exits 0.
//!
//! Transport discipline (matches `docs/ginux-recipe-interfaces.md` §2/§4):
//!
//! - stdin carries request/ack frames. Stdout carries serialized diagnostic,
//!   checkpoint and result frames. Child bytes and engine narration use the
//!   installed diagnostic sink; startup/audit details stay on stderr. No
//!   recipe text or child bytes are written as raw protocol output.
//! - Every checkpoint blocks for its `ack`, at most for the invocation's
//!   execution deadline. The host owns the `{seq → generation}` association;
//!   the worker only waits for it and logs the generation to stderr.
//! - A failed result says what failed ([`FailureKind`]): a phase (with its
//!   name), the recipe operation outside a phase, or the request itself.
//!
//! The coordinator prepares immutable inputs and restores the matching filesystem.
//! The worker initializes current facts, restores only ctx.state, and reports
//! per-recipe state at checkpoints. It never rewrites recipe sources.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};

use crate::core::executor::{EngineError, ExecutionContext, Load};
use crate::worker::protocol::{
    CheckpointPayload, Direction, FailureInfo, FailureKind, FramePayload, FrameReceiver,
    FrameSender, Operation, ProgressPayload, RequestPayload, ResultPayload,
};

struct WorkerChannel {
    sender: FrameSender,
    budget: crate::worker::protocol::DiagnosticBudget,
    /// Checkpoint acks are bounded by the invocation's execution deadline:
    /// the host never takes longer than that to answer or to stop the run.
    ack_timeout: Duration,
    failed: bool,
    terminal: bool,
}
type SharedChannel = Arc<Mutex<WorkerChannel>>;

fn install_diagnostics(channel: &SharedChannel) -> Result<()> {
    let channel = Arc::clone(channel);
    crate::diagnostics::install(Box::new(move |stream, mut bytes| {
        let mut state = channel
            .lock()
            .map_err(|_| std::io::Error::other("worker channel poisoned"))?;
        if state.failed || state.terminal {
            return Err(std::io::Error::other("worker channel is closed"));
        }
        let mut output = std::io::stdout();
        while !bytes.is_empty() {
            let Some(payload) = state.budget.next(stream.into(), &mut bytes) else {
                break;
            };
            if let Err(error) = state
                .sender
                .send(&mut output, &FramePayload::Diagnostic(payload))
            {
                state.failed = true;
                return Err(std::io::Error::other(error));
            }
            if let Err(error) = output.flush() {
                state.failed = true;
                return Err(error);
            }
        }
        Ok(())
    }))
}

fn install_progress(channel: &SharedChannel) {
    let channel = Arc::clone(channel);
    crate::core::output::install_progress_sink(Box::new(move |report| {
        let Ok(mut state) = channel.lock() else {
            return;
        };
        if state.failed || state.terminal {
            return;
        }
        let payload = FramePayload::Progress(ProgressPayload {
            recipe: report.recipe.clone(),
            phase: report.phase.clone(),
            what: report.what.clone(),
            done: report.done,
            total: report.total,
        });
        let mut output = std::io::stdout();
        // Progress is best effort: a failed write is left for the next
        // diagnostic or the result to report.
        if state.sender.send(&mut output, &payload).is_err() || output.flush().is_err() {
            state.failed = true;
        }
    }));
}

fn request_failure(message: String) -> FailureInfo {
    FailureInfo {
        kind: FailureKind::Request,
        phase: None,
        message,
    }
}

fn engine_failure(error: EngineError) -> FailureInfo {
    match error {
        EngineError::Recipe(error) => FailureInfo {
            kind: FailureKind::Recipe,
            phase: None,
            message: format!("{error:#}"),
        },
        EngineError::Phase { phase, error } => FailureInfo {
            kind: FailureKind::Phase,
            phase: Some(phase),
            message: format!("{error:#}"),
        },
    }
}

/// Worker endpoint for the private coordinator-supplied binary command.
///
/// Reads one request frame, runs the engine, sends `failure` (on error) and
/// exactly one `result`, and returns `Ok` on the protocol path (binary exits
/// 0). Transport collapse (unwritable stdout) returns `Err` (binary exits
/// nonzero; the host sees a worker exit without `result`).
pub fn run_worker() -> Result<()> {
    let (invocation_id, host_entry) = ensure_internal_entry()?;

    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let stdout = std::io::stdout();
    let mut output = stdout;
    let mut rx = FrameReceiver::new();
    let mut tx = FrameSender::new();

    // (1) One request frame. When the request itself is unusable, still try to
    // terminate the stream so the host does not hang on a silent worker.
    let request = match recv_request(&mut rx, &mut input) {
        Ok(req) => req,
        Err(err) => {
            let failure = request_failure(format!("bad request: {err:#}"));
            return send_result(
                &mut tx,
                &mut output,
                false,
                Some(failure),
                serde_json::json!({}),
            );
        }
    };
    if request.invocation_id != invocation_id || request.host.is_some() != host_entry {
        let failure =
            request_failure("request invocation id or mode does not match worker bootstrap".into());
        return send_result(
            &mut tx,
            &mut output,
            false,
            Some(failure),
            serde_json::json!({}),
        );
    }
    crate::diagnostics::trace(&format!(
        "worker: invocation={} operation={} persist_state={} recipe={}",
        request.invocation_id,
        request.operation,
        request.persist_state,
        crate::domain::MAIN_RECIPE
    ));

    // A host-mode worker sets up its confinement before any recipe code:
    // its directories, its network grant, and a kernel filter that refuses
    // to start any program at all.
    if let Some(host) = &request.host
        && let Err(error) = enter_host_mode(host)
    {
        let failure = request_failure(format!("cannot enter host mode: {error:#}"));
        return send_result(
            &mut tx,
            &mut output,
            false,
            Some(failure),
            serde_json::json!({}),
        );
    }

    let channel = Arc::new(Mutex::new(WorkerChannel {
        sender: tx,
        budget: Default::default(),
        ack_timeout: Duration::from_secs(request.options.execution.timeout_secs),
        failed: false,
        terminal: false,
    }));
    install_diagnostics(&channel)?;
    install_progress(&channel);
    crate::core::output::set_machine_events(request.options.execution.machine_events);
    crate::diagnostics::line("recipe: managed worker started");
    let outcome = run_operation(&request, &mut rx, &mut input, &channel, &mut output);
    crate::core::output::flush_progress();
    let mut channel = channel
        .lock()
        .map_err(|_| anyhow!("worker channel poisoned"))?;
    channel.terminal = true;
    if channel.failed {
        bail!("worker diagnostic transport failed");
    }
    let tx = &mut channel.sender;
    match outcome {
        Ok(summary) => send_result(tx, &mut output, true, None, summary),
        Err(failure) => send_result(tx, &mut output, false, Some(failure), serde_json::json!({})),
    }
}

/// Require the private argv entry, the coordinator's bootstrap flag, and an
/// invocation id. Public backend selection never uses these environment values.
/// Also says whether the coordinator started a host-mode worker.
fn ensure_internal_entry() -> Result<(String, bool)> {
    let args = || std::env::args_os().skip(1);
    let host = args().eq(crate::domain::WORKER_HOST_ARGS.map(std::ffi::OsString::from));
    if !(host || args().eq(crate::domain::WORKER_ARGS.map(std::ffi::OsString::from)))
        || std::env::var(crate::domain::ENV_WORKER).as_deref()
            != Ok(crate::domain::ENV_WORKER_VALUE)
    {
        bail!("private worker entry requires coordinator bootstrap arguments");
    }
    let id = std::env::var(crate::domain::ENV_INVOCATION)
        .context("private worker entry requires RECIPE_INVOCATION")?;
    if !crate::domain::valid_name(&id) {
        bail!("private worker entry has an invalid invocation id");
    }
    Ok((id, host))
}

/// Take the host run's directories and network grant, then forbid `execve`.
fn enter_host_mode(host: &crate::worker::protocol::HostRun) -> Result<()> {
    shed_privileges()?;
    let root = PathBuf::from(&host.root);
    for directory in [
        crate::domain::INPUTS,
        crate::domain::BUILD,
        crate::domain::OUT,
    ] {
        let path = root.join(directory.trim_start_matches('/'));
        anyhow::ensure!(
            std::fs::symlink_metadata(&path).is_ok_and(|meta| meta.is_dir()),
            "host directory {} is missing",
            path.display()
        );
    }
    crate::domain::set_host_root(root)?;
    crate::helpers::acquire::net::install(&host.network, host.network_max_bytes)?;
    forbid_programs()
}

/// The worker's own part of the host run's identity rule (the coordinator
/// already set it up before `exec`, `crate::host`): a host worker never runs
/// as root, holds no capability (ambient, effective, permitted or
/// inheritable), cannot be traced or dumped by other processes of its user,
/// and writes no core file.
fn shed_privileges() -> Result<()> {
    let (mut real, mut effective, mut saved) = (0, 0, 0);
    if unsafe { libc::getresuid(&mut real, &mut effective, &mut saved) } != 0 {
        return Err(std::io::Error::last_os_error()).context("cannot read the worker's identity");
    }
    anyhow::ensure!(
        real != 0 && effective != 0 && saved != 0,
        "a host worker must not run as root (the coordinator starts it as an unprivileged user)"
    );
    unsafe {
        libc::prctl(
            libc::PR_CAP_AMBIENT,
            libc::PR_CAP_AMBIENT_CLEAR_ALL as libc::c_ulong,
            0,
            0,
            0,
        );
    }
    crate::host::clear_capabilities().context("cannot drop the worker's capabilities")?;
    watch_coordinator()?;
    if unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) } != 0 {
        return Err(std::io::Error::last_os_error()).context("cannot make the worker non-dumpable");
    }
    let none = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    if unsafe { libc::setrlimit(libc::RLIMIT_CORE, &none) } != 0 {
        return Err(std::io::Error::last_os_error()).context("cannot disable core files");
    }
    Ok(())
}

/// End the worker as soon as the coordinator that started it is gone, on two
/// independent paths: the parent-death signal, armed again here (the
/// coordinator armed it before `exec`, but a secure exec, such as an SELinux
/// domain transition, clears it), and a thread that checks every quarter
/// second that the worker's parent is still that coordinator.
fn watch_coordinator() -> Result<()> {
    let coordinator: libc::pid_t = std::env::var(crate::domain::ENV_COORDINATOR)
        .ok()
        .and_then(|pid| pid.parse().ok())
        .context("a host worker needs its coordinator's pid")?;
    if unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) } != 0 {
        return Err(std::io::Error::last_os_error()).context("cannot arm the parent-death signal");
    }
    if unsafe { libc::getppid() } != coordinator {
        unsafe { libc::_exit(137) };
    }
    std::thread::Builder::new()
        .name("coordinator-watch".into())
        .spawn(move || {
            loop {
                std::thread::sleep(Duration::from_millis(250));
                if unsafe { libc::getppid() } != coordinator {
                    unsafe { libc::_exit(137) };
                }
            }
        })
        .context("cannot watch the coordinator")?;
    Ok(())
}

/// A seccomp filter for the whole worker: `execve` and `execveat` fail with
/// EPERM (as do x32 system calls), so no program starts in a host run even
/// if a helper tried. Nor does a new process: `fork`, `vfork` and a `clone`
/// without `CLONE_THREAD` fail with EPERM, and `clone3` (whose flags the
/// filter cannot read) with ENOSYS, so the C library falls back to `clone`
/// for threads. The worker therefore stays the one process the coordinator
/// started, stops and kills. `no_new_privs` is required for an unprivileged
/// filter.
fn forbid_programs() -> Result<()> {
    const LD_W_ABS: u16 = 0x20;
    const JEQ_K: u16 = 0x15;
    const JGE_K: u16 = 0x35;
    const JSET_K: u16 = 0x45;
    const RET_K: u16 = 0x06;
    const AUDIT_ARCH_X86_64: u32 = 0xC000_003E;
    const X32_BIT: u32 = 0x4000_0000;
    const SECCOMP_RET_KILL_PROCESS: u32 = 0x8000_0000;
    const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;
    let op = |code, jt, jf, k| libc::sock_filter { code, jt, jf, k };
    let deny = libc::SECCOMP_RET_ERRNO | libc::EPERM as u32;
    let unsupported = libc::SECCOMP_RET_ERRNO | libc::ENOSYS as u32;
    // Jump offsets count from the next instruction; the three returns are
    // at 13 (allow), 14 (deny) and 15 (unsupported).
    let filter = [
        op(LD_W_ABS, 0, 0, 4), // 0: seccomp_data.arch
        op(JEQ_K, 1, 0, AUDIT_ARCH_X86_64),
        op(RET_K, 0, 0, SECCOMP_RET_KILL_PROCESS),
        op(LD_W_ABS, 0, 0, 0), // 3: seccomp_data.nr
        op(JGE_K, 9, 0, X32_BIT),
        op(JEQ_K, 8, 0, libc::SYS_execve as u32),
        op(JEQ_K, 7, 0, libc::SYS_execveat as u32),
        op(JEQ_K, 6, 0, libc::SYS_fork as u32),
        op(JEQ_K, 5, 0, libc::SYS_vfork as u32),
        op(JEQ_K, 5, 0, libc::SYS_clone3 as u32),
        op(JEQ_K, 0, 2, libc::SYS_clone as u32), // 10
        op(LD_W_ABS, 0, 0, 16),                  // 11: seccomp_data.args[0], low half (flags)
        op(JSET_K, 0, 1, libc::CLONE_THREAD as u32),
        op(RET_K, 0, 0, SECCOMP_RET_ALLOW), // 13
        op(RET_K, 0, 0, deny),              // 14
        op(RET_K, 0, 0, unsupported),       // 15
    ];
    let program = libc::sock_fprog {
        len: filter.len() as u16,
        filter: filter.as_ptr() as *mut libc::sock_filter,
    };
    if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } != 0 {
        return Err(std::io::Error::last_os_error()).context("cannot set no_new_privs");
    }
    // TSYNC: every thread of the worker gets the filter.
    if unsafe {
        libc::syscall(
            libc::SYS_seccomp,
            libc::SECCOMP_SET_MODE_FILTER,
            libc::SECCOMP_FILTER_FLAG_TSYNC,
            &program as *const libc::sock_fprog,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error())
            .context("cannot install the filter that forbids starting programs");
    }
    Ok(())
}

/// Read and validate the single request frame (host→worker direction).
fn recv_request(rx: &mut FrameReceiver, input: &mut impl Read) -> Result<RequestPayload> {
    let frame = rx.recv(input).map_err(|e| anyhow!("read request: {e}"))?;
    frame
        .check_direction(Direction::HostToWorker)
        .map_err(|e| anyhow!("{e}"))?;
    match frame.payload {
        FramePayload::Request(req) => Ok(*req),
        other => bail!("expected a request frame, got {}", other.op().name()),
    }
}

/// Run the requested operation, reporting every checkpoint and waiting for
/// its ack. Returns the final ctx for the result frame; engine, request and
/// ack problems become a [`FailureInfo`].
fn run_operation(
    req: &RequestPayload,
    rx: &mut FrameReceiver,
    input: &mut impl Read,
    tx: &SharedChannel,
    output: &mut impl Write,
) -> std::result::Result<serde_json::Value, FailureInfo> {
    let recipe = &crate::domain::local(crate::domain::MAIN_RECIPE);
    let build_dir = &crate::domain::local(crate::domain::BUILD);
    let search_path = &crate::domain::local(crate::domain::INPUTS);
    if !recipe.is_file() || !build_dir.is_dir() {
        return Err(request_failure(format!(
            "the domain lacks {} or {}",
            recipe.display(),
            build_dir.display()
        )));
    }
    let engine = build_engine(search_path, req.host.is_none());
    let mut checkpoint_no: u64 = 0;
    let mut sink = |recipe: &Path, ctx: &rhai::Map| -> Result<()> {
        report_checkpoint(tx, output, rx, input, &mut checkpoint_no, recipe, ctx)
            .map(drop)
            .map_err(|err| anyhow!("checkpoint #{checkpoint_no}: {err:#}"))
    };
    // Saved states are keyed by domain path; a host worker knows its recipes
    // by their host paths.
    let restored = req
        .restored_states
        .iter()
        .map(|(recipe, state)| {
            (
                crate::domain::local(recipe).to_string_lossy().into_owned(),
                state.clone(),
            )
        })
        .collect();
    let mut exec = ExecutionContext::new(
        &mut sink,
        req.options.execution.clone(),
        &restored,
        req.persist_state,
    )
    .map_err(|err| request_failure(format!("restoring saved state: {err:#}")))?;
    let load = Load {
        search_path: Some(search_path),
        build_dir,
        defines: &req.options.defines,
        operation: req.operation.as_str(),
    };
    use crate::core::executor as engine_ops;
    let final_ctx = match req.operation {
        Operation::Install => engine_ops::install(&engine, &mut exec, recipe, &load),
        Operation::Remove => engine_ops::remove(&engine, &mut exec, recipe, &load),
        Operation::Cleanup => match req.cleanup_reason.as_deref() {
            Some(reason) => engine_ops::cleanup(&engine, &mut exec, recipe, &load, reason),
            None => return Err(request_failure("cleanup request without a reason".into())),
        },
        Operation::IsInstalled | Operation::IsBuilt | Operation::IsAcquired => {
            let check = req.operation.check().unwrap_or_default();
            engine_ops::check(&engine, &mut exec, recipe, &load, check)
        }
    }
    .map_err(engine_failure)?;
    let mut summary = ctx_json(&final_ctx).map_err(|err| engine_failure(err.into()))?;
    if req.host.is_some() {
        domain_outputs(&mut summary).map_err(|err| engine_failure(err.into()))?;
    }
    Ok(summary)
}

/// A host run's outputs name host paths; the coordinator takes outputs by
/// their domain paths (`/out/...`, `/work/...`), as from a Ginux run.
fn domain_outputs(summary: &mut serde_json::Value) -> Result<()> {
    let Some(outputs) = summary
        .get_mut("outputs")
        .and_then(serde_json::Value::as_object_mut)
    else {
        return Ok(());
    };
    for (name, value) in outputs.iter_mut() {
        let Some(path) = value.as_str() else {
            continue;
        };
        let domain = crate::domain::domain_path(Path::new(path)).with_context(|| {
            format!("ctx.outputs.{name} = {path} is outside this run's directories")
        })?;
        *value = serde_json::Value::String(domain);
    }
    Ok(())
}

/// Resolve modules by [`crate::recipes::find_related`]: beside the importing
/// module, then the prepared recipe root. Rhai gets an absolute path so nested
/// modules keep their source directory.
struct PreparedModules {
    root: PathBuf,
    files: rhai::module_resolvers::FileModuleResolver,
}

impl PreparedModules {
    fn path(&self, source: Option<&str>, path: &str) -> PathBuf {
        let reference = crate::recipes::module_file(path);
        source
            .map(|source| self.root.join(source))
            .and_then(|referrer| {
                crate::recipes::find_related(&reference, &referrer, Some(&self.root))
            })
            .unwrap_or_else(|| self.root.join(reference))
    }
}

impl rhai::ModuleResolver for PreparedModules {
    fn resolve(
        &self,
        engine: &rhai::Engine,
        source: Option<&str>,
        path: &str,
        pos: rhai::Position,
    ) -> Result<rhai::Shared<rhai::Module>, Box<rhai::EvalAltResult>> {
        let full = self.path(source, path);
        self.files
            .resolve(engine, source, &full.to_string_lossy(), pos)
    }
    fn resolve_raw(
        &self,
        engine: &rhai::Engine,
        global: &mut rhai::GlobalRuntimeState,
        scope: &mut rhai::Scope,
        path: &str,
        pos: rhai::Position,
    ) -> Result<rhai::Shared<rhai::Module>, Box<rhai::EvalAltResult>> {
        let full = self.path(global.source(), path);
        self.files
            .resolve_raw(engine, global, scope, &full.to_string_lossy(), pos)
    }
}

fn build_engine(search_path: &Path, programs: bool) -> rhai::Engine {
    let mut engine = crate::core::invocation::engine(programs);
    engine.set_module_resolver(PreparedModules {
        root: search_path.to_path_buf(),
        files: rhai::module_resolvers::FileModuleResolver::new(),
    });
    engine
}

/// Send one checkpoint and block for its ack.
fn report_checkpoint(
    tx: &SharedChannel,
    output: &mut impl Write,
    rx: &mut FrameReceiver,
    input: &mut impl Read,
    checkpoint_no: &mut u64,
    recipe: &Path,
    ctx_map: &rhai::Map,
) -> Result<Option<u64>> {
    *checkpoint_no += 1;
    let seq = *checkpoint_no;
    let state = match ctx_map.get("state") {
        Some(state) => state_json(state)?,
        None => bail!("checkpoint context is missing ctx.state"),
    };
    let mut channel = tx.lock().map_err(|_| anyhow!("worker channel poisoned"))?;
    if channel.failed {
        bail!("worker diagnostic transport failed");
    }
    channel
        .sender
        .send(
            output,
            &FramePayload::Checkpoint(CheckpointPayload {
                seq,
                recipe: crate::domain::domain_path(recipe)
                    .context("recipe identity is not UTF-8 or outside the run")?,
                state,
            }),
        )
        .map_err(|e| {
            channel.failed = true;
            anyhow!("send checkpoint #{seq}: {e}")
        })?;
    // Flush before waiting: the host cannot ack a checkpoint it has not seen.
    output.flush().map_err(|e| {
        channel.failed = true;
        anyhow!("flush checkpoint: {e}")
    })?;
    let ack_timeout = channel.ack_timeout;
    drop(channel);
    let generation = wait_ack(rx, input, seq, ack_timeout)?;
    crate::diagnostics::trace(&format!(
        "worker: checkpoint #{seq} acked (generation {generation:?})"
    ));
    Ok(generation)
}

/// Wait up to `timeout` for the ack of checkpoint `seq`, then validate it is
/// a host→worker ack for exactly this checkpoint.
fn wait_ack(
    rx: &mut FrameReceiver,
    input: &mut impl Read,
    checkpoint_no: u64,
    timeout: Duration,
) -> Result<Option<u64>> {
    wait_readable(timeout)
        .with_context(|| format!("wait for ack of checkpoint #{checkpoint_no}"))?;
    let frame = rx
        .recv(input)
        .map_err(|e| anyhow!("ack for checkpoint #{checkpoint_no}: {e}"))?;
    frame
        .check_direction(Direction::HostToWorker)
        .map_err(|e| anyhow!("ack for checkpoint #{checkpoint_no}: {e}"))?;
    match frame.payload {
        FramePayload::Ack(ack) if ack.seq == checkpoint_no => Ok(ack.generation),
        FramePayload::Ack(ack) => bail!(
            "ack seq mismatch for checkpoint #{checkpoint_no}: host acked {}",
            ack.seq
        ),
        other => bail!(
            "expected ack for checkpoint #{checkpoint_no}, got {}",
            other.op().name()
        ),
    }
}

/// Bounded readability wait on stdin (fd 0), the host→worker stream.
/// Retries on `EINTR`; anything else that is not data is an explicit error —
/// a worker that blocks forever on a dead host is a hang, not a result.
fn wait_readable(timeout: Duration) -> Result<()> {
    let mut pfd = libc::pollfd {
        fd: 0,
        events: libc::POLLIN,
        revents: 0,
    };
    // Ginux refuses (EPERM) a single poll longer than about 300 s; longer
    // waits are a series of bounded polls.
    const MAX_POLL: Duration = Duration::from_secs(300);
    let mut remaining = timeout;
    loop {
        let slice = remaining.min(MAX_POLL);
        let timeout_ms = slice.as_millis() as libc::c_int;
        // Single-threaded stdin wait; EINTR retries.
        let ready = unsafe { libc::poll(&mut pfd, 1, timeout_ms) };
        if ready < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            bail!("poll stdin: {err}");
        }
        if ready == 0 {
            remaining = remaining.saturating_sub(slice);
            if remaining.is_zero() {
                bail!("timed out after {}s", timeout.as_secs());
            }
            continue;
        }
        if pfd.revents & (libc::POLLERR | libc::POLLNVAL) != 0 {
            bail!("stdin not readable (revents {:#x})", pfd.revents);
        }
        // POLLIN or POLLHUP: recv() takes pending data or reports
        // Closed/Truncated explicitly.
        return Ok(());
    }
}

fn send_result(
    tx: &mut FrameSender,
    output: &mut impl Write,
    ok: bool,
    failure: Option<FailureInfo>,
    ctx_summary: serde_json::Value,
) -> Result<()> {
    tx.send(
        output,
        &FramePayload::Result(ResultPayload {
            ok,
            failure,
            ctx_summary,
            network_usage: crate::helpers::acquire::net::usage(),
        }),
    )
    .map_err(|e| anyhow!("send result: {e}"))?;
    output.flush().context("flush result")?;
    Ok(())
}

/// Serialize a final/ checkpoint `ctx` map exactly like the CLI's JSON
/// output does. Must be a JSON object (§4 field rule).
fn ctx_json(map: &rhai::Map) -> Result<serde_json::Value> {
    if let Some(state) = map.get("state") {
        state_json(state)?;
    }
    let dynamic = rhai::Dynamic::from(map.clone());
    let value: serde_json::Value =
        rhai::serde::from_dynamic(&dynamic).map_err(|e| anyhow!("serialize ctx: {e}"))?;
    if !value.is_object() {
        bail!("ctx did not serialize to a JSON object");
    }
    Ok(value)
}

/// Reject values JSON cannot preserve, with the exact field needing migration.
fn state_json(state: &rhai::Dynamic) -> Result<serde_json::Value> {
    fn validate(value: &rhai::Dynamic, path: &str, depth: usize) -> Result<()> {
        if depth > 64 {
            bail!("{path}: state nesting exceeds 64");
        }
        let value = value.flatten_clone();
        if value.is_unit() || value.is_bool() || value.is_int() || value.is_string() {
            return Ok(());
        }
        if value.is_float() {
            if !value
                .as_float()
                .map_err(|error| anyhow!("{path}: {error}"))?
                .is_finite()
            {
                bail!("{path}: nonfinite numbers cannot be saved");
            }
            return Ok(());
        }
        if let Some(array) = value.read_lock::<rhai::Array>() {
            for (index, child) in array.iter().enumerate() {
                validate(child, &format!("{path}[{index}]"), depth + 1)?;
            }
            return Ok(());
        }
        if let Some(map) = value.read_lock::<rhai::Map>() {
            for (name, child) in map.iter() {
                validate(child, &format!("{path}[{name:?}]"), depth + 1)?;
            }
            return Ok(());
        }
        bail!(
            "{path}: {} is not serializable runtime state",
            value.type_name()
        )
    }
    validate(state, "ctx.state", 0)?;
    let value =
        rhai::serde::from_dynamic::<serde_json::Value>(state).context("serialize ctx.state")?;
    if !value.is_object() {
        bail!("ctx.state must be a map");
    }
    Ok(value)
}
