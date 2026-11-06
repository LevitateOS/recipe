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
    FrameSender, Operation, RequestPayload, ResultPayload,
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


fn request_failure(message: String) -> FailureInfo {
    FailureInfo { kind: FailureKind::Request, phase: None, message }
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
    let invocation_id = ensure_internal_entry()?;

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
            return send_result(&mut tx, &mut output, false, Some(failure), serde_json::json!({}));
        }
    };
    if request.invocation_id != invocation_id {
        let failure = request_failure("request invocation id does not match worker bootstrap".into());
        return send_result(&mut tx, &mut output, false, Some(failure), serde_json::json!({}));
    }
    crate::diagnostics::trace(&format!(
        "worker: invocation={} operation={} persist_state={} recipe={}",
        request.invocation_id,
        request.operation,
        request.persist_state,
        crate::domain::MAIN_RECIPE
    ));

    let channel = Arc::new(Mutex::new(WorkerChannel {
        sender: tx,
        budget: Default::default(),
        ack_timeout: Duration::from_secs(request.options.execution.timeout_secs),
        failed: false,
        terminal: false,
    }));
    install_diagnostics(&channel)?;
    crate::core::output::set_machine_events(request.options.execution.machine_events);
    crate::diagnostics::line("recipe: managed worker started");
    let outcome = run_operation(&request, &mut rx, &mut input, &channel, &mut output);
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
fn ensure_internal_entry() -> Result<String> {
    if !std::env::args_os()
        .skip(1)
        .eq(crate::domain::WORKER_ARGS.map(std::ffi::OsString::from))
        || std::env::var(crate::domain::ENV_WORKER).as_deref() != Ok(crate::domain::ENV_WORKER_VALUE)
    {
        bail!(
            "private worker entry requires coordinator bootstrap arguments"
        );
    }
    let id = std::env::var(crate::domain::ENV_INVOCATION)
        .context("private worker entry requires RECIPE_INVOCATION")?;
    if !crate::domain::valid_name(&id) {
        bail!("private worker entry has an invalid invocation id");
    }
    Ok(id)
}

/// Read and validate the single request frame (host→worker direction).
fn recv_request(rx: &mut FrameReceiver, input: &mut impl Read) -> Result<RequestPayload> {
    let frame = rx.recv(input).map_err(|e| anyhow!("read request: {e}"))?;
    frame
        .check_direction(Direction::HostToWorker)
        .map_err(|e| anyhow!("{e}"))?;
    match frame.payload {
        FramePayload::Request(req) => Ok(req),
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
    let recipe = Path::new(crate::domain::MAIN_RECIPE);
    let build_dir = Path::new(crate::domain::BUILD);
    let search_path = Path::new(crate::domain::INPUTS);
    if !recipe.is_file() || !build_dir.is_dir() {
        return Err(request_failure(format!(
            "the domain lacks {} or {}",
            recipe.display(),
            build_dir.display()
        )));
    }
    let engine = build_engine(search_path);
    let mut checkpoint_no: u64 = 0;
    let mut sink = |recipe: &Path, ctx: &rhai::Map| -> Result<()> {
        report_checkpoint(tx, output, rx, input, &mut checkpoint_no, recipe, ctx)
            .map(drop)
            .map_err(|err| anyhow!("checkpoint #{checkpoint_no}: {err:#}"))
    };
    let mut exec = ExecutionContext::new(
        &mut sink,
        req.options.execution.clone(),
        build_dir,
        &req.restored_states,
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
            engine_ops::check(&engine, &mut exec, recipe, &load, req.operation.as_str())
        }
    }
    .map_err(engine_failure)?;
    ctx_json(&final_ctx).map_err(|err| engine_failure(err.into()))
}

/// Resolve relative to the importing module, then the prepared recipe root.
/// Pass an absolute path to Rhai so nested modules retain their source directory.
struct PreparedModules {
    root: PathBuf,
    files: rhai::module_resolvers::FileModuleResolver,
}

impl PreparedModules {
    fn path(&self, source: Option<&str>, path: &str) -> PathBuf {
        let reference = Path::new(path).with_extension("rhai");
        if let Some(parent) = source.and_then(|name| Path::new(name).parent()) {
            let parent = if parent.is_absolute() {
                parent.to_path_buf()
            } else {
                self.root.join(parent)
            };
            let candidate = parent.join(&reference);
            if candidate.is_file() {
                return candidate;
            }
        }
        self.root.join(reference)
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

fn build_engine(search_path: &Path) -> rhai::Engine {
    let mut engine = crate::core::invocation::engine();
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
                recipe: recipe
                    .to_str()
                    .context("recipe identity is not UTF-8")?
                    .to_owned(),
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

