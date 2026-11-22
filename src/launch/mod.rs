//! Running one recipe operation: what the coordinator decides and keeps,
//! independent of how the worker runs.
//!
//! [`run_domain`] runs an operation in a Ginux domain, `crate::host::run_host`
//! on the host; both share everything here: the start decision from
//! Recipe's records ([`start`]), the result of a run with nothing missing
//! ([`done_report`]), validation, keeping and publication of a finished
//! run's outputs ([`finish_outputs`]), the end of an install ([`complete`])
//! and the invocation log ([`recorded`]).
//!
//! The module split (`docs/ginux-public-lifecycle-migration.md`, section 4):
//!
//! - [`configured`]: builds and runs `ginux launch`; its one entry point,
//!   [`configured::run`], is the only way into it. The move to upstream's
//!   public lifecycle replaces that module and nothing else here.
//! - [`worker_io`]: the coordinator's side of a running worker that both
//!   runtimes share: the line records on its standard output, its standard
//!   error log, reaping, and reading its result file
//!   ([`crate::worker::records`]).
//!
//! No path here evaluates recipe code on the host.

mod configured;
pub(crate) mod worker_io;

use std::path::Path;

use anyhow::{Context, Result};

use crate::inputs::PreparedInvocation;
use crate::worker::records::{WorkerRequest, WorkerResult};

/// One executable operation, as the coordinator runs it.
pub(crate) struct Invocation<'a> {
    pub operation: crate::worker::records::Operation,
    pub recipe: &'a Path,
    pub search_path: Option<&'a Path>,
    pub persist_state: bool,
    pub options: crate::worker::records::InvocationOptions,
    pub inputs: &'a [crate::inputs::ExtraInput],
    /// Lock and saved-state scope key.
    pub destination: &'a Path,
    pub state_dir: Option<&'a Path>,
    /// Each dependency's declared output names, for the worker's `DEPS`
    /// (`crate::worker::records::WorkerRequest::dependencies`).
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
    /// The worker's result (one constructed by the coordinator when no
    /// worker ran, [`done_report`]).
    pub result: WorkerResult,
    /// A successful run's result for the caller ([`finish_outputs`]); None
    /// for a failed one.
    pub summary: Option<serde_json::Value>,
    /// Launcher exit code when it exited normally (`None` when killed or
    /// signaled).
    pub launcher_code: Option<i32>,
    /// How the run ended (`exited`, `signaled`, or `supervisor-error`): the
    /// supervisor's record in Ginux, the worker's exit on the host.
    pub result_outcome: crate::ginux::Outcome,
    /// The workload's exit code when `exited`.
    pub result_code: Option<i64>,
    /// Total launcher/worker stderr bytes observed (the retained portion is
    /// capped at `crate::invocation_log::MAX_LOG_BYTES`).
    pub log_total_bytes: u64,
    /// Launcher/worker stderr bytes retained beyond the cap (drained and
    /// discarded).
    pub log_discarded_bytes: u64,
}

/// Run the prepared invocation in a Ginux domain ([`configured::run`]).
///
/// Restore the kept files the run starts from. No path here evaluates recipe
/// code on the host.
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
/// (worker result, network use) after, for either mode.
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
                "worker": report.result,
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
        key: invocation_key,
        ..
    } = *invocation;
    let run = invocation;
    let invocation = prepared.invocation_id.clone();
    let timeout = worker_io::output_timeout()
        .map_err(|error| crate::failure::Category::Startup.wrap(error, "invalid output timeout"))?;
    let _destination_lock =
        crate::retained::lock_destination(destination, timeout).map_err(|error| {
            crate::failure::Category::Storage.wrap(error, "cannot lock the destination")
        })?;
    let run_dir = crate::temporary::TemporaryDirectory::new()
        .context(crate::failure::Category::Startup.error("cannot create ginux run directory"))?;
    let export_path = run_dir.path().join("export.tar");

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
                operation == crate::worker::records::Operation::Download,
            )
            .map_err(|error| {
                crate::failure::Category::Storage.wrap(error, "cannot open saved state")
            })?,
        )
    } else {
        None
    };
    // What is done already, from Recipe's records: nothing to run, or a run
    // that starts from the kept downloads, or from nothing.
    let restore_path = run_dir.path().join("restore.tar");
    let restore = match start(retained.as_ref(), run, prepared, &restore_path)? {
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
        Start::Run { restore } => restore,
    };

    let mut progress = worker_io::progress_file(&options.execution, &invocation, recipe, operation);
    let outcome = configured::run(configured::LaunchSpec {
        prepared,
        // The offline cache is filled in where the root is made, which
        // decides whether it is bound (`crate::inputs::prepare_root`).
        request: request(prepared, run, None, None),
        launcher,
        diagnostics,
        image,
        worker_digest: &worker_digest,
        run_dir: &run_dir,
        restore,
        restore_path: &restore_path,
        export_path: &export_path,
        timeout,
        progress: &mut progress,
    });
    if let Some(progress) = progress.as_mut() {
        progress.finish(outcome.as_ref().is_ok_and(|outcome| outcome.result.ok));
    }
    let outcome = outcome?;
    let mut summary = None;
    if outcome.result.ok {
        let usage = crate::needs::report::network_usage(
            &prepared.network.host_endpoints,
            diagnostics,
            outcome.log.discarded_bytes,
        );
        summary = Some(finish_outputs(
            &outcome.result,
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
        result: outcome.result,
        summary,
        launcher_code: outcome.launcher_code,
        result_outcome: outcome.outcome,
        result_code: outcome.code,
        log_total_bytes: outcome.log.total_bytes,
        log_discarded_bytes: outcome.log.discarded_bytes,
    })
}

/// The request record of one run (`crate::worker::records::WorkerRequest`),
/// the same for both runtimes but for a host run's `host` and the offline
/// cache each runtime knows.
pub(crate) fn request(
    prepared: &PreparedInvocation,
    run: &Invocation<'_>,
    host: Option<crate::worker::records::HostRun>,
    cache: Option<String>,
) -> WorkerRequest {
    WorkerRequest {
        version: crate::worker::records::RECORDS_VERSION,
        invocation_id: prepared.invocation_id.clone(),
        recipe_name: prepared.declaration.name().to_owned(),
        operation: run.operation,
        options: run.options.clone(),
        dependencies: run.dependencies.clone(),
        host,
        cache,
    }
}

/// What Recipe's records leave to do for one run (docs/recipe-format.md,
/// section 6.2): decided before any worker starts, never from recipe code.
pub(crate) enum Start {
    /// Nothing is missing: a download already kept for `download`, a kept
    /// result for `build`, or for `install` a kept result published from the
    /// restore input (`publish`). No worker runs.
    Done { publish: bool },
    /// A worker runs; `restore` when the restore input holds the kept
    /// downloads of this state key it starts from.
    Run { restore: bool },
}

impl Start {
    fn fresh() -> Self {
        Self::Run { restore: false }
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
    use crate::worker::records::Operation;
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
                true => Start::Run { restore: true },
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
        (_, Some(Reached::Downloaded)) => {
            match retained.restore_input(restore_path).map_err(storage)? {
                true => Start::Run { restore: true },
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
    run.operation == crate::worker::records::Operation::Install
        && run.finish.is_some_and(|finish| !finish.keep)
}

/// The recipe's own `plan` event for a front end, once Recipe knows what is
/// done: the steps that will run (the others are not needed this time).
fn plan_event(
    prepared: &PreparedInvocation,
    operation: crate::worker::records::Operation,
    start: &Start,
    cleans: bool,
) {
    use crate::worker::records::Operation;
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
    operation: crate::worker::records::Operation,
    published: bool,
    cleanup: crate::core::events::StepStatus,
) -> Vec<crate::core::events::StepReport> {
    use crate::core::events::{StepReport, StepStatus};
    use crate::worker::records::Operation;
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
    options: &crate::worker::records::InvocationOptions,
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
    let cleanup = if publish && operation == crate::worker::records::Operation::Install {
        complete(run, &mut summary)?
    } else {
        crate::core::events::StepStatus::Skipped
    };
    let steps = finish_steps(prepared, &[], operation, publish, cleanup);
    summary["steps"] = serde_json::to_value(&steps)?;
    let result = WorkerResult {
        version: crate::worker::records::RECORDS_VERSION,
        invocation_id: invocation.to_owned(),
        ok: true,
        failure: None,
        outputs: if publish { kept } else { Default::default() },
        steps: steps.clone(),
        latest: None,
        network_usage: None,
    };
    Ok(LaunchReport {
        result,
        summary: Some(summary),
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
    result: &WorkerResult,
    export_path: &Path,
    tree: Option<&Path>,
    retained: &mut Option<crate::retained::Retained>,
    prepared: &PreparedInvocation,
    invocation: &str,
    run: &Invocation<'_>,
    usage: serde_json::Value,
) -> Result<serde_json::Value> {
    use crate::retained::Reached;
    use crate::worker::records::Operation;
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
    // and no longer reads it. A RERUN recipe keeps no result.
    if let (Some(state), Some((reached, outputs))) = (retained.as_mut(), keep) {
        state
            .finish(export_path, reached, outputs)
            .map_err(|error| {
                crate::failure::Category::Storage.wrap(error, "cannot keep the run's files")
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
/// at this destination (downloads and the kept result) under
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
