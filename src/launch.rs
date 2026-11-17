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
//! Before launch, the input owner creates a fresh domain root from the selected
//! assembled rootfs tar, verified against its digest (cloned from a verified
//! per-digest cache where the filesystem has reflinks, see
//! `crate::inputs::image_cache`), and copies the worker plus staged inputs into
//! `/inputs`. The root is private to this invocation and
//! is kept alive until the launcher is reaped.
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
/// first byte of each frame starts a bounded complete-frame read. A snapshot
/// acknowledgement waits this long after the launcher's last visible progress
/// (see [`SnapshotWait`]).
const FRAME_TIMEOUT: Duration = Duration::from_secs(300);

/// Bound on checkpoint chatter: more than this many checkpoints in one
/// invocation is a `protocol` failure, never unbounded acking.
const MAX_CHECKPOINTS: u64 = 1024;

/// Bound on log collection: launcher/worker stderr beyond this many bytes is
/// still drained (so a chatty worker never blocks on a full pipe) but not
/// retained.
const LOG_CAP_BYTES: u64 = crate::invocation_log::MAX_LOG_BYTES;

/// Grace for teardown after failure/cancellation. Successful final export uses
/// the remaining supervised execution deadline instead.
pub(crate) const SHUTDOWN_GRACE: Duration = Duration::from_secs(10);

/// Bytes of log tail attached to failure diagnostics.
const LOG_TAIL_BYTES: usize = 4096;

/// One executable operation, as the coordinator runs it.
pub(crate) struct Invocation<'a> {
    pub operation: crate::worker::protocol::Operation,
    pub recipe: &'a Path,
    pub search_path: Option<&'a Path>,
    pub persist_state: bool,
    pub options: crate::worker::protocol::InvocationOptions,
    pub inputs: &'a [crate::inputs::ExtraInput],
    /// Lock and saved-state scope key.
    pub destination: &'a Path,
    pub state_dir: Option<&'a Path>,
    /// Each dependency's declared output names, for the worker's `DEPS`
    /// (`crate::worker::protocol::RequestPayload::dependencies`).
    pub dependencies: &'a std::collections::BTreeMap<String, Vec<String>>,
    /// The recipe's state key (`crate::state::key`), recorded with each
    /// generation.
    pub key: &'a str,
    /// What a successful install of the main recipe does once its outputs
    /// are published ([`Finish`]); None for every other run (dependencies,
    /// the other operations).
    pub finish: Option<&'a Finish<'a>>,
}

/// The end of a successful install of the main recipe (docs/recipe-format.md,
/// sections 6.1, 6.4 and 6.5): activate its package (the caller's package
/// store; nothing for a producer, whose install is its published outputs),
/// then the cleanup step unless the program keeps its material. Both run in
/// the run that installed, under the destination lock it holds, so no other
/// run of the recipe at this destination can start in between or lose
/// material it needs; the dependency runs made for it are cleaned afterwards
/// (`crate::backend`).
pub(crate) struct Finish<'a> {
    pub keep: bool,
    pub activate: Option<&'a crate::Activate<'a>>,
}

/// Outcome of one supervised domain run.
#[derive(Debug)]
pub(crate) struct LaunchReport {
    /// The exactly-once worker result frame.
    pub result: ResultPayload,
    /// A successful run's result for the caller ([`finish_outputs`]); None
    /// for a failed one.
    pub summary: Option<serde_json::Value>,
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
    image: &mut Option<crate::inputs::VerifiedImage>,
) -> Result<LaunchReport> {
    recorded(prepared, invocation, |diagnostics| {
        run_domain_inner(prepared, invocation, launcher, diagnostics, image)
    })
}

/// Keep an invocation's log: its authority before the run, and its outcome
/// (worker result, checkpoints, network use) after, for either mode.
pub(crate) fn recorded(
    prepared: &PreparedInvocation,
    invocation: &Invocation<'_>,
    run: impl FnOnce(&crate::invocation_log::InvocationLog) -> Result<LaunchReport>,
) -> Result<LaunchReport> {
    let state_dir = invocation.state_dir;
    let diagnostics = crate::retained::state_root(state_dir)
        .and_then(|root| crate::invocation_log::InvocationLog::new(&root, &prepared.invocation_id))
        .map_err(|error| {
            crate::failure::Category::Storage.wrap(error, "cannot create the invocation log")
        })?;
    diagnostics.authority(prepared)?;
    let outcome = run(&diagnostics);
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
    let mut details = details;
    details["contract"] = serde_json::to_value(&prepared.contract)?;
    details["network_usage"] = match &outcome {
        Ok(report) => report
            .summary
            .as_ref()
            .and_then(|summary| summary.get("network_usage"))
            .cloned()
            .unwrap_or_else(|| match &report.result.network_usage {
                Some(usage) => crate::needs::report::host_network_usage(
                    &prepared.network.host_endpoints,
                    usage,
                ),
                None => crate::needs::report::network_usage(
                    &prepared.network.host_endpoints,
                    &diagnostics,
                    report.log_discarded_bytes,
                ),
            }),
        Err(_) => {
            serde_json::json!({"status":"unobserved","reason":"invocation failed before complete log accounting","warnings":[]})
        }
    };
    diagnostics.record(status, details)?;
    outcome
}

/// Descriptors the launcher and supervisor use besides Ginux's storage
/// reservation (control, result, log, lease and epoll descriptors).
const LAUNCHER_FD_MARGIN: u64 = 256;

/// Ginux reserves one backing descriptor per possible storage object plus 128
/// (`launch_storage.rs`) and refuses to start when the HARD `RLIMIT_NOFILE` is
/// below that; it raises the soft limit itself. systemd hosts have a hard limit
/// of 524288, but OpenRC/busybox hosts (AcornOS, live medium) typically have
/// 4096, so a recipe with a large `storage_max_objects` could never start there.
/// Raise the hard limit (never above `nr_open`) before the launcher is spawned:
/// root may, an ordinary user may not, and then Ginux's own error follows this
/// line. The soft limit stays as it is; the limit only ever grows.
fn ensure_open_file_limit(storage_max_objects: u64) {
    let needed = storage_max_objects + 128 + LAUNCHER_FD_MARGIN;
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) } != 0 {
        return;
    }
    let hard = limit.rlim_max as u64;
    if hard >= needed {
        return;
    }
    let nr_open = std::fs::read_to_string("/proc/sys/fs/nr_open")
        .ok()
        .and_then(|text| text.trim().parse::<u64>().ok())
        .unwrap_or(needed);
    let target = needed.min(nr_open);
    if target > hard {
        let raised = libc::rlimit {
            rlim_cur: limit.rlim_cur,
            rlim_max: target as libc::rlim_t,
        };
        if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &raised) } == 0 {
            if target >= needed {
                return;
            }
            crate::diagnostics::diag!(
                "recipe: the system allows at most {target} open files (/proc/sys/fs/nr_open) \
                 but this recipe needs {needed}; raise fs.nr_open"
            );
            return;
        }
    }
    crate::diagnostics::diag!(
        "recipe: the hard open-file limit is {hard} but this recipe needs {needed}; \
         run recipe as root or raise the hard nofile limit (ulimit -Hn {needed})"
    );
}

fn run_domain_inner(
    prepared: &PreparedInvocation,
    invocation: &Invocation<'_>,
    launcher: &Path,
    diagnostics: &crate::invocation_log::InvocationLog,
    image: &mut Option<crate::inputs::VerifiedImage>,
) -> Result<LaunchReport> {
    let Invocation {
        operation,
        recipe,
        persist_state,
        ref options,
        destination,
        state_dir,
        dependencies,
        key: invocation_key,
        ..
    } = *invocation;
    let run = invocation;
    let invocation = prepared.invocation_id.clone();
    let timeout = frame_timeout()
        .map_err(|error| crate::failure::Category::Startup.wrap(error, "invalid frame timeout"))?;
    let _destination_lock =
        crate::retained::lock_destination(destination, timeout).map_err(|error| {
            crate::failure::Category::Storage.wrap(error, "cannot lock the destination")
        })?;
    let run_dir = crate::temporary::TemporaryDirectory::new()
        .context(crate::failure::Category::Startup.error("cannot create ginux run directory"))?;
    let result_path = run_dir.path().join("result.json");
    let export_path = run_dir.path().join("export.tar");
    let config_path = prepared.config_path();

    // Recipe and Ginux enter the identity by their declared behaviour
    // versions; with exact identity also by the worker's and the launcher's
    // bytes (`crate::identity`). The worker copied into the root must be the
    // file hashed here (checked once it is copied).
    let worker_digest = if persist_state && options.execution.exact_identity {
        crate::retained::file_digest(&crate::inputs::worker_binary()?)?
    } else {
        String::new()
    };
    let mut retained = if persist_state {
        Some(
            crate::retained::Retained::open(
                state_dir,
                destination,
                recipe,
                invocation_key,
                prepared,
                &worker_digest,
                Some(launcher),
                options,
                None,
                operation == crate::worker::protocol::Operation::Download,
            )
            .map_err(|error| {
                crate::failure::Category::Storage.wrap(error, "cannot open saved state")
            })?,
        )
    } else {
        None
    };
    // What is done already, from Recipe's records: nothing to run, or a run
    // that starts from an unfinished generation, or from nothing.
    let restore_path = run_dir.path().join("restore.tar");
    let restored_states;
    let mut restore = match start(retained.as_ref(), run, prepared, &restore_path)? {
        Start::Done { publish } => {
            return done_report(
                &retained,
                &restore_path,
                publish,
                prepared,
                &invocation,
                run,
            );
        }
        Start::Run { restore, states } => {
            restored_states = states;
            restore
        }
    };

    let domain_root = crate::inputs::prepare_root(prepared, image).map_err(|error| {
        anyhow::Error::new(crate::failure::Category::Startup.error(format!(
            "cannot prepare Ginux root for invocation {invocation}: {error:#}"
        )))
    })?;
    if !worker_digest.is_empty() && domain_root.worker_digest != worker_digest {
        bail!(crate::failure::Category::Startup.error(format!(
            "the worker changed while invocation {invocation} started; run it again"
        )));
    }
    if !restore {
        restore = crate::inputs::seed_target(prepared, domain_root.path(), &restore_path).map_err(
            |error| {
                crate::failure::Category::Startup
                    .wrap(error, "cannot seed the requested target view")
            },
        )?;
    }
    crate::cancellation::check()?;
    let (mut control, control_server) = if persist_state {
        let (client, server) = UnixStream::pair()?;
        (Some(DeadlinePipe::new(client, timeout)?), Some(server))
    } else {
        (None, None)
    };
    let control_fd = control_server.as_ref().map(AsRawFd::as_raw_fd);

    ensure_open_file_limit(options.execution.storage_max_objects);
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
    let mut worker_diagnostics =
        std::fs::File::create(diagnostics.file(crate::invocation_log::WORKER_LOG))?;
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| {
            anyhow::Error::new(crate::failure::Category::Startup.error(format!(
                "cannot spawn ginux launcher {} for invocation \
                 {invocation}: {err:#}",
                launcher.display(),
            )))
        })?;

    drop(control_server);
    let stdin = child
        .stdin
        .take()
        .context(crate::failure::Category::Startup.error("launcher stdin was not piped"))?;
    let stdout = child
        .stdout
        .take()
        .context(crate::failure::Category::Startup.error("launcher stdout was not piped"))?;
    let stderr = child
        .stderr
        .take()
        .context(crate::failure::Category::Startup.error("launcher stderr was not piped"))?;
    let log_path = diagnostics.file(crate::invocation_log::SUPERVISOR_LOG);
    let drain = spawn_stderr_drain(stderr, log_path.clone());

    let mut worker_started = false;
    let request = RequestPayload {
        invocation_id: invocation.clone(),
        recipe_name: prepared.declaration.name().to_owned(),
        operation,
        persist_state,
        options: options.clone(),
        dependencies: dependencies.clone(),
        // The saved values go with the files of the generation restored,
        // and with nothing else.
        restored_states,
        host: None,
    };
    let mut progress = options.execution.progress_file.as_deref().map(|path| {
        crate::progress_file::ProgressFile::start(
            path,
            &invocation,
            recipe
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or_default(),
            &operation.to_string(),
        )
    });
    let launcher_pid = child.id();
    let pump = {
        // A checkpoint: the supervisor snapshots the domain storage over the
        // control socket, and the snapshot is paired with the state.
        let mut capture = |point: &crate::worker::protocol::CheckpointPayload,
                           deadline: Instant|
         -> Result<Option<u64>> {
            let Some(state) = retained.as_mut() else {
                return Ok(None);
            };
            let control = control.as_mut().context(
                crate::failure::Category::Startup.error("missing snapshot control socket"),
            )?;
            capture_checkpoint(
                state,
                control,
                point,
                SnapshotWait {
                    launcher: launcher_pid,
                    idle: timeout,
                    cap: deadline,
                },
            )
            .map(Some)
        };
        pump_protocol(
            request,
            (stdin, stdout),
            &mut capture,
            &mut worker_diagnostics,
            &mut worker_started,
            timeout,
            &mut progress,
        )
    };
    if let Some(progress) = progress.as_mut() {
        progress.finish(pump.as_ref().is_ok_and(|stream| stream.result.ok));
    }
    let stream = match pump {
        Ok(stream) => stream,
        Err(err) => {
            // The stream error stays the result; teardown problems are reported beside it.
            let exit = reap_launcher(&mut child, Instant::now() + SHUTDOWN_GRACE).unwrap_or_else(
                |error| {
                    crate::diagnostics::diag!("recipe: warning: {error:#}");
                    None
                },
            );
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
    // Diagnostics are not fsynced (see `crate::invocation_log`).
    drop(worker_diagnostics);
    let exit = reaped?;

    let record = crate::ginux::ResultRecord::read(&result_path, &invocation)?;
    check_agreement(&invocation, &stream.result, &record)?;
    check_launcher_exit(&invocation, &stream.result, &record, exit)?;
    let mut summary = None;
    if stream.result.ok {
        let usage = crate::needs::report::network_usage(
            &prepared.network.host_endpoints,
            diagnostics,
            drained.discarded_bytes,
        );
        summary = Some(finish_outputs(
            &stream.result,
            &export_path,
            None,
            &mut retained,
            prepared,
            &invocation,
            run,
            usage,
        )?);
    }

    Ok(LaunchReport {
        result: stream.result,
        summary,
        checkpoints_acked: stream.checkpoints,
        generations: stream.generations,
        launcher_code: exit,
        result_outcome: record.outcome,
        result_code: record.code,
        log_total_bytes: drained.total_bytes,
        log_discarded_bytes: drained.discarded_bytes,
    })
}

/// What Recipe's records leave to do for one run (docs/recipe-format.md,
/// section 6.2): decided before any worker starts, never from recipe code.
pub(crate) enum Start {
    /// Nothing is missing: a download already kept for `download`, a kept
    /// result for `build`, or for `install` a kept result published from the
    /// restore input (`publish`). No worker runs.
    Done { publish: bool },
    /// A worker runs; `restore` when the restore input holds an unfinished
    /// generation (a checkpoint, or the kept downloads of this state key) it
    /// starts from, with the saved values `states` of that generation.
    Run {
        restore: bool,
        states: std::collections::BTreeMap<String, serde_json::Value>,
    },
}

impl Start {
    fn fresh() -> Self {
        Self::Run {
            restore: false,
            states: Default::default(),
        }
    }
}

/// Decide [`Start`] from the current generation of the run's identity. A
/// generation counts only while its archive has the SHA-256 and length
/// Recipe recorded (`crate::retained::Retained::verify_current`); a finished
/// result is never built on again, and a `RERUN` recipe's never reused.
pub(crate) fn start(
    retained: Option<&crate::retained::Retained>,
    run: &Invocation<'_>,
    prepared: &PreparedInvocation,
    restore_path: &Path,
) -> Result<Start> {
    use crate::retained::Reached;
    use crate::worker::protocol::Operation;
    let operation = run.operation;
    let cleans = cleans(run);
    let storage = |error| crate::failure::Category::Storage.wrap(error, "cannot read saved state");
    let Some(retained) = retained else {
        let start = Start::fresh();
        plan_event(prepared, operation, &start, cleans);
        return Ok(start);
    };
    let rerun = prepared.declaration.declared.rerun;
    // A build or install that has nothing of its own to start from starts
    // from the downloads `recipe download` kept for this state key.
    let from_downloads = || -> Result<Start> {
        if !operation.builds() || !retained.has_download() {
            return Ok(Start::fresh());
        }
        Ok(
            match retained.restore_download(restore_path).map_err(storage)? {
                true => Start::Run {
                    restore: true,
                    states: retained.download_states(),
                },
                false => Start::fresh(),
            },
        )
    };
    let start = match (operation, retained.reached()) {
        (Operation::Cleanup | Operation::Latest, _) => Start::fresh(),
        (_, None) => from_downloads()?,
        (Operation::Download, Some(_)) => match retained.verify_current().map_err(storage)? {
            true => Start::Done { publish: false },
            false => Start::fresh(),
        },
        (Operation::Build, Some(Reached::Built)) if !rerun => {
            match retained.verify_current().map_err(storage)? {
                true => Start::Done { publish: false },
                false => from_downloads()?,
            }
        }
        (Operation::Install, Some(Reached::Built)) if !rerun => {
            match retained.restore_input(restore_path).map_err(storage)? {
                true => Start::Done { publish: true },
                false => from_downloads()?,
            }
        }
        (_, Some(Reached::Built)) => from_downloads()?,
        (_, Some(Reached::Downloaded | Reached::Checkpoint)) => {
            match retained.restore_input(restore_path).map_err(storage)? {
                true => Start::Run {
                    restore: true,
                    states: retained.states().clone(),
                },
                false => from_downloads()?,
            }
        }
    };
    plan_event(prepared, operation, &start, cleans);
    Ok(start)
}

/// Whether this run ends with the cleanup step: the main recipe's install,
/// unless the program keeps its material.
fn cleans(run: &Invocation<'_>) -> bool {
    run.operation == crate::worker::protocol::Operation::Install
        && run.finish.is_some_and(|finish| !finish.keep)
}

/// The recipe's own `plan` event for a front end, once Recipe knows what is
/// done: the steps that will run (the others are not needed this time).
fn plan_event(
    prepared: &PreparedInvocation,
    operation: crate::worker::protocol::Operation,
    start: &Start,
    cleans: bool,
) {
    use crate::worker::protocol::Operation;
    let declared = &prepared.declaration.declared;
    let steps: Vec<&str> = match start {
        Start::Done { publish: true } if cleans => vec!["install", "cleanup"],
        Start::Done { publish: true } => vec!["install"],
        Start::Done { publish: false } => Vec::new(),
        Start::Run { .. } => declared
            .steps()
            .into_iter()
            .filter(|step| match *step {
                "download" => matches!(
                    operation,
                    Operation::Download | Operation::Build | Operation::Install
                ),
                "build" => operation.builds(),
                "install" => operation == Operation::Install,
                "cleanup" => cleans,
                _ => false,
            })
            .collect(),
    };
    if matches!(operation, Operation::Cleanup | Operation::Latest) {
        return;
    }
    crate::core::output::machine_event(serde_json::json!({
        "event": "plan", "recipe": declared.package.name, "steps": steps,
    }));
}

/// Every step of the recipe with its status in this run (section 8.6):
/// what the worker reported, `none` for a step the recipe does not have,
/// `done` for an install that published, `already` for a step within the
/// operation's reach that nothing had to do, `skipped` for the rest. The
/// steps the worker did not report are told as `step` events here.
fn finish_steps(
    prepared: &PreparedInvocation,
    worker: &[crate::core::events::StepReport],
    operation: crate::worker::protocol::Operation,
    published: bool,
    cleanup: crate::core::events::StepStatus,
) -> Vec<crate::core::events::StepReport> {
    use crate::core::events::{StepReport, StepStatus};
    use crate::worker::protocol::Operation;
    let declared = &prepared.declaration.declared;
    let has = declared.steps();
    let name = &declared.package.name;
    ["download", "build", "install", "cleanup"]
        .into_iter()
        .map(|step| {
            if let Some(reported) = worker.iter().find(|reported| reported.step == step) {
                return reported.clone();
            }
            let status = if !has.contains(&step) {
                StepStatus::None
            } else {
                match step {
                    "install" if published => StepStatus::Done,
                    "download" if operation != Operation::Cleanup => StepStatus::Already,
                    "build" if operation.builds() => StepStatus::Already,
                    // Done after an install that deleted the material;
                    // skipped when the program keeps it, and for the other
                    // operations.
                    "cleanup" => cleanup,
                    _ => StepStatus::Skipped,
                }
            };
            if operation != Operation::Cleanup {
                crate::core::output::step_event(name, step, status, "");
            }
            StepReport::new(step, status)
        })
        .collect()
}

/// Validate a finished run's outputs in `export_path` (or `tree`) against
/// the declared outputs and bounds; the publication to make of them.
fn validate(
    outputs: &std::collections::BTreeMap<String, String>,
    export_path: &Path,
    tree: Option<&Path>,
    prepared: &PreparedInvocation,
    options: &crate::worker::protocol::InvocationOptions,
    state_dir: Option<&Path>,
) -> Result<Option<crate::publication::Publication>> {
    let needs = &prepared.declaration.needs;
    let moved = match tree {
        Some(run_root) => crate::publication::prepare_tree(
            run_root,
            outputs,
            &options.execution,
            state_dir,
            needs,
            true,
        )?,
        None => None,
    };
    match moved {
        Some(publication) => Ok(Some(publication)),
        None => crate::publication::prepare(
            export_path,
            outputs,
            &options.execution,
            state_dir,
            needs,
            true,
        ),
    }
    .context(crate::failure::Category::Storage.error("cannot validate completed outputs"))
}

/// Publish what [`validate`] prepared; each output's published host path.
fn publish(
    publication: Option<crate::publication::Publication>,
    invocation: &str,
) -> Result<serde_json::Value> {
    Ok(match publication {
        Some(publication) => serde_json::to_value(publication.publish(invocation).context(
            crate::failure::Category::Storage.error("cannot publish completed outputs"),
        )?)?,
        None => serde_json::json!({}),
    })
}

/// The run's result for the caller: the recipe's `name` and `version` (from
/// its declarations), `steps`, `outputs` (each output's published host
/// path), `prefix` (`PREFIX`, or null), the compiled `contract` and the
/// bounded `network_usage`.
fn summary(
    prepared: &PreparedInvocation,
    steps: Vec<crate::core::events::StepReport>,
    outputs: serde_json::Value,
    usage: serde_json::Value,
) -> Result<serde_json::Value> {
    let declaration = &prepared.declaration;
    let mut summary = serde_json::json!({
        "name": declaration.name(),
        "version": declaration.version(),
        "steps": steps,
        "outputs": outputs,
        "prefix": prepared.execution.package_prefix.as_ref()
            .filter(|_| declaration.needs.outputs.contains_key("package")),
        "network_usage": usage,
    });
    if let Some(contract) = &prepared.contract {
        summary["contract"] = serde_json::to_value(contract)?;
    }
    Ok(summary)
}

/// A run with nothing missing ([`Start::Done`]): no worker ran. An install
/// publishes the kept result from the restore input, which holds the
/// verified copy of its archive.
#[allow(clippy::too_many_arguments)]
pub(crate) fn done_report(
    retained: &Option<crate::retained::Retained>,
    restore_path: &Path,
    publish: bool,
    prepared: &PreparedInvocation,
    invocation: &str,
    run: &Invocation<'_>,
) -> Result<LaunchReport> {
    let operation = run.operation;
    let kept = retained
        .as_ref()
        .map(|state| state.outputs().clone())
        .unwrap_or_default();
    let outputs = if publish {
        let publication = validate(
            &kept,
            restore_path,
            None,
            prepared,
            &run.options,
            run.state_dir,
        )?;
        self::publish(publication, invocation)?
    } else {
        serde_json::json!({})
    };
    let usage = serde_json::json!({"status": "unused",
        "reason": "nothing ran: Recipe's records hold what was asked for", "warnings": []});
    let mut summary = summary(prepared, Vec::new(), outputs, usage)?;
    let cleanup = if publish && operation == crate::worker::protocol::Operation::Install {
        complete(run, &mut summary)?
    } else {
        crate::core::events::StepStatus::Skipped
    };
    let steps = finish_steps(prepared, &[], operation, publish, cleanup);
    summary["steps"] = serde_json::to_value(&steps)?;
    let result = ResultPayload {
        ok: true,
        failure: None,
        outputs: if publish { kept } else { Default::default() },
        steps: steps.clone(),
        saved: crate::worker::protocol::SavedState::default().to_value(),
        latest: None,
        network_usage: None,
    };
    Ok(LaunchReport {
        result,
        summary: Some(summary),
        checkpoints_acked: 0,
        generations: Vec::new(),
        launcher_code: Some(0),
        result_outcome: crate::ginux::Outcome::Exited,
        result_code: Some(0),
        log_total_bytes: 0,
        log_discarded_bytes: 0,
    })
}

/// After a successful worker result: validate the export archive against the
/// declared outputs and bounds, keep it (the downloads of `download`, the
/// finished result of `build` and `install`, never a `RERUN` recipe's), and
/// publish the outputs of an install. The same for a Ginux export and a
/// host run's archive.
#[allow(clippy::too_many_arguments)]
pub(crate) fn finish_outputs(
    result: &ResultPayload,
    export_path: &Path,
    tree: Option<&Path>,
    retained: &mut Option<crate::retained::Retained>,
    prepared: &PreparedInvocation,
    invocation: &str,
    run: &Invocation<'_>,
    usage: serde_json::Value,
) -> Result<serde_json::Value> {
    use crate::retained::Reached;
    use crate::worker::protocol::Operation;
    let (operation, options, state_dir) = (run.operation, &run.options, run.state_dir);
    if let Some(warnings) = usage["warnings"].as_array() {
        for warning in warnings {
            if let Some(warning) = warning.as_str() {
                crate::diagnostics::diag!("recipe: warning: {warning}");
            }
        }
    }
    let install = operation == Operation::Install;
    // A build or install names every declared output; nothing else names any.
    let publication = if operation.builds() {
        validate(
            &result.outputs,
            export_path,
            tree,
            prepared,
            options,
            state_dir,
        )?
    } else {
        None
    };
    let keep = match operation {
        // A recipe without a download step has no downloads to keep.
        Operation::Download if prepared.declaration.declared.steps().contains(&"download") => {
            Some((Reached::Downloaded, Default::default()))
        }
        Operation::Build | Operation::Install if !prepared.declaration.declared.rerun => {
            Some((Reached::Built, result.outputs.clone()))
        }
        _ => None,
    };
    // The run's archive moves into the kept generation (a finished result
    // stays kept until the install is complete and activated, so a crash
    // before that installs it without building again). The publication was
    // unpacked from it (or, in a host run, moved from the run's tree) above
    // and no longer reads it.
    // A RERUN recipe keeps no result, and its finished run leaves no
    // checkpoint to be resumed next time.
    if operation.builds()
        && prepared.declaration.declared.rerun
        && let Some(state) = retained.as_mut()
    {
        state.discard().map_err(|error| {
            crate::failure::Category::Storage.wrap(error, "cannot discard a finished checkpoint")
        })?;
    }
    if let (Some(state), Some((reached, outputs))) = (retained.as_mut(), keep) {
        state
            .finish(export_path, result.saved.clone(), reached, outputs)
            .map_err(|error| {
                crate::failure::Category::Storage
                    .wrap(error, "cannot persist final state/files pair")
            })?;
    }
    // A build keeps its result and publishes nothing.
    let outputs = if install {
        publish(publication, invocation)?
    } else {
        serde_json::json!({})
    };
    let mut summary = summary(prepared, Vec::new(), outputs, usage)?;
    let cleanup = if install {
        complete(run, &mut summary)?
    } else {
        crate::core::events::StepStatus::Skipped
    };
    let steps = finish_steps(prepared, &result.steps, operation, install, cleanup);
    summary["steps"] = serde_json::to_value(&steps)?;
    if let Some(latest) = &result.latest {
        summary["latest"] = latest.as_str().into();
    }
    Ok(summary)
}

/// After an install published its outputs ([`Finish`]): activate, then the
/// cleanup step unless kept, deleting every generation of the recipe's scope
/// at this destination (downloads, unfinished runs, the kept result) under
/// the destination lock this run holds. Only once activation succeeded: a
/// failed activation fails the run and leaves the kept result, which the
/// next install publishes again. Sets `kept` and `freed_bytes` in `summary`;
/// returns the cleanup step's status. A dependency's run has no [`Finish`]
/// and keeps its result for its consumer.
fn complete(
    run: &Invocation<'_>,
    summary: &mut serde_json::Value,
) -> Result<crate::core::events::StepStatus> {
    use crate::core::events::StepStatus;
    let Some(finish) = run.finish else {
        return Ok(StepStatus::Skipped);
    };
    if let Some(activate) = finish.activate {
        activate(summary)?;
    }
    summary["kept"] = finish.keep.into();
    summary["freed_bytes"] = 0.into();
    if finish.keep {
        return Ok(StepStatus::Skipped);
    }
    let removed =
        crate::retained::Scope::of(run.state_dir, run.recipe, run.destination).and_then(|scope| {
            match scope {
                Some(scope) => scope.remove_locked(crate::state::Material::All.kinds()),
                None => Ok(0),
            }
        });
    Ok(match removed {
        Ok(freed) => {
            summary["freed_bytes"] = freed.into();
            StepStatus::Done
        }
        // The program is installed; what could not be deleted stays for
        // `recipe cleanup`.
        Err(error) => {
            crate::diagnostics::diag!(
                "recipe: warning: the install is complete, but its material could not be deleted: {error:#}; `recipe cleanup` deletes it later"
            );
            StepStatus::Failed
        }
    })
}

/// Completed worker stream: exactly one result plus the relayed checkpoints.
pub(crate) struct WorkerStream {
    pub result: ResultPayload,
    pub checkpoints: u64,
    pub generations: Vec<(u64, Option<u64>)>,
    pub stdin: DeadlinePipe<std::process::ChildStdin>,
    pub execution_deadline: Instant,
}

/// What the coordinator does at a checkpoint, while the worker waits for its
/// ack: make the files durable with the state, and name the generation.
pub(crate) type Capture<'a> =
    dyn FnMut(&crate::worker::protocol::CheckpointPayload, Instant) -> Result<Option<u64>> + 'a;

/// Send the request, relay checkpoint→ack, and collect the exactly-once
/// result. Transport errors before the result frame are returned with a
/// `protocol` or `startup` category (see [`classify_stream_error`]); the
/// caller enriches them with launcher/result-file state.
pub(crate) fn pump_protocol(
    request: RequestPayload,
    pipes: (std::process::ChildStdin, std::process::ChildStdout),
    capture: &mut Capture<'_>,
    worker_diagnostics: &mut std::fs::File,
    worker_started: &mut bool,
    timeout: Duration,
    progress: &mut Option<crate::progress_file::ProgressFile>,
) -> Result<WorkerStream> {
    let (stdin, stdout) = pipes;
    let mut stdin = DeadlinePipe::new(stdin, timeout)?;
    let mut stdout = DeadlinePipe::new(stdout, timeout)?;
    let mut tx = FrameSender::new();
    let mut rx = FrameReceiver::new();
    let invocation_id = request.invocation_id.clone();
    let invocation = invocation_id.as_str();
    let execution_deadline =
        Instant::now() + Duration::from_secs(request.options.execution.timeout_secs);

    tx.send(&mut stdin, &FramePayload::Request(Box::new(request)))
        .map_err(|err| classify_stream_error(invocation, "send request", err, true))?;
    stdin
        .flush()
        .map_err(|err| classify_stream_error(invocation, "flush request", err, true))?;

    let mut checkpoints: u64 = 0;
    let mut generations = Vec::new();
    let mut diagnostic_budget = crate::worker::protocol::DiagnosticBudget::default();
    let result = loop {
        if Instant::now() >= execution_deadline {
            bail!(
                crate::failure::Category::Deadline.error("supervised execution deadline expired")
            );
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
                anyhow::Error::new(crate::failure::Category::Protocol.error(format!(
                    "invocation {invocation}: worker stream direction \
                 violation: {err}"
                )))
            })?;
        *worker_started = true;
        match frame.payload {
            FramePayload::Diagnostic(payload) => {
                diagnostic_budget.accept(&payload).map_err(|violation| {
                    crate::failure::Category::Protocol.error(violation.to_string())
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
            FramePayload::Progress(report) => {
                if let Some(progress) = progress.as_mut() {
                    progress.report(&report);
                }
            }
            FramePayload::Checkpoint(point) => {
                checkpoints += 1;
                if checkpoints > MAX_CHECKPOINTS {
                    bail!(crate::failure::Category::Protocol.error(format!(
                        "invocation {invocation} sent more than \
                         {MAX_CHECKPOINTS} checkpoints"
                    )));
                }
                let seq = point.seq;
                if seq != checkpoints {
                    bail!(crate::failure::Category::Protocol.error(format!(
                        "invocation {invocation} sent checkpoint {seq} where {checkpoints} was due"
                    )));
                }
                let generation = capture(&point, execution_deadline).map_err(|error| {
                    crate::failure::Category::Storage
                        .wrap(error, format!("cannot keep checkpoint {seq}"))
                })?;
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
                bail!(crate::failure::Category::Protocol.error(format!(
                    "invocation {invocation} received a host-direction \
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

/// How long the coordinator waits for a filesystem snapshot. Its duration
/// grows with the domain's storage and with disk load, so a fixed frame
/// timeout fails large checkpoints on a busy disk. Instead the launcher's
/// progress extends the wait: while its written bytes (`wchar` in
/// `/proc/<launcher>/io`; the supervisor writes nothing else while every
/// task is stopped for the snapshot) grow, the deadline moves to `idle`
/// after the latest growth, plus time to flush what was written at
/// [`SNAPSHOT_FLUSH_FLOOR`] (the snapshot ends with an fsync). A launcher that
/// writes nothing for `idle` still times out, and `cap` (the invocation's
/// execution deadline) bounds the whole wait, so a stuck launcher cannot hang
/// a build forever. Where `/proc` does not show the launcher's I/O, the wait
/// is the fixed `idle` window, as before.
struct SnapshotWait {
    launcher: u32,
    idle: Duration,
    cap: Instant,
}

/// Slowest flush rate assumed for snapshot bytes already written.
const SNAPSHOT_FLUSH_FLOOR: u64 = 8 * 1024 * 1024;

fn capture_checkpoint(
    retained: &mut crate::retained::Retained,
    control: &mut DeadlinePipe<UnixStream>,
    point: &crate::worker::protocol::CheckpointPayload,
    wait: SnapshotWait,
) -> Result<u64> {
    let pending = retained.begin()?;
    control.reset(wait.idle);
    control.progress = Some(Progress::new(wait));
    let snapshot = crate::ginux::request_snapshot(control, pending.generation, &pending.path);
    control.progress = None;
    snapshot?;
    retained
        .commit(pending, &point.recipe, point.state.clone())
        .context("cannot persist checkpoint state/files pair")
}

/// One absolute deadline covers the complete frame, including partial headers,
/// payloads and blocked writes. Nonblocking descriptors prevent a poll/read race
/// from accidentally turning the next operation into an unbounded wait.
pub(crate) struct DeadlinePipe<T> {
    inner: T,
    deadline: Instant,
    frame_start: Option<(Duration, Instant)>,
    execution_deadline: Option<Instant>,
    /// Set while a snapshot is awaited: launcher progress extends `deadline`.
    progress: Option<Progress>,
}

/// The launcher's write progress during one snapshot (see [`SnapshotWait`]).
struct Progress {
    wait: SnapshotWait,
    io: PathBuf,
    /// `wchar` when first read, and at the latest growth.
    first: Option<u64>,
    last: u64,
    next_check: Instant,
}

impl Progress {
    fn new(wait: SnapshotWait) -> Self {
        let mut progress = Self {
            io: PathBuf::from(format!("/proc/{}/io", wait.launcher)),
            wait,
            first: None,
            last: 0,
            next_check: Instant::now(),
        };
        progress.first = progress.written();
        progress.last = progress.first.unwrap_or(0);
        progress
    }

    fn written(&self) -> Option<u64> {
        let text = std::fs::read_to_string(&self.io).ok()?;
        text.lines()
            .find_map(|line| line.strip_prefix("wchar:"))?
            .trim()
            .parse()
            .ok()
    }

    /// A later deadline when the launcher wrote more since the last check.
    fn extend(&mut self, deadline: Instant) -> Instant {
        let now = Instant::now();
        if now < self.next_check {
            return deadline;
        }
        self.next_check = now + Duration::from_secs(1);
        let (Some(first), Some(written)) = (self.first, self.written()) else {
            return deadline;
        };
        if written <= self.last {
            return deadline;
        }
        self.last = written;
        let flush = Duration::from_secs((written - first) / SNAPSHOT_FLUSH_FLOOR);
        deadline.max((now + self.wait.idle + flush).min(self.wait.cap))
    }
}

pub(crate) fn frame_timeout() -> Result<Duration> {
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
            progress: None,
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
    fn wait(&mut self, events: i16) -> std::io::Result<()> {
        loop {
            crate::cancellation::check_io()?;
            if let Some(progress) = self.progress.as_mut() {
                self.deadline = progress.extend(self.deadline);
            }
            let left = self.deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    if let Some(progress) = &self.progress {
                        if Instant::now() >= progress.wait.cap {
                            "supervised execution deadline expired during the snapshot"
                        } else {
                            "filesystem snapshot made no visible progress within the frame timeout"
                        }
                    } else if self
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
                    if count != 0
                        && let Some((frame, execution)) = self.frame_start.take()
                    {
                        self.deadline = (Instant::now() + frame).min(execution);
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
pub(crate) fn classify_stream_error(
    invocation: &str,
    what: &str,
    err: impl std::fmt::Display,
    before_request_delivered: bool,
) -> anyhow::Error {
    if crate::cancellation::signal() != 0 {
        return crate::cancellation::cancelled().into();
    }
    if before_request_delivered {
        anyhow::Error::new(crate::failure::Category::Startup.error(format!(
            "invocation {invocation}: cannot {what} through launcher \
             (launcher cannot serve the worker transport): {err}"
        )))
    } else {
        anyhow::Error::new(
            crate::failure::Category::Protocol
                .error(format!("invocation {invocation}: cannot {what}: {err}")),
        )
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
    if crate::ginux::peek_outcome(result_path, invocation)
        == Some(crate::ginux::Outcome::SupervisorError)
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
        file_note
            .as_deref()
            .unwrap_or("no result file was produced"),
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

pub(crate) fn exit_message(exit: Option<i32>) -> String {
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
            bail!(crate::failure::Category::Supervisor.error(format!(
                "invocation {invocation} worker reported success but the \
                 supervisor failed during finalization (refusing publication)"
            )));
        }
        if record.outcome != Outcome::Exited || record.code != Some(0) {
            bail!(crate::failure::Category::Protocol.error(
                format!("invocation {invocation} worker reported success but \
                 the authoritative result is {}/{}",
                record.outcome,
                record
                    .code
                    .map(|c| c.to_string())
                    .or_else(|| record.signal.map(|s| format!("signal {s}")))
                    .unwrap_or_else(|| "no status".to_string()),
            )
            ));
        }
        return Ok(());
    }
    if record.outcome == Outcome::SupervisorError {
        bail!(crate::failure::Category::Supervisor.error(format!(
            "invocation {invocation} worker reported failure and the \
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
        bail!(crate::failure::Category::Workload.error(format!(
            "invocation {invocation} worker was terminated by signal {}",
            record.signal.map(|s| s.to_string()).unwrap_or_default(),
        )));
    }
    if record.outcome == Outcome::Exited && record.code != Some(0) {
        let detail = result
            .failure
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_else(|| "worker reported failure without detail".to_string());
        bail!(crate::failure::Category::Workload.error(format!(
            "invocation {invocation} worker exited with code {} \
             (launcher exit {}): {detail}",
            record.code.unwrap_or(-1),
            exit_message(exit),
        )));
    }
    Ok(())
}
