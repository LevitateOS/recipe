//! Worker entry point: run the existing engine behind protocol v1.
//!
//! The worker is the same `recipe` binary entered with `RECIPE_WORKER=1` (see
//! the dispatch in `src/bin/`). It reads exactly one request frame from
//! stdin, dispatches `install` / `remove` / `cleanup` / `<check>` through the
//! existing engine entry points under [`ExecutionContext::worker_stub`] (the
//! execution seam: caller-held lock, checkpoints reported through a sink
//! instead of rewriting recipe sources), then terminates the stream with
//! exactly one result frame on stdout and exits 0.
//!
//! Transport discipline (matches `docs/ginux-recipe-interfaces.md` §2/§4):
//!
//! - stdin is the host→worker direction (one `request`, then one `ack` per
//!   checkpoint); stdout is the worker→host direction (`checkpoint`, an
//!   optional `failure`, exactly one `result`). Nothing else may be written to
//!   stdout: engine narration and worker diagnostics go to stderr (verified:
//!   `core::output` only uses `eprintln!`).
//! - Every checkpoint blocks for its `ack` with a bounded 300 s wait (§4).
//!   The host owns the `{seq → generation}` association; the worker only waits
//!   for it and logs the generation to stderr.
//! - `RECIPE_WORKER=1` forces in-process execution through the unified R04
//!   selection: a requested non-host backend is overridden to in-process (with
//!   a stderr note), so the worker never invokes the Ginux backend.
//!
//! Failure mapping (R03): the engine's [`InstallAttemptError`] shape is
//! preserved through [`install_with_context`], so an unrecoverable engine
//! failure reports `fatal: true` while a named phase failure reports
//! `fatal: false` with its phase and reason (`auto.<phase>.failure`). Other
//! operations (`remove` / `cleanup` / checks) have no phase-error shape and
//! still report `fatal: true`, as does any checkpoint/ack transport failure.
//!
//! What this does NOT do (owned by later tasks): input preparation and stable
//! domain paths (R05 — the recipe comes from argv, see [`LocalConfig`]);
//! generation-backed persistence (R07 — the host decides what the acked
//! generation means for its source files).

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
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

/// Bounded wait for one checkpoint acknowledgement (§4: 300 s default).
const ACK_TIMEOUT: Duration = Duration::from_secs(300);

/// Recipe inputs the worker resolves locally (R05 owns prepared domain inputs;
/// until then the harness passes these the same way a domain config would).
struct LocalConfig {
    /// Recipe file to execute (argv[1]).
    recipe: PathBuf,
    /// Coordinator-prepared build dir (`$BUILD_DIR`, allowlisted by §5).
    build_dir: PathBuf,
    /// Module/extends search path (`$RECIPE_WORKER_RECIPES` or the recipe's
    /// own directory).
    search_path: PathBuf,
}

impl LocalConfig {
    fn from_env() -> Result<Self> {
        let recipe = std::env::args_os()
            .nth(1)
            .map(PathBuf::from)
            .context("worker usage: RECIPE_WORKER=1 recipe <recipe-path>")?;
        if !recipe.exists() {
            bail!("worker recipe not found: {}", recipe.display());
        }
        let build_dir = std::env::var_os("BUILD_DIR")
            .map(PathBuf::from)
            .context("worker requires the coordinator-prepared BUILD_DIR")?;
        let metadata = std::fs::metadata(&build_dir).with_context(|| {
            format!(
                "worker: prepared BUILD_DIR {} is unavailable",
                build_dir.display()
            )
        })?;
        if !metadata.is_dir() {
            bail!(
                "worker: prepared BUILD_DIR {} is not a directory",
                build_dir.display()
            );
        }
        let search_path = match std::env::var_os("RECIPE_WORKER_RECIPES") {
            Some(p) => PathBuf::from(p),
            None => recipe
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| PathBuf::from(".")),
        };
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

/// Worker entry point for the `RECIPE_WORKER=1` binary dispatch.
///
/// Reads one request frame, runs the engine, sends `failure` (on error) and
/// exactly one `result`, and returns `Ok` on the protocol path (binary exits
/// 0). Transport collapse (unwritable stdout) returns `Err` (binary exits
/// nonzero; the host sees a worker exit without `result`).
pub fn run_worker() -> Result<()> {
    ensure_internal_entry()?;
    ensure_in_process_backend()?;
    let local = LocalConfig::from_env()?;

    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let stdout = std::io::stdout();
    let mut output = stdout.lock();
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
    eprintln!(
        "worker: invocation={} operation={} persist_ctx={} recipe={}",
        request.invocation_id,
        request.operation,
        request.persist_ctx,
        local.recipe.display()
    );

    // (2) Dispatch through the existing engine; (3) terminate with exactly
    // one result frame.
    match run_operation(&local, &request, &mut rx, &mut input, &mut tx, &mut output) {
        Ok(summary) => send_result(&mut tx, &mut output, true, Vec::new(), summary),
        Err(failure) => {
            send_failure(
                &mut tx,
                &mut output,
                failure.fatal,
                &failure.phase,
                failure.reason,
                &failure.message,
            )?;
            send_result(
                &mut tx,
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

/// The worker entry path is internal: it is only reachable through a
/// coordinator launch, which issues a `RECIPE_INVOCATION` id (interfaces §5
/// allowlist) alongside `RECIPE_WORKER=1`. A bare `RECIPE_WORKER=1` in the
/// public command environment is refused here — before argv is read or any
/// recipe code is evaluated — so the flag alone can never bypass the public
/// coordinator execution path (S0).
fn ensure_internal_entry() -> Result<()> {
    match std::env::var("RECIPE_INVOCATION") {
        Ok(id) => crate::inputs::validate_invocation_id(&id).map_err(|err| {
            anyhow!(
                "worker refuses to run on the host: RECIPE_WORKER=1 without a \
                 coordinator-issued RECIPE_INVOCATION ({err:#}); refusing host execution"
            )
        }),
        Err(_) => bail!(
            "worker refuses to run on the host: RECIPE_WORKER=1 without RECIPE_INVOCATION \
             (only a coordinator launch issues the invocation id); refusing host execution"
        ),
    }
}

/// `RECIPE_WORKER=1` forces in-process execution. This goes through the
/// unified R04 selection ([`crate::backend::Backend::select`]): under the
/// worker flag selection itself resolves to [`crate::backend::Backend::Host`],
/// so reaching the `Ginux` arm below is impossible — it stays as the explicit
/// guard so the worker can never recursively invoke the Ginux backend, with no
/// second parallel environment check beside the canonical selection.
fn ensure_in_process_backend() -> Result<()> {
    match crate::backend::Backend::from_env()? {
        crate::backend::Backend::Host => Ok(()),
        crate::backend::Backend::Ginux { launcher } => {
            bail!(
                "worker refuses ginux backend (launcher {}): RECIPE_WORKER=1 forces \
                 in-process execution (no recursive Ginux backend invocation)",
                launcher.display()
            )
        }
    }
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

/// Run the requested operation through the existing engine under the F04
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
    tx: &mut FrameSender,
    output: &mut impl Write,
) -> std::result::Result<serde_json::Value, EngineFailure> {
    let engine = build_engine(&local.search_path);
    let defines: Vec<(String, String)> = Vec::new();
    let search_path = Some(local.search_path.as_path());

    let mut checkpoint_no: u64 = 0;
    let mut sink = |compiled: &CompiledRecipe,
                    ctx_map: &rhai::Map,
                    point: CheckpointPoint,
                    _persist: bool|
     -> Result<()> {
        // The sink fires at every persistence point even when persist is
        // disabled (seam contract); the host decides what the acked
        // generation means for its source files. The `_persist` flag is
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
    let mut exec = ExecutionContext::worker_stub(&mut sink);

    let operation = req.operation.as_str();
    let final_ctx = match operation {
        "install" => install_with_context(
            &engine,
            &local.build_dir,
            &local.recipe,
            &defines,
            req.persist_ctx,
            search_path,
            None,
            &mut exec,
        )
        .map_err(|err| EngineFailure::from_attempt(operation, err))?,
        "remove" => remove_with_context(
            &engine,
            &local.recipe,
            search_path,
            &defines,
            req.persist_ctx,
            &mut exec,
        )
        .map_err(|err| EngineFailure::workload(operation, err))?,
        "cleanup" => cleanup_with_context(
            &engine,
            &local.build_dir,
            &local.recipe,
            search_path,
            &defines,
            "manual",
            req.persist_ctx,
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

/// Build the engine exactly like the host CLI does (same limits and helpers),
/// plus module resolution against the worker search path. No autofix/LLM: the
/// host coordinator owns retry policy (R11), and no credentials cross into
/// the worker.
fn build_engine(search_path: &Path) -> rhai::Engine {
    let mut engine = rhai::Engine::new();
    engine.set_max_operations(2_000_000);
    engine.set_max_call_levels(64);
    engine.set_max_string_size(64 * 1024 * 1024);
    crate::helpers::register_all(&mut engine);
    let mut resolver = rhai::module_resolvers::FileModuleResolver::new();
    resolver.set_base_path(search_path);
    engine.set_module_resolver(resolver);
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
    tx: &mut FrameSender,
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
    let main_ctx =
        ctx_json(ctx_map).with_context(|| format!("serialize ctx for checkpoint #{seq}"))?;
    tx.send(
        output,
        &FramePayload::Checkpoint(CheckpointPayload {
            seq,
            phase: point.phase.to_string(),
            reason: point.reason.to_string(),
            main_ctx: main_ctx.clone(),
            // `ctx` is a single map shared across `//! extends:`; when a base
            // recipe is present the host needs the same content to decide
            // which trusted source (main or base) the acked generation updates.
            base_ctx: if compiled.base_path.is_some() {
                main_ctx
            } else {
                serde_json::Value::Object(serde_json::Map::new())
            },
            // A recipe-side source name, never a host path (§4).
            base_path: compiled
                .base_path
                .as_ref()
                .and_then(|p| p.file_name())
                .map(|s| s.to_string_lossy().into_owned()),
        }),
    )
    .map_err(|e| anyhow!("send checkpoint #{seq}: {e}"))?;
    // Flush before waiting: the host cannot ack a checkpoint it has not seen.
    output.flush().context("flush checkpoint")?;
    let generation = wait_ack(rx, input, seq)?;
    eprintln!("worker: checkpoint #{seq} acked (generation {generation})");
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
#[cfg(unix)]
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

/// Non-Unix fallback: block in `recv`, which still reports EOF/truncation
/// explicitly but has no timeout bound.
#[cfg(not(unix))]
fn wait_readable(_timeout: Duration) -> Result<()> {
    Ok(())
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
    let dynamic = rhai::Dynamic::from(map.clone());
    let value: serde_json::Value =
        rhai::serde::from_dynamic(&dynamic).map_err(|e| anyhow!("serialize ctx: {e}"))?;
    if !value.is_object() {
        bail!("ctx did not serialize to a JSON object");
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
    }
}
