//! One worker run in a Ginux instance, through the public lifecycle of the
//! `ginux` CLI (`docs/ginux-cutover-design.md`, section 1).
//!
//! [`run`] is this module's one entry point and the only thing the rest of
//! Recipe calls (`super::run_domain`). It takes the prepared invocation, the
//! request record, the [`Ginux`] to use and the run's paths, and returns the
//! run's outcome with the worker's result ([`RunOutcome`]); every failure
//! carries a [`crate::failure::Category`]. The decision to run at all, the
//! kept state, publication and logs are the caller's.
//!
//! What it does, in order:
//!
//! - Once per process, a non-root caller's sudo rule for Recipe's guard is
//!   checked ([`Ginux::require_privilege`]): without it the run stops with
//!   the way forward before anything privileged runs.
//! - Once per process, deletes the instances earlier Recipe processes left
//!   behind ([`Ginux::sweep`]).
//! - The image the invocation names is in the state root's cache, imported
//!   from its layout when it is not ([`Ginux::ensure_image`]).
//! - Everything the worker needs (inputs, request file, worker, kept files,
//!   `/work`, `/out`, `/etc/hosts`) is written into one tar
//!   (`crate::inputs::inject_archive`).
//! - A stopped instance with a fresh name ([`crate::ginux::instance_name`])
//!   is created from the invocation's config, and the tar injected into its
//!   root. From then on the instance is deleted however the run ends
//!   (success, failure, cancellation, panic): [`Instance`]'s drop.
//! - `ginux start NAME --wait --stream-console --full-audit` runs in its own
//!   process group, dies with the coordinator (`PDEATHSIG`; the helper then
//!   tears the instance down), and its standard error is drained to
//!   `ginux-cli.log`.
//!   The worker's line records arrive on its standard output, the instance
//!   console the helper copies there ([`super::worker_io::ConsoleStream`]),
//!   under the stall, execution, free-space and cancellation rules of
//!   [`super::worker_io::pump_lines`]. Any of those
//!   ends it: SIGTERM to its group (`sudo` relays it to the helper, whose
//!   teardown takes at most about 7.5 s; the CLI dies by the signal after its
//!   own 1 s relay grace), then SIGKILL after [`SHUTDOWN_GRACE`].
//! - After it exited: `status --format json` is the authority on how the run
//!   ended; the CLI's exit code is recorded and checked against it, never
//!   trusted alone. The instance directory is root-private: its audit log
//!   (worker standard error, runtime audit; the last 16 MiB), which the
//!   helper appends to the CLI's standard error after a marker line on every
//!   outcome, becomes the invocation's supervisor log
//!   ([`super::worker_io::split_cli_log`]).
//! - Only a workload that exited has a result: `/work` and `/out` are
//!   extracted into the export archive, the worker's result file is read
//!   from it and must agree with the outcome. No valid result is classified
//!   from the outcome and the CLI's exit.
//! - An interrupted run keeps nothing of its own: the next run of the step
//!   starts it again, from the kept downloads or from nothing.

use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};

use super::worker_io::{self, DrainedLog, SHUTDOWN_GRACE, exit_message};
use crate::failure::Category;
use crate::ginux::{Ginux, Outcome, OutcomeRecord, exit};
use crate::inputs::PreparedInvocation;
use crate::invocation_log::{CLI_LOG, INSTANCE_RECORD};
use crate::worker::records::{WorkerRequest, WorkerResult};

/// The directories extracted after a run.
const EXTRACTED: [&str; 2] = [crate::domain::WORK, crate::domain::OUT];
/// Slack on the injection bounds over what the tar holds.
const INJECT_SLACK_BYTES: u64 = 1 << 20;
const INJECT_SLACK_MEMBERS: u64 = 64;

/// Everything one run needs.
pub(crate) struct LaunchSpec<'a> {
    pub prepared: &'a PreparedInvocation,
    /// The request record; the injected tar completes it (offline cache).
    pub request: WorkerRequest,
    pub ginux: &'a Ginux,
    pub diagnostics: &'a crate::invocation_log::InvocationLog,
    /// The worker's digest the run's identity named (exact identity), or
    /// empty: the worker injected must be that file.
    pub worker_digest: &'a str,
    /// The run directory: the injected tar and the export archive are made
    /// in it.
    pub run_dir: &'a crate::temporary::TemporaryDirectory,
    /// Whether `restore_path` already holds the kept files the run starts
    /// from.
    pub restore: bool,
    pub restore_path: &'a Path,
    /// Where `extract` writes the export archive (`work`, `out`).
    pub export_path: &'a Path,
    /// The output timeout (`super::worker_io::output_timeout`).
    pub timeout: Duration,
    pub progress: &'a mut Option<crate::progress_file::ProgressFile>,
}

/// How one run ended, with the worker's result.
pub(crate) struct RunOutcome {
    pub result: WorkerResult,
    /// The exit code of `ginux start --wait` when it exited normally.
    pub launcher_code: Option<i32>,
    /// The supervisor's record: `exited`, `signaled` or `supervisor-error`.
    pub outcome: Outcome,
    /// The workload's exit code when `exited`.
    pub code: Option<i64>,
    pub log: DrainedLog,
}

/// Run one worker in a fresh Ginux instance (see the module docs).
pub(crate) fn run(spec: LaunchSpec<'_>) -> Result<RunOutcome> {
    let LaunchSpec {
        prepared,
        mut request,
        ginux,
        diagnostics,
        worker_digest,
        run_dir,
        restore,
        restore_path,
        export_path,
        timeout,
        progress,
    } = spec;
    let invocation = prepared.invocation_id.clone();
    ginux.require_privilege()?;
    static SWEEP: std::sync::Once = std::sync::Once::new();
    SWEEP.call_once(|| ginux.sweep());

    let execution = request.options.execution.clone();
    let budget = Duration::from_secs(execution.timeout_secs);
    let startup = |error: anyhow::Error, what: &str| {
        Category::Startup.wrap(error, format!("{what} for invocation {invocation}"))
    };

    ginux
        .ensure_image_from(&prepared.root_reference, || {
            crate::runtime::ensure_layout(&prepared.root_reference, prepared.root_image)
        })
        .map_err(|error| startup(error, "cannot provide the Ginux image"))?;
    crate::cancellation::check()?;

    let tar = run_dir.path().join("inject.tar");
    let staged = crate::inputs::inject_archive(
        prepared,
        &mut request,
        restore.then_some(restore_path),
        &tar,
    )
    .map_err(|error| startup(error, "cannot stage the inputs"))?;
    if !worker_digest.is_empty() && staged.worker_digest != worker_digest {
        bail!(Category::Startup.error(format!(
            "the worker changed while invocation {invocation} started; run it again"
        )));
    }
    crate::cancellation::check()?;

    let instance = Instance::create(ginux, &prepared.config_path(), diagnostics)?;
    let name = instance.name.as_str();
    let inject = ginux.inject(
        name,
        &tar,
        (staged.bytes as u64).saturating_add(INJECT_SLACK_BYTES),
        (staged.members as u64).saturating_add(INJECT_SLACK_MEMBERS),
        budget,
    )?;
    if !inject.ok() {
        bail!(Category::Startup.error(format!(
            "cannot inject the inputs of invocation {invocation} into the Ginux instance \
             {name}: {}{}",
            inject.message(),
            if inject.code == Some(exit::INVALID) {
                " (exit 65: the image already holds a path Recipe injects, or the tar holds \
                 something the injector refuses)"
            } else {
                ""
            }
        )));
    }
    let _ = std::fs::remove_file(&tar);
    crate::cancellation::check()?;

    let mut command = ginux.start_command(name);
    command.process_group(0);
    let parent = unsafe { libc::getpid() };
    // Killing the coordinator must not leave the instance running: the CLI
    // dies with it, and the helper tears the instance down when its caller
    // is gone. No allocation: this runs between fork and exec.
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
    let mut worker_diagnostics =
        std::fs::File::create(diagnostics.file(crate::invocation_log::WORKER_LOG))?;
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            anyhow::Error::new(Category::Startup.error(format!(
                "cannot run {} start {name} --wait --stream-console --full-audit for invocation \
                 {invocation}: {error}",
                ginux.cli().display()
            )))
        })?;
    let stderr = child
        .stderr
        .take()
        .context(Category::Startup.error("ginux start stderr was not piped"))?;
    let cli_drain = worker_io::spawn_stderr_drain(
        stderr,
        diagnostics.file(CLI_LOG),
        worker_io::CLI_LOG_CAP_BYTES,
    );
    let stdout = child
        .stdout
        .take()
        .context(Category::Startup.error("ginux start stdout was not piped"));
    let console = stdout.and_then(|stdout| {
        worker_io::ConsoleStream::new(stdout, ginux.state_root().to_path_buf()).map_err(|error| {
            Category::Startup
                .error(format!("cannot read the console of {name}: {error}"))
                .into()
        })
    });
    let mut console = match console {
        Ok(console) => console,
        Err(error) => {
            crate::ginux::stop_group(&mut child, SHUTDOWN_GRACE);
            let _ = worker_io::join_drain(cli_drain);
            return Err(error);
        }
    };
    let mut worker_started = false;
    let pump = worker_io::pump_lines(
        &invocation,
        &mut child,
        &mut console,
        &mut worker_diagnostics,
        &mut worker_started,
        timeout,
        budget,
        progress,
    );
    let pumped = pump.map_err(|error| match console.low_disk() {
        Some(reason) => Category::Storage.error(reason.to_owned()).into(),
        None => error,
    });
    // On a stream error the CLI is still running: end it (the helper tears
    // the instance down). After end of stream it has exited already.
    let status = crate::ginux::stop_group(&mut child, SHUTDOWN_GRACE);
    let cli_exit = status.and_then(|status| status.code());
    let cli_drained = worker_io::join_drain(cli_drain);
    // Diagnostics are not fsynced (see `crate::invocation_log`).
    drop(worker_diagnostics);

    let record = match ginux.status(name) {
        Ok(status) if !status.running => Some(
            status
                .outcome
                .unwrap_or_else(OutcomeRecord::supervisor_error),
        ),
        Ok(_) => {
            crate::diagnostics::diag!(
                "recipe: warning: the Ginux instance {name} still runs after ginux start \
                 exited; it is stopped and deleted"
            );
            None
        }
        Err(error) => {
            crate::diagnostics::diag!(
                "recipe: warning: cannot read the outcome of the Ginux instance {name}: {error:#}"
            );
            None
        }
    };
    let (mut drained, cli_tail) = worker_io::split_cli_log(
        &diagnostics.file(CLI_LOG),
        &diagnostics.file(crate::invocation_log::SUPERVISOR_LOG),
        console.runtime_text(),
        &cli_drained,
    );
    if !cli_tail.is_empty() {
        drained.tail = if drained.tail.is_empty() {
            format!("[ginux start] {cli_tail}")
        } else {
            format!("{}\n[ginux start] {cli_tail}", drained.tail)
        };
    }

    if let Err(error) = pumped {
        return Err(enrich_stream_error(
            &invocation,
            error,
            cli_exit,
            &drained,
            record.as_ref(),
            worker_started,
        ));
    }
    let Some(record) = record else {
        return Err(enrich_stream_error(
            &invocation,
            worker_io::missing_result(&invocation, "Ginux recorded no outcome for the run"),
            cli_exit,
            &drained,
            None,
            worker_started,
        ));
    };
    if record.outcome != Outcome::Exited {
        return Err(enrich_stream_error(
            &invocation,
            worker_io::missing_result(
                &invocation,
                &format!("the workload ended {}", record.describe()),
            ),
            cli_exit,
            &drained,
            Some(&record),
            worker_started,
        ));
    }

    extract(ginux, name, export_path, &execution, budget)?;
    let result = match worker_io::result_from_archive(export_path, &invocation, &execution) {
        Ok(result) => result,
        Err(reason) => {
            return Err(enrich_stream_error(
                &invocation,
                worker_io::missing_result(&invocation, &reason),
                cli_exit,
                &drained,
                Some(&record),
                worker_started,
            ));
        }
    };
    check_agreement(&invocation, &result, &record)?;
    check_cli_exit(&invocation, &result, &record, cli_exit)?;
    instance.delete();
    Ok(RunOutcome {
        result,
        launcher_code: cli_exit,
        outcome: record.outcome,
        code: record.code,
        log: drained,
    })
}

/// A created instance, deleted when this value goes, however `run` leaves.
struct Instance<'a> {
    ginux: &'a Ginux,
    name: String,
    deleted: bool,
}

impl<'a> Instance<'a> {
    /// Create a stopped instance from `config` under a fresh name, and
    /// record the name beside the invocation's logs so a reader can map one
    /// to the other.
    fn create(
        ginux: &'a Ginux,
        config: &Path,
        diagnostics: &crate::invocation_log::InvocationLog,
    ) -> Result<Self> {
        let name = crate::ginux::instance_name();
        ginux.create(&name, config)?;
        let instance = Self {
            ginux,
            name,
            deleted: false,
        };
        let record = serde_json::json!({ "instance": instance.name });
        if let Err(error) = std::fs::write(
            diagnostics.file(INSTANCE_RECORD),
            serde_json::to_vec(&record)?,
        ) {
            crate::diagnostics::diag!(
                "recipe: warning: cannot record the instance name {}: {error}",
                instance.name
            );
        }
        Ok(instance)
    }

    fn delete(mut self) {
        self.remove();
    }

    fn remove(&mut self) {
        if std::mem::replace(&mut self.deleted, true) {
            return;
        }
        if let Err(error) = self.ginux.delete(&self.name) {
            crate::diagnostics::diag!(
                "recipe: warning: the Ginux instance {} could not be deleted: {error:#}; \
                 a later Recipe run deletes it",
                self.name
            );
        }
    }
}

impl Drop for Instance<'_> {
    fn drop(&mut self) {
        self.remove();
    }
}

/// `extract` `/work` and `/out` into `export_path`, bounded like the
/// publication that reads it. This is where `storage_max_bytes` is enforced
/// now, after the run (Ginux bounds no instance storage). A member that
/// changed while it was read (75) is retried once.
fn extract(
    ginux: &Ginux,
    name: &str,
    export_path: &Path,
    execution: &crate::ExecutionOptions,
    budget: Duration,
) -> Result<()> {
    let max_members = execution.storage_max_objects.saturating_mul(3);
    for attempt in 0..2 {
        let call = ginux.extract(
            name,
            export_path,
            execution.archive_bound(),
            max_members,
            &EXTRACTED,
            budget,
        )?;
        match call.code {
            Some(0) => return Ok(()),
            Some(exit::CHANGED) if attempt == 0 => {}
            Some(exit::BOUND) => bail!(Category::Storage.error(format!(
                "the run's outputs exceed their bound ({} bytes, {max_members} entries): {}",
                execution.archive_bound(),
                call.message()
            ))),
            Some(exit::INVALID) => bail!(Category::Workload.error(format!(
                "the run left a file Recipe cannot take in /work or /out (a FIFO, socket or \
                 device): {}",
                call.message()
            ))),
            _ => bail!(Category::Supervisor.error(format!(
                "cannot extract the run's files from the Ginux instance {name}: {}",
                call.message()
            ))),
        }
    }
    bail!(Category::Supervisor.error(format!(
        "the files of the Ginux instance {name} kept changing while they were extracted"
    )))
}

/// Attach the CLI's exit, the log tail and the outcome to a stream error or
/// a missing result. A `supervisor-error` outcome underneath distinguishes
/// an unconfirmed startup from a supervisor failure after the worker spoke;
/// everything else stays as it is and never success.
fn enrich_stream_error(
    invocation: &str,
    err: anyhow::Error,
    exit: Option<i32>,
    drained: &DrainedLog,
    record: Option<&OutcomeRecord>,
    worker_started: bool,
) -> anyhow::Error {
    let text = format!("{err:#}");
    let category = Category::of(&err);
    if record.is_some_and(|record| record.outcome == Outcome::SupervisorError)
        && category == Some(Category::Protocol)
    {
        let (category, timing) = if worker_started {
            (Category::Supervisor, "after worker execution started")
        } else {
            (Category::Startup, "before worker startup was confirmed")
        };
        return category
            .error(format!(
                "invocation {invocation}: supervisor aborted {timing}: {text}; ginux start exit \
                 {}; log tail: {}",
                exit_message(exit),
                drained.tail
            ))
            .into();
    }
    let detail = format!(
        "; invocation {invocation} ginux start exit {}; {}; log tail: {}",
        exit_message(exit),
        record
            .map(|record| format!("outcome {}", record.describe()))
            .unwrap_or_else(|| "no outcome was recorded".to_string()),
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

/// The worker's result and the authoritative outcome must agree. A worker
/// `ok` requires a clean `exited`/`0` outcome; a worker failure requires the
/// domain to have run it. Contradictions are `protocol` failures, never
/// success.
fn check_agreement(invocation: &str, result: &WorkerResult, record: &OutcomeRecord) -> Result<()> {
    if result.ok {
        if record.outcome == Outcome::SupervisorError {
            bail!(Category::Supervisor.error(format!(
                "invocation {invocation} worker reported success but the \
                 supervisor failed during finalization (refusing publication)"
            )));
        }
        if record.outcome != Outcome::Exited || record.code != Some(0) {
            bail!(Category::Protocol.error(format!(
                "invocation {invocation} worker reported success but \
                 the authoritative result is {}",
                record.describe()
            )));
        }
        return Ok(());
    }
    if record.outcome == Outcome::SupervisorError {
        bail!(Category::Supervisor.error(format!(
            "invocation {invocation} worker reported failure and the \
             supervisor also failed during finalization (refusing publication)"
        )));
    }
    Ok(())
}

/// `ginux start --wait` must exit cleanly once the worker succeeded (its
/// code is the workload's); one that had to be killed after a completed run
/// is a `protocol` failure.
fn check_cli_exit(
    invocation: &str,
    result: &WorkerResult,
    record: &OutcomeRecord,
    exit: Option<i32>,
) -> Result<()> {
    if result.ok {
        if exit != Some(0) {
            bail!(Category::Protocol.error(format!(
                "invocation {invocation} worker reported success but ginux start exited with {} \
                 (refusing publication)",
                exit_message(exit),
            )));
        }
        return Ok(());
    }
    if record.outcome == Outcome::Signaled {
        bail!(Category::Workload.error(format!(
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
        bail!(Category::Workload.error(format!(
            "invocation {invocation} worker exited with code {} (ginux start exit {}): {detail}",
            record.code.unwrap_or(-1),
            exit_message(exit),
        )));
    }
    Ok(())
}
