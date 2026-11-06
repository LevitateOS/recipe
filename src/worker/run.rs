//! Worker entry point: run the existing engine behind protocol v6.
//!
//! The worker is the same `recipe` binary entered with a private argv marker (see
//! the dispatch in `src/bin/`). It reads exactly one request frame from
//! stdin, dispatches `install` / `remove` / `cleanup` / `<check>` through the
//! existing engine entry points under [`ExecutionContext::worker`] (the
//! execution seam: caller-held lock, checkpoints reported through a sink
//! instead of rewriting recipe sources), then terminates the stream with
//! exactly one result frame on stdout and exits 0.
//!
//! Transport discipline (matches `docs/ginux-recipe-interfaces.md` §2/§4):
//!
//! - stdin carries request/ack frames. Stdout carries serialized diagnostic,
//!   checkpoint, failure and result frames. Child bytes and engine narration
//!   use the explicitly installed logging sink; startup/audit details stay on
//!   stderr. No recipe text or child bytes are written as raw protocol output.
//! - Every checkpoint blocks for its `ack` with a bounded 300 s wait (§4).
//!   The host owns the `{seq → generation}` association; the worker only waits
//!   for it and logs the generation to stderr.
//! - The private entry invokes the shared executor directly; it never selects
//!   a public backend. Worker environment values do not authorize host execution.
//!
//! Failure mapping: the engine's [`InstallAttemptError`] shape is
//! preserved through [`install_with_context`], so an unrecoverable engine
//! failure reports `fatal: true` while a named phase failure reports
//! `fatal: false` with its phase and reason (`auto.<phase>.failure`). Other
//! operations (`remove` / `cleanup` / checks) have no phase-error shape and
//! still report `fatal: true`, as does any checkpoint/ack transport failure.
//!
//! The coordinator prepares immutable inputs and restores the matching filesystem.
//! The worker initializes current facts, restores only ctx.state, and reports
//! per-recipe state at checkpoints. It never rewrites recipe sources.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};

use crate::core::executor::{
    CheckpointPoint, CompiledRecipe, ExecutionContext, InstallAttemptError, cleanup_with_context,
    install_with_context, remove_with_context, run_check_with_context,
};
use crate::worker::protocol::{
    CheckpointPayload, Direction, FailureInfo, FailurePayload, FramePayload, FrameReceiver,
    FrameSender, RequestPayload, ResultPayload,
};

struct WorkerChannel {
    sender: FrameSender,
    diagnostic_bytes: usize,
    diagnostic_frames: usize,
    truncated: bool,
    failed: bool,
    terminal: bool,
}
type SharedChannel = Arc<Mutex<WorkerChannel>>;

fn install_diagnostics(channel: &SharedChannel) -> Result<()> {
    let channel = Arc::clone(channel);
    crate::diagnostics::install(Box::new(move |stream, mut bytes| {
        use crate::worker::protocol::{
            DiagnosticPayload, MAX_DIAGNOSTIC_BYTES, MAX_DIAGNOSTIC_CHUNK, MAX_DIAGNOSTIC_FRAMES,
        };
        let mut state = channel
            .lock()
            .map_err(|_| std::io::Error::other("worker channel poisoned"))?;
        if state.failed || state.terminal {
            return Err(std::io::Error::other("worker channel is closed"));
        }
        let mut output = std::io::stdout();
        while !bytes.is_empty() && !state.truncated {
            let room = MAX_DIAGNOSTIC_BYTES - state.diagnostic_bytes;
            let payload = if room == 0 || state.diagnostic_frames == MAX_DIAGNOSTIC_FRAMES {
                state.truncated = true;
                DiagnosticPayload {
                    stream: stream.into(),
                    data: Vec::new(),
                    truncated: true,
                }
            } else {
                let take = room.min(MAX_DIAGNOSTIC_CHUNK).min(bytes.len());
                let data = bytes[..take].to_vec();
                bytes = &bytes[take..];
                state.diagnostic_bytes += take;
                state.diagnostic_frames += 1;
                DiagnosticPayload {
                    stream: stream.into(),
                    data,
                    truncated: false,
                }
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

/// Bounded wait for one checkpoint acknowledgement (§4: 300 s default).
const ACK_TIMEOUT: Duration = Duration::from_secs(300);

/// Recipe location and build directory inside the prepared domain.
struct LocalConfig {
    /// Recipe file after the private command marker (argv[2]).
    recipe: PathBuf,
    /// Coordinator-prepared writable build directory.
    build_dir: PathBuf,
    /// Module/extends search path: the prepared inputs root holding the recipe.
    search_path: PathBuf,
}

impl LocalConfig {
    fn from_env() -> Result<Self> {
        let recipe = PathBuf::from(crate::domain::MAIN_RECIPE);
        if !recipe.exists() {
            bail!("worker recipe not found: {}", recipe.display());
        }
        let build_dir = PathBuf::from(crate::domain::BUILD);
        if !build_dir.is_dir() {
            bail!(
                "worker build directory is unavailable: {}",
                build_dir.display()
            );
        }
        let search_path = PathBuf::from(crate::domain::INPUTS);
        Ok(Self {
            recipe,
            build_dir,
            search_path,
        })
    }
}

/// One engine failure for the `failure` + `result` termination frames.
/// Install preserves the engine's fatal-vs-phase shape: an unrecoverable
/// engine failure is fatal (the invocation cannot continue and the host
/// should not retry it as-is); a named phase failure is non-fatal and carries
/// the phase and its reason for host retry/autofix policy.
struct EngineFailure {
    fatal: bool,
    phase: String,
    reason: &'static str,
    message: String,
}

impl EngineFailure {
    /// Map the structured install error faithfully: `Fatal` (unrecoverable
    /// engine failure) vs `Phase` (named phase failure with its reason).
    fn from_attempt(operation: &str, err: InstallAttemptError) -> Self {
        match err {
            InstallAttemptError::Fatal(err) => Self {
                fatal: true,
                phase: operation.to_string(),
                reason: "fatal",
                message: format!("{err:#}"),
            },
            InstallAttemptError::Phase {
                reason,
                phase,
                error,
            } => Self {
                fatal: false,
                phase: phase.to_string(),
                reason,
                message: format!("{error:#}"),
            },
        }
    }

    /// Single-shot operations (`remove` / `cleanup` / checks) and local
    /// worker problems have no phase-error shape: always fatal, with the
    /// operation as phase.
    fn workload(phase: &str, err: anyhow::Error) -> Self {
        Self {
            fatal: true,
            phase: phase.to_string(),
            reason: "workload",
            message: format!("{err:#}"),
        }
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
    let local = LocalConfig::from_env()?;

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
            let message = format!("bad request: {err:#}");
            return abort(&mut tx, &mut output, "protocol", &message);
        }
    };
    if request.invocation_id != invocation_id {
        return abort(
            &mut tx,
            &mut output,
            "protocol",
            "request invocation id does not match worker bootstrap",
        );
    }
    crate::diagnostics::trace(&format!(
        "worker: invocation={} operation={} persist_state={} recipe={}",
        request.invocation_id,
        request.operation,
        request.persist_state,
        local.recipe.display()
    ));

    let channel = Arc::new(Mutex::new(WorkerChannel {
        sender: tx,
        diagnostic_bytes: 0,
        diagnostic_frames: 0,
        truncated: false,
        failed: false,
        terminal: false,
    }));
    install_diagnostics(&channel)?;
    crate::core::output::set_machine_events(request.options.execution.machine_events);
    crate::diagnostics::line("recipe: managed worker started");
    let outcome = run_operation(&local, &request, &mut rx, &mut input, &channel, &mut output);
    let mut channel = channel
        .lock()
        .map_err(|_| anyhow!("worker channel poisoned"))?;
    channel.terminal = true;
    if channel.failed {
        bail!("worker diagnostic transport failed");
    }
    let tx = &mut channel.sender;
    match outcome {
        Ok(summary) => send_result(tx, &mut output, true, Vec::new(), summary),
        Err(failure) => {
            send_failure(
                tx,
                &mut output,
                failure.fatal,
                &failure.phase,
                failure.reason,
                &failure.message,
            )?;
            send_result(
                tx,
                &mut output,
                false,
                vec![FailureInfo {
                    phase: failure.phase,
                    reason: failure.reason.to_string(),
                    message: failure.message,
                }],
                serde_json::Value::Object(serde_json::Map::new()),
            )
        }
    }
}

/// Require the private argv entry, the coordinator's bootstrap flag, and an
/// invocation id. Public backend selection never uses these environment values.
fn ensure_internal_entry() -> Result<String> {
    if !std::env::args_os()
        .skip(1)
        .eq(crate::domain::WORKER_ARGS.map(std::ffi::OsString::from))
        || std::env::var(crate::domain::ENV_WORKER).as_deref() != Ok("1")
    {
        bail!(
            "private worker entry requires coordinator bootstrap arguments; refusing host execution"
        );
    }
    let id = std::env::var(crate::domain::ENV_INVOCATION)
        .context("private worker entry requires RECIPE_INVOCATION; refusing host execution")?;
    crate::inputs::validate_invocation_id(&id)?;
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
        other => bail!("expected a request frame, got {}", payload_name(&other)),
    }
}

/// Run the requested operation through the existing engine under the
/// worker seam, reporting every checkpoint and waiting for each ack.
///
/// Returns the final-ctx summary for the result frame. Engine, request, and
/// ack problems become [`EngineFailure`] (failure + result frames, exit 0);
/// only a broken stdout propagates as `Err` (no result sent, exit nonzero).
#[allow(clippy::too_many_arguments)]
fn run_operation(
    local: &LocalConfig,
    req: &RequestPayload,
    rx: &mut FrameReceiver,
    input: &mut impl Read,
    tx: &SharedChannel,
    output: &mut impl Write,
) -> std::result::Result<serde_json::Value, EngineFailure> {
    let engine = build_engine(&local.search_path);
    let defines = req.options.defines.clone();
    let search_path = Some(local.search_path.as_path());

    let mut checkpoint_no: u64 = 0;
    let mut sink = |compiled: &CompiledRecipe,
                    ctx_map: &rhai::Map,
                    point: CheckpointPoint,
                    _persist: bool|
     -> Result<()> {
        // The sink fires at every persistence point even when persist is
        // disabled (seam contract); the host decides whether to retain the
        // matching state/files generation. The `_persist` flag is
        // intentionally unused here: the worker never writes sources either
        // way.
        report_checkpoint(
            tx,
            output,
            rx,
            input,
            &mut checkpoint_no,
            compiled,
            ctx_map,
            point,
        )
        .map_err(|err| anyhow!("checkpoint #{checkpoint_no}: {err:#}"))?;
        Ok(())
    };
    let mut exec = ExecutionContext::worker(&mut sink);
    exec.set_options(req.options.execution.clone());
    exec.restore_states(&req.restored_states, req.persist_state)
        .map_err(|err| EngineFailure::workload("startup", err))?;

    let operation = req.operation.as_str();
    let final_ctx = match operation {
        "install" => install_with_context(
            &engine,
            &local.build_dir,
            &local.recipe,
            &defines,
            req.persist_state,
            search_path,
            &mut exec,
        )
        .map_err(|err| EngineFailure::from_attempt(operation, err))?,
        "remove" => remove_with_context(
            &engine,
            &local.build_dir,
            &local.recipe,
            search_path,
            &defines,
            req.persist_state,
            &mut exec,
        )
        .map_err(|err| EngineFailure::workload(operation, err))?,
        "cleanup" => cleanup_with_context(
            &engine,
            &local.build_dir,
            &local.recipe,
            search_path,
            &defines,
            &req.options.cleanup_reason,
            req.persist_state,
            &mut exec,
        )
        .map_err(|err| EngineFailure::workload(operation, err))?,
        "is_installed" | "isinstalled" => run_check_with_context(
            &engine,
            &local.build_dir,
            &local.recipe,
            search_path,
            &defines,
            "is_installed",
            &mut exec,
        )
        .map_err(|err| EngineFailure::workload("is_installed", err))?,
        "is_built" | "isbuilt" => run_check_with_context(
            &engine,
            &local.build_dir,
            &local.recipe,
            search_path,
            &defines,
            "is_built",
            &mut exec,
        )
        .map_err(|err| EngineFailure::workload("is_built", err))?,
        "is_acquired" | "isacquired" => run_check_with_context(
            &engine,
            &local.build_dir,
            &local.recipe,
            search_path,
            &defines,
            "is_acquired",
            &mut exec,
        )
        .map_err(|err| EngineFailure::workload("is_acquired", err))?,
        other => {
            return Err(EngineFailure {
                fatal: true,
                phase: other.to_string(),
                reason: "startup",
                message: format!(
                    "unknown operation {other:?}: want install|remove|cleanup|\
                     is_installed|is_built|is_acquired"
                ),
            });
        }
    };

    ctx_json(&final_ctx).map_err(|err| EngineFailure::workload(operation, err))
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

/// Send one checkpoint for a persistence point and block for its ack.
///
/// The engine seam hands us the structured [`CheckpointPoint`] directly, so
/// the frame reports the actual phase and reason verbatim — no label
/// matching. Persistence points are unchanged: phase completions
/// (`phase:<phase>`) and success cleanups (the actual cleanup reason).
#[allow(clippy::too_many_arguments)]
fn report_checkpoint(
    tx: &SharedChannel,
    output: &mut impl Write,
    rx: &mut FrameReceiver,
    input: &mut impl Read,
    checkpoint_no: &mut u64,
    compiled: &CompiledRecipe,
    ctx_map: &rhai::Map,
    point: CheckpointPoint,
) -> Result<u64> {
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
                phase: point.phase.to_string(),
                reason: point.reason.to_string(),
                recipe: compiled
                    .recipe_path
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
    drop(channel);
    let generation = wait_ack(rx, input, seq)?;
    crate::diagnostics::trace(&format!(
        "worker: checkpoint #{seq} acked (generation {generation})"
    ));
    Ok(generation)
}

/// Wait for the ack of checkpoint `seq` with a bounded wait (§4: 300 s), then
/// validate it is a host→worker ack for exactly this checkpoint.
fn wait_ack(rx: &mut FrameReceiver, input: &mut impl Read, checkpoint_no: u64) -> Result<u64> {
    wait_readable(ACK_TIMEOUT)
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
            payload_name(&other)
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
    let timeout_ms = timeout.as_millis().min(i32::MAX as u128) as libc::c_int;
    loop {
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
            bail!("timed out after {}s", timeout.as_secs());
        }
        if pfd.revents & (libc::POLLERR | libc::POLLNVAL) != 0 {
            bail!("stdin not readable (revents {:#x})", pfd.revents);
        }
        // POLLIN or POLLHUP: recv() takes pending data or reports
        // Closed/Truncated explicitly.
        return Ok(());
    }
}

/// Best-effort stream termination when the request itself is unusable:
/// `failure` (fatal) then `result`, so the host never hangs on a silent
/// worker. Send errors propagate (transport collapse → nonzero exit).
fn abort(tx: &mut FrameSender, output: &mut impl Write, reason: &str, message: &str) -> Result<()> {
    send_failure(tx, output, true, "", reason, message)?;
    send_result(
        tx,
        output,
        false,
        vec![FailureInfo {
            phase: String::new(),
            reason: reason.to_string(),
            message: message.to_string(),
        }],
        serde_json::Value::Object(serde_json::Map::new()),
    )
}

fn send_failure(
    tx: &mut FrameSender,
    output: &mut impl Write,
    fatal: bool,
    phase: &str,
    reason: &str,
    message: &str,
) -> Result<()> {
    tx.send(
        output,
        &FramePayload::Failure(FailurePayload {
            fatal,
            phase: phase.to_string(),
            reason: reason.to_string(),
            message: message.to_string(),
        }),
    )
    .map_err(|e| anyhow!("send failure: {e}"))?;
    output.flush().context("flush failure")?;
    Ok(())
}

fn send_result(
    tx: &mut FrameSender,
    output: &mut impl Write,
    ok: bool,
    failures: Vec<FailureInfo>,
    ctx_summary: serde_json::Value,
) -> Result<()> {
    tx.send(
        output,
        &FramePayload::Result(ResultPayload {
            ok,
            failures,
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

fn payload_name(payload: &FramePayload) -> &'static str {
    match payload {
        FramePayload::Request(_) => "request",
        FramePayload::Checkpoint(_) => "checkpoint",
        FramePayload::Ack(_) => "ack",
        FramePayload::Failure(_) => "failure",
        FramePayload::Result(_) => "result",
        FramePayload::Diagnostic(_) => "diagnostic",
    }
}
