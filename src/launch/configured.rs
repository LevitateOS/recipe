//! The configured Ginux launch: everything that builds and runs
//! `ginux launch CONFIG ROOT ...` for one worker.
//!
//! [`run`] is this module's one entry point and the only thing the rest of
//! Recipe calls (`super::run_domain`). It takes the prepared invocation, the
//! request record, the launcher and the run's paths, and returns the run's
//! outcome with the worker's result ([`RunOutcome`]); every failure carries a
//! [`crate::failure::Category`]. The decision to run at all, the kept state,
//! publication and logs are the caller's. The move to upstream's public
//! lifecycle replaces this module (`docs/ginux-public-lifecycle-migration.md`,
//! section 4): create an instance, stage inputs, start and wait, read the
//! outcome, export, delete.
//!
//! What it does, in order:
//!
//! - A fresh domain root from the selected assembled rootfs tar, verified
//!   against its digest (cloned from a verified per-digest cache where the
//!   filesystem has reflinks, see `crate::inputs::image_cache`), with the
//!   worker, the staged inputs and the request file
//!   ([`crate::domain::REQUEST`]) in `/inputs`. The root is private to this
//!   invocation and kept alive until the launcher is reaped. A run that
//!   starts from kept files hands them over as `--restore-file`.
//! - Launcher argv: `launch <config.toml> <prepared-rootfs> --result-file
//!   <fresh host path> --export-file <fresh tar path> [--restore-file
//!   <tar>]`. The result file is the supervisor's record (a missing or
//!   malformed one is a `protocol` failure, never success); it is the
//!   authority on how the run ended. The launcher runs in its own process
//!   group, dies with the coordinator (`PDEATHSIG`), and makes the read-only
//!   binds of `/inputs` in a mount namespace of its own ([`bind_read_only`]).
//! - Standard input is `/dev/null`. Standard output carries the worker's line
//!   records, read by `super::worker_io::pump_lines` until it ends. Standard
//!   error (launcher stderr, which also carries `ginux:`-prefixed supervisor
//!   diagnostics) is drained to a capped log and never parsed: log bytes only
//!   appear in human diagnostics, never as results.
//! - After the launcher exited, the worker's result file is read from the
//!   export archive (the supervisor exports `work` and `out` only after a
//!   clean exit) and must agree with the supervisor's record. No valid result
//!   is classified from the record and the launcher's exit, as a stream that
//!   ended without a result always was.
//! - An interrupted run keeps nothing of its own: the next run of the step
//!   starts it again, from the kept downloads or from nothing.

use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};

use super::worker_io::{self, DrainedLog, SHUTDOWN_GRACE, exit_message};
use crate::ginux::{Outcome, ResultRecord};
use crate::inputs::PreparedInvocation;
use crate::worker::records::{WorkerRequest, WorkerResult};

/// Everything one configured launch needs.
pub(crate) struct LaunchSpec<'a> {
    pub prepared: &'a PreparedInvocation,
    /// The request record; its offline cache is set here, where the root
    /// tells whether the cache is bound.
    pub request: WorkerRequest,
    pub launcher: &'a Path,
    pub diagnostics: &'a crate::invocation_log::InvocationLog,
    pub image: &'a mut Option<crate::inputs::VerifiedImage>,
    /// The worker's digest the run's identity named (exact identity), or
    /// empty: the worker copied into the root must be that file.
    pub worker_digest: &'a str,
    /// The run directory: the supervisor's result record and the export
    /// archive are made in it.
    pub run_dir: &'a crate::temporary::TemporaryDirectory,
    /// Whether `restore_path` already holds the kept files the run starts
    /// from; otherwise a seeded target view may be written there.
    pub restore: bool,
    pub restore_path: &'a Path,
    /// Where the supervisor writes the export archive (`work`, `out`).
    pub export_path: &'a Path,
    /// The output timeout (`super::worker_io::output_timeout`).
    pub timeout: Duration,
    pub progress: &'a mut Option<crate::progress_file::ProgressFile>,
}

/// How one configured launch ended, with the worker's result.
pub(crate) struct RunOutcome {
    pub result: WorkerResult,
    /// Launcher exit code when it exited normally.
    pub launcher_code: Option<i32>,
    /// The supervisor's record: `exited`, `signaled` or `supervisor-error`.
    pub outcome: Outcome,
    /// The workload's exit code when `exited`.
    pub code: Option<i64>,
    pub log: DrainedLog,
}

/// Run one worker through `ginux launch` (see the module docs).
pub(crate) fn run(spec: LaunchSpec<'_>) -> Result<RunOutcome> {
    let LaunchSpec {
        prepared,
        mut request,
        launcher,
        diagnostics,
        image,
        worker_digest,
        run_dir,
        mut restore,
        restore_path,
        export_path,
        timeout,
        progress,
    } = spec;
    let invocation = prepared.invocation_id.clone();
    let result_path = run_dir.path().join("result.json");
    let config_path = prepared.config_path();
    let execution = request.options.execution.clone();

    let domain_root =
        crate::inputs::prepare_root(prepared, image, &mut request).map_err(|error| {
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
        restore = crate::inputs::seed_target(prepared, domain_root.path(), restore_path).map_err(
            |error| {
                crate::failure::Category::Startup
                    .wrap(error, "cannot seed the requested target view")
            },
        )?;
    }
    crate::cancellation::check()?;

    ensure_open_file_limit(execution.storage_max_objects);
    let mut command = Command::new(launcher);
    command.process_group(0);
    let parent = unsafe { libc::getpid() };
    let leases = [
        prepared.lease_fd(),
        domain_root.lease_fd(),
        run_dir.lease_fd(),
        diagnostics.lease_fd(),
    ];
    let binds = domain_root.binds().to_vec();
    let binding = !binds.is_empty();
    // Killing the coordinator must not orphan the supervisor. Ginux's
    // PTRACE_O_EXITKILL then tears down the supervised tasks.
    unsafe {
        command.pre_exec(move || {
            for fd in leases {
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
            bind_read_only(&binds)
        });
    }
    command
        .arg("launch")
        .arg(&config_path)
        .arg(domain_root.path())
        .arg("--result-file")
        .arg(&result_path)
        .arg("--export-file")
        .arg(export_path);
    if restore {
        command.arg("--restore-file").arg(restore_path);
    }
    let mut worker_diagnostics =
        std::fs::File::create(diagnostics.file(crate::invocation_log::WORKER_LOG))?;
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| {
            anyhow::Error::new(crate::failure::Category::Startup.error(format!(
                "cannot spawn ginux launcher {} for invocation \
                 {invocation}: {err:#}{}",
                launcher.display(),
                if binding {
                    " (while binding the system root read-only in a private mount namespace)"
                } else {
                    ""
                },
            )))
        })?;

    let stdout = child
        .stdout
        .take()
        .context(crate::failure::Category::Startup.error("launcher stdout was not piped"))?;
    let stderr = child
        .stderr
        .take()
        .context(crate::failure::Category::Startup.error("launcher stderr was not piped"))?;
    let log_path = diagnostics.file(crate::invocation_log::SUPERVISOR_LOG);
    let drain = worker_io::spawn_stderr_drain(stderr, log_path);

    let mut worker_started = false;
    let pump = worker_io::pump_lines(
        &invocation,
        &mut child,
        stdout,
        &mut worker_diagnostics,
        &mut worker_started,
        timeout,
        Duration::from_secs(execution.timeout_secs),
        progress,
    );
    let execution_deadline = match pump {
        Ok(deadline) => deadline,
        Err(err) => {
            // The stream error stays the result; teardown problems are reported beside it.
            let exit = worker_io::reap_launcher(&mut child, Instant::now() + SHUTDOWN_GRACE)
                .unwrap_or_else(|error| {
                    crate::diagnostics::diag!("recipe: warning: {error:#}");
                    None
                });
            let drained = worker_io::join_drain(drain);
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

    // The worker's output ended; the supervisor's final archive export
    // follows. Large valid exports still belong to execution, not short error
    // teardown.
    let reaped = worker_io::reap_launcher(&mut child, execution_deadline);
    let drained = worker_io::join_drain(drain);
    // Diagnostics are not fsynced (see `crate::invocation_log`).
    drop(worker_diagnostics);
    let exit = reaped?;

    let result = match worker_io::result_from_archive(export_path, &invocation, &execution) {
        Ok(result) => result,
        Err(reason) => {
            return Err(enrich_stream_error(
                &invocation,
                worker_io::missing_result(&invocation, &reason),
                exit,
                &drained,
                &result_path,
                worker_started,
            ));
        }
    };
    let record = ResultRecord::read(&result_path, &invocation)?;
    check_agreement(&invocation, &result, &record)?;
    check_launcher_exit(&invocation, &result, &record, exit)?;
    Ok(RunOutcome {
        result,
        launcher_code: exit,
        outcome: record.outcome,
        code: record.code,
        log: drained,
    })
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
    let hard = limit.rlim_max;
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

/// In the launcher's child, before it execs: bind each host folder of a live
/// system root read-only on its empty folder under the domain's `/inputs`
/// (`crate::inputs::InputKind::Bind`). The binds are made in a mount
/// namespace of the launcher's own, entered here, whose mounts are slaves of
/// the host's: nothing propagates back to the host, so the coordinator and
/// every other process see only the empty folders (the domain root is
/// removed as before), and the kernel drops the binds with the namespace when
/// the launcher and its tasks are gone, however the run ends. Each bind is
/// remounted read-only, nosuid and nodev; it is not recursive, so mounts below
/// the folder (`proc`, a separate `var`) are not in it. No allocation:
/// this runs between fork and exec.
fn bind_read_only(binds: &[(std::ffi::CString, std::ffi::CString)]) -> std::io::Result<()> {
    if binds.is_empty() {
        return Ok(());
    }
    let check = |result: libc::c_int| {
        if result == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    };
    let none = std::ptr::null::<libc::c_char>();
    unsafe {
        check(libc::unshare(libc::CLONE_NEWNS))?;
        check(libc::mount(
            none,
            c"/".as_ptr(),
            none,
            libc::MS_REC | libc::MS_SLAVE,
            std::ptr::null(),
        ))?;
        for (source, target) in binds {
            check(libc::mount(
                source.as_ptr(),
                target.as_ptr(),
                none,
                libc::MS_BIND,
                std::ptr::null(),
            ))?;
            check(libc::mount(
                none,
                target.as_ptr(),
                none,
                libc::MS_BIND
                    | libc::MS_REMOUNT
                    | libc::MS_RDONLY
                    | libc::MS_NOSUID
                    | libc::MS_NODEV,
                std::ptr::null(),
            ))?;
        }
    }
    Ok(())
}

/// Attach launcher exit, drained-log, and supervisor-record state to a
/// stream error or a missing result. A `supervisor-error` record underneath
/// distinguishes an unconfirmed startup from a supervisor failure after the
/// worker spoke; everything else stays `protocol` and never success.
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
    if crate::ginux::peek_outcome(result_path, invocation) == Some(Outcome::SupervisorError)
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

/// The worker's result and the authoritative supervisor record must agree.
/// A worker `ok` requires a clean `exited`/`0` record; a worker failure
/// requires the domain to have run it. Contradictions are `protocol`
/// failures, never success.
fn check_agreement(invocation: &str, result: &WorkerResult, record: &ResultRecord) -> Result<()> {
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

/// The launcher must exit cleanly once the worker succeeded: a nonzero or
/// signaled launcher exit under a worker success contradicts the record
/// (already checked) and is reported here with process detail. A launcher
/// that had to be killed after a completed run is a `protocol` failure.
fn check_launcher_exit(
    invocation: &str,
    result: &WorkerResult,
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
