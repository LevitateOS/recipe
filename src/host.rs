//! A host run: one recipe operation without Ginux (`crate::execution`).
//!
//! The coordinator does what a Ginux launch does, with host directories in
//! place of the domain root:
//!
//! - `ROOT/{inputs,work/{build,tools,tmp},out}` in the scratch pool. `ROOT`
//!   is the same path on every run for a destination (`invocation-host-…`),
//!   so absolute paths in the kept files stay valid when a run starts from
//!   them. Inputs are staged and digest-checked exactly as for a domain root.
//! - The worker is the same `recipe` binary, started directly as a child: its
//!   own process group, killed with the coordinator (PDEATHSIG), its address
//!   space bounded by `memory_mib` (RLIMIT_AS), its working directory `work`,
//!   and the worker environment with host paths. It reads the same request
//!   file (staged with the inputs), writes the same line records and the same
//!   result file (`crate::worker::records`); `RECIPE_HOST_ROOT`
//!   (`crate::domain::ENV_HOST_ROOT`) tells it where its directories are and
//!   the request's `host` field what it may reach. It installs the path rule
//!   (`crate::helpers::confine`), the network rule
//!   (`crate::helpers::acquire::net`) and a seccomp filter that refuses
//!   `execve`, and it has no program helpers.
//! - Once the worker has exited, the coordinator reads its result file from
//!   `work` by descriptor (`crate::launch::worker_io::result_from_tree`). At
//!   the end the coordinator archives `work` and `out` in the format a
//!   Ginux export uses (members `work/…`, `out/…`), and `crate::retained`
//!   keeps it, as for Ginux. Restore unpacks a kept archive (or the seeded
//!   target) before the worker starts.
//! - Storage bounds are checked when the final archive is made, over
//!   everything in `work` and `out` (coarse: a run may exceed them while it
//!   runs).
//! - The outputs are validated and moved into publication from the run's
//!   directory where it shares a filesystem with the state root
//!   (`crate::publication::prepare_tree`); elsewhere the final archive is the
//!   export `crate::publication` validates and unpacks, exactly as a Ginux
//!   export. Saved state keeps the final archive as the kept result.
//! - The deadline and cancellation kill the worker's process group.
//! - Identity: the worker never keeps privilege; a root run's worker has a
//!   uid and gid of its own and only it reaches the run's directories
//!   (`identity`). Restores and exports walk by descriptor
//!   (`snapshot`).
//! - Resource limits on the worker: `memory_mib` (RLIMIT_AS), no core files,
//!   1024 descriptors, and files no larger than `NEEDS.storage_max_bytes`
//!   when that is set (RLIMIT_FSIZE; SIGXFSZ is ignored, so the write fails
//!   instead of killing the worker).

mod identity;
mod snapshot;

pub(crate) use identity::clear_capabilities;
use identity::{Gate, Limits};
use snapshot::{check_storage, unpack_snapshot, write_snapshot_of};

use std::fs::{self, File};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Instant;

use anyhow::{Context, Result, bail};

use crate::failure::Category;
use crate::inputs::PreparedInvocation;
use crate::launch::{Invocation, LaunchReport};

/// Run one prepared operation on the host.
pub(crate) fn run_host(
    prepared: &PreparedInvocation,
    invocation: &Invocation<'_>,
) -> Result<LaunchReport> {
    crate::launch::recorded(prepared, invocation, |diagnostics| {
        run_inner(prepared, invocation, diagnostics)
    })
}

fn run_inner(
    prepared: &PreparedInvocation,
    invocation: &Invocation<'_>,
    diagnostics: &crate::invocation_log::InvocationLog,
) -> Result<LaunchReport> {
    let Invocation {
        operation,
        recipe,
        persist_state,
        ref options,
        destination,
        state_dir,
        key,
        ..
    } = *invocation;
    let invocation_id = prepared.invocation_id.clone();
    prepared
        .network
        .check_host_reachable()
        .map_err(|error| Category::Startup.wrap(error, "refused network endpoint"))?;
    let timeout = crate::launch::worker_io::output_timeout()
        .map_err(|error| Category::Startup.wrap(error, "invalid output timeout"))?;
    let _destination_lock = crate::retained::lock_destination(destination, timeout)
        .map_err(|error| Category::Storage.wrap(error, "cannot lock the destination"))?;

    // The run's directories: one stable path per destination.
    use sha2::Digest;
    let scope = destination.canonicalize()?;
    let name = hex::encode(sha2::Sha256::digest(scope.as_os_str().as_encoded_bytes()));
    let unprivileged = unsafe { libc::geteuid() } == 0;
    let scratch =
        crate::temporary::TemporaryDirectory::named_for_host(&format!("host-{}", &name[..32]))
            .map_err(|error| {
                Category::Startup.wrap(error, "cannot create the host run directory")
            })?;
    let root = scratch.path().canonicalize()?;
    // A root run's own uid and gid, held until the run ends.
    let slot = if unprivileged {
        let pool = scratch
            .container()
            .parent()
            .context("the host run directory has no pool")?;
        Some(
            identity::take_slot(pool, scratch.container()).map_err(|error| {
                Category::Startup.wrap(error, "cannot take a host run identity")
            })?,
        )
    } else {
        None
    };
    let identity = slot.as_ref().map(|slot| slot.id);
    let startup = |error: anyhow::Error| {
        anyhow::Error::new(Category::Startup.error(format!(
            "cannot prepare the host run of invocation {invocation_id}: {error:#}"
        )))
    };
    let run_started = Instant::now();
    let inputs = root.join(crate::domain::INPUTS.trim_start_matches('/'));
    // Only a Ginux launcher binds a folder (`crate::inputs::prepare_invocation`
    // copies a live root for a host run).
    if prepared.binds().next().is_some() {
        return Err(startup(anyhow::anyhow!("a host run cannot bind an input")));
    }
    // The worker learns its root from the environment (`ENV_HOST_ROOT`),
    // which it reads as UTF-8.
    root.to_str()
        .context("the host run directory is not UTF-8")
        .map_err(startup)?;
    let request = crate::launch::request(
        prepared,
        invocation,
        Some(crate::worker::records::HostRun {
            network: prepared.network.host_grants(),
            network_max_bytes: prepared.execution.network_max_bytes,
        }),
        offline_cache_text().map_err(startup)?,
    );
    crate::inputs::stage_inputs(prepared, &inputs, &request).map_err(startup)?;
    for path in crate::domain::WRITABLE_DIRS {
        let directory = root.join(path.trim_start_matches('/'));
        fs::create_dir(&directory).map_err(|e| startup(e.into()))?;
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o755))
            .map_err(|e| startup(e.into()))?;
    }
    crate::inputs::seal_inputs(&inputs).map_err(startup)?;
    let worker = crate::inputs::worker_binary().map_err(startup)?;
    let worker_digest = if options.execution.exact_identity {
        crate::retained::file_digest(&worker).map_err(startup)?
    } else {
        String::new()
    };

    let mut retained = if persist_state {
        Some(
            crate::retained::Retained::open(
                state_dir,
                destination,
                recipe,
                key,
                prepared,
                &worker_digest,
                None,
                options,
                Some(&root),
                operation == crate::worker::records::Operation::Download,
            )
            .map_err(|error| Category::Storage.wrap(error, "cannot open saved state"))?,
        )
    } else {
        None
    };
    let run_dir = crate::temporary::TemporaryDirectory::new()
        .context(Category::Startup.error("cannot create the host run's scratch directory"))?;
    let restore_path = run_dir.path().join("restore.tar");
    let export_path = run_dir.path().join("export.tar");
    // What is done already, from Recipe's records: nothing to run, or a run
    // that starts from the kept downloads, or from nothing.
    let mut restore =
        match crate::launch::start(retained.as_ref(), invocation, prepared, &restore_path)? {
            crate::launch::Start::Done { publish } => {
                return crate::launch::done_report(
                    &retained,
                    &restore_path,
                    publish,
                    prepared,
                    &invocation_id,
                    invocation,
                );
            }
            crate::launch::Start::Run { restore } => restore,
        };
    if !restore {
        restore = crate::inputs::seed_target(prepared, &root, &restore_path).map_err(|error| {
            Category::Startup.wrap(error, "cannot seed the requested target view")
        })?;
    }
    if restore {
        unpack_snapshot(&restore_path, &root)
            .map_err(|error| Category::Storage.wrap(error, "cannot restore saved files"))?;
    }
    crate::cancellation::check()?;
    // A root run: hand work and out to the worker's identity, then open the
    // run's directory to it (and to nobody else). It is closed again once
    // the worker has exited, before root reads anything there.
    let gate = match identity {
        Some(id) => {
            identity::hand_over(scratch.container(), &root, id).map_err(startup)?;
            Some(Gate::open(scratch.container(), id).map_err(startup)?)
        }
        None => None,
    };

    let mut command = Command::new(&worker);
    command
        .args(crate::domain::WORKER_HOST_ARGS)
        .env_clear()
        .envs(environment(&root, &invocation_id))
        .current_dir(root.join(crate::domain::WORK.trim_start_matches('/')))
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let parent = unsafe { libc::getpid() };
    let limits = Limits {
        memory: options.execution.memory_mib.saturating_mul(1024 * 1024),
        file_size: options.execution.storage_max_bytes,
    };
    unsafe {
        command.pre_exec(move || identity::enter_worker_identity(identity, parent, limits));
    }
    let mut worker_diagnostics = File::create(diagnostics.file(crate::invocation_log::WORKER_LOG))?;
    let mut child = command.spawn().map_err(|error| {
        let hint = match error.raw_os_error() {
            Some(libc::EACCES) if unprivileged => format!(
                " (a root run starts its worker as its own unprivileged uid {}, which must be \
                 able to execute it: install recipe where every user can run it, e.g. /usr/bin, \
                 or point RECIPE_WORKER_BIN there)",
                identity.unwrap_or_default()
            ),
            Some(libc::EPERM) if unprivileged => format!(
                " (a root run starts its worker as its own unprivileged uid {} without \
                 capabilities, which needs CAP_SETUID, CAP_SETGID and CAP_SETPCAP)",
                identity.unwrap_or_default()
            ),
            _ => String::new(),
        };
        anyhow::Error::new(Category::Startup.error(format!(
            "cannot start the worker {} for invocation {invocation_id}: {error}{hint}",
            worker.display()
        )))
    })?;
    let worker_pid = child.id() as libc::pid_t;
    // Whatever way this function leaves, a worker it did not reap is killed
    // with its process group, and a root run's uid is swept.
    let mut teardown = identity::Teardown::new(worker_pid, identity);
    let stdout = child.stdout.take().context("worker stdout was not piped")?;
    let stderr = child.stderr.take().context("worker stderr was not piped")?;
    let drain = crate::launch::worker_io::spawn_stderr_drain(
        stderr,
        diagnostics.file(crate::invocation_log::SUPERVISOR_LOG),
    );

    let mut progress = crate::launch::worker_io::progress_file(
        &options.execution,
        &invocation_id,
        recipe,
        operation,
    );
    let mut worker_started = false;
    let started = Instant::now();
    timing("inputs staged, worker starting", run_started);
    let pump = crate::launch::worker_io::pump_lines(
        &invocation_id,
        &mut child,
        stdout,
        &mut worker_diagnostics,
        &mut worker_started,
        timeout,
        std::time::Duration::from_secs(options.execution.timeout_secs),
        &mut progress,
    );
    // The stream error stays the error, with its category; the worker's exit
    // and log tail follow it.
    let with_detail = |error: anyhow::Error, exit: Option<i32>, tail: &str| {
        let hint = if tail.contains("private worker entry requires") {
            format!(
                " ({} predates host runs; point RECIPE_WORKER_BIN at this recipe, or unset it)",
                worker.display()
            )
        } else {
            String::new()
        };
        let detail = format!(
            "; host run of invocation {invocation_id}: worker exit {}; log tail: {}{hint}",
            crate::launch::worker_io::exit_message(exit),
            tail.trim_end()
        );
        let text = format!("{error:#}");
        match Category::of(&error) {
            Some(category) => {
                let message = text
                    .strip_prefix(category.prefix())
                    .and_then(|rest| rest.strip_prefix(": "))
                    .unwrap_or(&text);
                category.error(format!("{message}{detail}")).into()
            }
            None => anyhow::anyhow!("{text}{detail}"),
        }
    };
    let execution_deadline = match pump {
        Ok(deadline) => deadline,
        Err(error) => {
            if let Some(progress) = progress.as_mut() {
                progress.finish(false);
            }
            // The worker may be blocked inside a helper and will not stop by
            // itself: end its process group now.
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            }
            let exit = crate::launch::worker_io::reap_launcher(
                &mut child,
                Instant::now() + crate::launch::worker_io::SHUTDOWN_GRACE,
            )
            .unwrap_or_else(|error| {
                crate::diagnostics::diag!("recipe: warning: {error:#}");
                None
            });
            if exit.is_some() {
                teardown.reaped();
            }
            let drained = crate::launch::worker_io::join_drain(drain);
            teardown.sweep()?;
            if let Some(gate) = gate {
                gate.close()?;
            }
            return Err(with_detail(error, exit, &drained.tail));
        }
    };
    let reaped = crate::launch::worker_io::reap_launcher(&mut child, execution_deadline);
    let drained = crate::launch::worker_io::join_drain(drain);
    drop(worker_diagnostics);
    let exit = match reaped {
        Ok(exit) => exit,
        Err(error) => {
            if let Some(progress) = progress.as_mut() {
                progress.finish(false);
            }
            return Err(error);
        }
    };
    teardown.reaped();
    // The worker has exited (reaped): end anything else still running as the
    // run's uid, then close its directory before reading it.
    teardown.sweep()?;
    if let Some(gate) = gate {
        gate.close()?;
    }
    let result = crate::launch::worker_io::result_from_tree(&root, &invocation_id);
    if let Some(progress) = progress.as_mut() {
        progress.finish(result.as_ref().is_ok_and(|result| result.ok));
    }
    let result = result.map_err(|reason| {
        with_detail(
            crate::launch::worker_io::missing_result(&invocation_id, &reason),
            exit,
            &drained.tail,
        )
    })?;
    if result.ok && exit != Some(0) {
        bail!(Category::Protocol.error(format!(
            "invocation {invocation_id} worker reported success but exited with {} (refusing publication)",
            crate::launch::worker_io::exit_message(exit)
        )));
    }
    timing("worker finished", started);
    let mut summary = None;
    if result.ok {
        // Publication keeps only the named outputs: when they all lie in
        // `out` and no saved state keeps the export, `work` (downloads,
        // build trees) is not archived at all.
        let in_out = retained.is_none()
            && result
                .outputs
                .values()
                .all(|path| path.starts_with(&format!("{}/", crate::domain::OUT)));
        // Where the run's directory and the state root share a filesystem,
        // the outputs are moved out of the run's directory (discarded after
        // the run) into publication instead of unpacked from an archive: a
        // result is moved, never copied twice (docs/recipe-format.md,
        // section 6.5). Saved state still keeps the run's archive as the
        // kept result, made before the move; without saved state nothing is
        // archived (bounds checked by the same walk the archive makes).
        let tree = crate::publication::can_move(&root, &result.outputs, state_dir)
            .then_some(root.as_path());
        if tree.is_some() && retained.is_none() {
            check_storage(&root, &options.execution).map_err(|error| {
                Category::Storage.wrap(error, "cannot archive the run's outputs")
            })?;
        } else {
            let tops: &[&str] = if in_out {
                &["out"]
            } else {
                &crate::domain::WRITABLE_ROOTS
            };
            write_snapshot_of(&root, &export_path, &options.execution, tops).map_err(|error| {
                Category::Storage.wrap(error, "cannot archive the run's outputs")
            })?;
        }
        let usage = match &result.network_usage {
            Some(usage) => {
                crate::needs::report::host_network_usage(&prepared.network.host_endpoints, usage)
            }
            None => serde_json::json!({"status":"unobserved",
                "reason":"the host worker reported no network use","warnings":[]}),
        };
        timing("outputs archived", started);
        summary = Some(crate::launch::finish_outputs(
            &result,
            &export_path,
            tree,
            &mut retained,
            prepared,
            &invocation_id,
            invocation,
            usage,
        )?);
    }
    timing("outputs published", started);
    Ok(LaunchReport {
        result,
        summary,
        launcher_code: exit,
        result_outcome: if exit.is_some() {
            crate::ginux::Outcome::Exited
        } else {
            crate::ginux::Outcome::Signaled
        },
        result_code: exit.map(i64::from),
        log_total_bytes: drained.total_bytes,
        log_discarded_bytes: drained.discarded_bytes,
    })
}

/// With `RECIPE_TIMING` set, say how long the host run has taken at `what`.
pub(crate) fn timing(what: &str, since: Instant) {
    if std::env::var_os("RECIPE_TIMING").is_some() {
        crate::diagnostics::diag!(
            "recipe: timing: {what} after {:.1} s",
            since.elapsed().as_secs_f64()
        );
    }
}

/// The worker environment of a host run: the variables a Ginux worker gets,
/// with the host directories in place of the domain paths, and nothing of
/// the coordinator's own environment.
/// The offline cache directory a host run's worker reads (`crate::offline_cache`).
pub(crate) fn offline_cache_text() -> Result<Option<String>> {
    crate::offline_cache::dir()?
        .map(|dir| {
            dir.into_os_string()
                .into_string()
                .map_err(|_| anyhow::anyhow!("the offline cache path is not UTF-8"))
        })
        .transpose()
}

fn environment(root: &Path, invocation_id: &str) -> Vec<(String, String)> {
    let local = |path: &str| {
        root.join(path.trim_start_matches('/'))
            .to_string_lossy()
            .into_owned()
    };
    let mut path: Vec<String> = crate::domain::tool_bin_dirs()
        .map(|dir| local(&dir))
        .collect();
    path.extend([
        local(crate::domain::TOOLS),
        "/usr/bin".into(),
        "/bin".into(),
    ]);
    vec![
        ("PATH".into(), path.join(":")),
        (
            crate::domain::ENV_WORKER.into(),
            crate::domain::ENV_WORKER_VALUE.into(),
        ),
        (crate::domain::ENV_INVOCATION.into(), invocation_id.into()),
        (
            crate::domain::ENV_HOST_ROOT.into(),
            root.to_string_lossy().into_owned(),
        ),
        (
            crate::domain::ENV_COORDINATOR.into(),
            std::process::id().to_string(),
        ),
        ("TMPDIR".into(), local(crate::domain::TMP)),
    ]
}
