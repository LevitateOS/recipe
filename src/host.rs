//! A host run: one recipe operation without Ginux (`crate::execution`).
//!
//! The coordinator does what a Ginux launch does, with host directories in
//! place of the domain root:
//!
//! - `ROOT/{inputs,work/{build,tools,tmp},out}` in the scratch pool. `ROOT`
//!   is the same path on every run for a destination (`invocation-host-…`),
//!   so absolute paths a recipe keeps in `ctx.state` stay valid on resume.
//!   Inputs are staged and digest-checked exactly as for a domain root.
//! - The worker is the same `recipe` binary, started directly as a child: its
//!   own process group, killed with the coordinator (PDEATHSIG), its address
//!   space bounded by `memory_mib` (RLIMIT_AS), its working directory `work`,
//!   and the worker environment with host paths. It speaks the same frame
//!   protocol; the request's `host` field tells it where its directories are
//!   and what it may reach. It installs the path rule
//!   (`crate::helpers::confine`), the network rule
//!   (`crate::helpers::acquire::net`) and a seccomp filter that refuses
//!   `execve`, and it has no program helpers.
//! - A checkpoint arrives while the worker waits for its ack, so nothing
//!   writes: the coordinator archives `work` and `out` in the snapshot format
//!   a Ginux snapshot uses (members `work/…`, `out/…`), and `crate::retained`
//!   keeps it with the state, as for Ginux. Restore unpacks that archive (or
//!   the seeded target) before the worker starts.
//! - Storage bounds are checked when an archive is made: at every checkpoint
//!   and at the end, over everything in `work` and `out` (coarse: a run may
//!   exceed them between checkpoints).
//! - The final archive is the export `crate::publication` validates and
//!   publishes, exactly as a Ginux export.
//! - The deadline and cancellation kill the worker's process group.
//! - Identity: the worker never keeps privilege; a root run's worker has a
//!   uid and gid of its own and only it reaches the run's directories
//!   (`identity`). Snapshots, restores and exports walk by descriptor
//!   (`snapshot`).
//! - Resource limits on the worker: `memory_mib` (RLIMIT_AS), no core files,
//!   1024 descriptors, and files no larger than `NEEDS.storage_max_bytes`
//!   when that is set (RLIMIT_FSIZE; SIGXFSZ is ignored, so the write fails
//!   instead of killing the worker).

mod identity;
mod snapshot;

pub(crate) use identity::clear_capabilities;
use identity::{Gate, Limits, Stopped};
use snapshot::{check_storage, unpack_snapshot, write_snapshot};

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
        cleanup_reason,
        recipe,
        persist_state,
        ref options,
        destination,
        state_dir,
        ..
    } = *invocation;
    let invocation_id = prepared.invocation_id.clone();
    prepared
        .network
        .check_host_reachable()
        .map_err(|error| Category::Startup.wrap(error, "refused network endpoint"))?;
    let timeout = crate::launch::frame_timeout()
        .map_err(|error| Category::Startup.wrap(error, "invalid frame timeout"))?;
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
    let inputs = root.join(crate::domain::INPUTS.trim_start_matches('/'));
    crate::inputs::stage_inputs(prepared, &inputs).map_err(startup)?;
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
                prepared,
                &worker_digest,
                None,
                options,
                Some(&root),
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
    let mut restore = retained
        .as_ref()
        .map(|state| state.restore_input(&restore_path))
        .transpose()
        .map_err(|error| Category::Storage.wrap(error, "cannot restore saved files"))?
        .unwrap_or(false);
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
        .stdin(Stdio::piped())
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
    let stdin = child.stdin.take().context("worker stdin was not piped")?;
    let stdout = child.stdout.take().context("worker stdout was not piped")?;
    let stderr = child.stderr.take().context("worker stderr was not piped")?;
    let drain = crate::launch::spawn_stderr_drain(
        stderr,
        diagnostics.file(crate::invocation_log::SUPERVISOR_LOG),
    );

    let request = crate::worker::protocol::RequestPayload {
        invocation_id: invocation_id.clone(),
        operation,
        persist_state,
        cleanup_reason: cleanup_reason.map(str::to_owned),
        options: options.clone(),
        restored_states: retained
            .as_ref()
            .map(|state| state.states().clone())
            .unwrap_or_default(),
        host: Some(crate::worker::protocol::HostRun {
            root: root
                .to_str()
                .context("the host run directory is not UTF-8")?
                .to_owned(),
            network: prepared.network.host_grants(),
            network_max_bytes: options.execution.network_max_bytes,
        }),
    };
    let mut progress = options.execution.progress_file.as_deref().map(|path| {
        crate::progress_file::ProgressFile::start(
            path,
            &invocation_id,
            recipe
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or_default(),
            &operation.to_string(),
        )
    });
    let mut worker_started = false;
    let pump = {
        // A checkpoint: the worker waits for the ack, so nothing writes while
        // work and out are archived.
        let execution = &options.execution;
        let root = &root;
        let mut capture = |point: &crate::worker::protocol::CheckpointPayload,
                           _deadline: Instant|
         -> Result<Option<u64>> {
            // A root run reads the worker's files only while it is stopped.
            let _stopped = if unprivileged {
                Some(Stopped::stop(worker_pid)?)
            } else {
                None
            };
            let Some(state) = retained.as_mut() else {
                // Without persistence the bounds are still checked.
                check_storage(root, execution)?;
                return Ok(None);
            };
            let pending = state.begin()?;
            write_snapshot(root, &pending.path, execution)?;
            state
                .commit(pending, &point.recipe, point.state.clone())
                .map(Some)
                .context("cannot persist checkpoint state/files pair")
        };
        crate::launch::pump_protocol(
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
    let mut stream = match pump {
        Ok(stream) => stream,
        Err(error) => {
            // The worker may be blocked inside a helper and will not stop by
            // itself: end its process group now.
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            }
            let exit = crate::launch::reap_launcher(
                &mut child,
                Instant::now() + crate::launch::SHUTDOWN_GRACE,
            )
            .unwrap_or_else(|error| {
                crate::diagnostics::diag!("recipe: warning: {error:#}");
                None
            });
            if exit.is_some() {
                teardown.reaped();
            }
            let drained = crate::launch::join_drain(drain);
            teardown.sweep()?;
            if let Some(gate) = gate {
                gate.close()?;
            }
            // The stream error stays the error, with its category; the
            // worker's exit and log tail follow it.
            let hint = if drained.tail.contains("private worker entry requires") {
                format!(
                    " ({} predates host runs; point RECIPE_WORKER_BIN at this recipe, or unset it)",
                    worker.display()
                )
            } else {
                String::new()
            };
            let detail = format!(
                "; host run of invocation {invocation_id}: worker exit {}; log tail: {}{hint}",
                crate::launch::exit_message(exit),
                drained.tail.trim_end()
            );
            let text = format!("{error:#}");
            return Err(match Category::of(&error) {
                Some(category) => {
                    let message = text
                        .strip_prefix(category.prefix())
                        .and_then(|rest| rest.strip_prefix(": "))
                        .unwrap_or(&text);
                    category.error(format!("{message}{detail}")).into()
                }
                None => anyhow::anyhow!("{text}{detail}"),
            });
        }
    };
    drop(stream.stdin);
    let reaped = crate::launch::reap_launcher(&mut child, stream.execution_deadline);
    let drained = crate::launch::join_drain(drain);
    drop(worker_diagnostics);
    let exit = reaped?;
    teardown.reaped();
    // The worker has exited (reaped): end anything else still running as the
    // run's uid, then close its directory before reading it.
    teardown.sweep()?;
    if let Some(gate) = gate {
        gate.close()?;
    }
    if stream.result.ok && exit != Some(0) {
        bail!(Category::Protocol.error(format!(
            "invocation {invocation_id} worker reported success but exited with {} (refusing publication)",
            crate::launch::exit_message(exit)
        )));
    }
    if stream.result.ok {
        write_snapshot(&root, &export_path, &options.execution)
            .map_err(|error| Category::Storage.wrap(error, "cannot archive the run's outputs"))?;
        let usage = match &stream.result.network_usage {
            Some(usage) => {
                crate::needs::report::host_network_usage(&prepared.network.host_endpoints, usage)
            }
            None => serde_json::json!({"status":"unobserved",
                "reason":"the host worker reported no network use","warnings":[]}),
        };
        crate::launch::finish_outputs(
            &mut stream.result,
            &export_path,
            &mut retained,
            prepared,
            &invocation_id,
            operation,
            options,
            state_dir,
            usage,
        )?;
    }
    Ok(LaunchReport {
        result: stream.result,
        checkpoints_acked: stream.checkpoints,
        generations: stream.generations,
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

/// The worker environment of a host run: the variables a Ginux worker gets,
/// with the host directories in place of the domain paths, and nothing of
/// the coordinator's own environment.
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
            crate::domain::ENV_COORDINATOR.into(),
            std::process::id().to_string(),
        ),
        ("TMPDIR".into(), local(crate::domain::TMP)),
    ]
}
