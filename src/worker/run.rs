//! Worker entry point: run the engine behind the coordinator records
//! ([`crate::worker::records`]).
//!
//! The worker is the same `recipe` binary entered with a private argv marker (see
//! the dispatch in `src/bin/`). It reads the request file
//! ([`crate::domain::REQUEST`]), runs the requested operation, then writes
//! exactly one result file ([`crate::domain::RESULT`]) and exits 0.
//!
//! Transport discipline:
//!
//! - Standard input is unused. Standard output carries line records only
//!   ([`LineRecord`]: diagnostics and progress). Child bytes and engine
//!   narration go through the installed diagnostic sink, which writes them
//!   as diagnostic lines; startup/audit details stay on stderr.
//! - The result is written to [`crate::domain::RESULT_STAGING`] and renamed
//!   to [`crate::domain::RESULT`], after a stale result (one a restored
//!   archive brought back) is removed at start. The staging file is created
//!   and allocated ([`crate::domain::RESULT_RESERVE_BYTES`]) before any
//!   recipe code runs, so a run that fills its storage quota still records
//!   its failure. When even the request is
//!   unusable, the worker still writes a failed result, so the coordinator
//!   learns why.
//! - A failed result says what failed ([`FailureKind`]): a phase (with its
//!   name), the recipe operation outside a phase, or the request itself.
//!
//! The coordinator prepares immutable inputs and restores the kept files the
//! run starts from (kept downloads, or a seeded target). The worker builds the
//! run's constants anew; an interrupted step starts again from its beginning.
//! Its result names every output at the path the engine placed it at. It
//! never rewrites recipe sources.

use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};

use crate::core::executor::{EngineError, Load};
use crate::worker::records::{
    FailureInfo, FailureKind, LineRecord, Operation, ProgressRecord, RECORDS_VERSION,
    WorkerRequest, WorkerResult,
};

/// What a finished operation reports in its result.
#[derive(Default)]
struct Outcome {
    /// Build and install: each declared output to its engine-placed domain path.
    outputs: std::collections::BTreeMap<String, String>,
    /// The steps the worker worked on, with their status.
    steps: Vec<crate::core::events::StepReport>,
    /// Latest: the version `latest()` returned.
    latest: Option<String>,
}

/// The worker's standard output, shared by the diagnostic and progress
/// sinks: one whole line per write, never interleaved.
struct WorkerChannel {
    budget: crate::worker::records::DiagnosticBudget,
    failed: bool,
    terminal: bool,
}
type SharedChannel = Arc<Mutex<WorkerChannel>>;

/// Write one line record to standard output and flush it.
fn write_line(record: &LineRecord) -> std::io::Result<()> {
    let line = record.encode().map_err(std::io::Error::other)?;
    let mut output = std::io::stdout().lock();
    output.write_all(&line)?;
    output.flush()
}

fn install_diagnostics(channel: &SharedChannel) -> Result<()> {
    let channel = Arc::clone(channel);
    crate::diagnostics::install(Box::new(move |stream, mut bytes| {
        let mut state = channel
            .lock()
            .map_err(|_| std::io::Error::other("worker channel poisoned"))?;
        if state.failed || state.terminal {
            return Err(std::io::Error::other("worker channel is closed"));
        }
        while !bytes.is_empty() {
            let Some(record) = state.budget.next(stream.into(), &mut bytes) else {
                break;
            };
            if let Err(error) = write_line(&LineRecord::diagnostic(record)) {
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
        let record = LineRecord::progress(ProgressRecord {
            recipe: report.recipe.clone(),
            phase: report.phase.clone(),
            what: report.what.clone(),
            done: report.done,
            total: report.total,
        });
        // Progress is best effort: a failed write is left for the next
        // diagnostic or the result to report.
        if write_line(&record).is_err() {
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
/// Reads the request file, runs the engine, writes exactly one result file
/// (a failed one on error) and returns `Ok` (binary exits 0). When the result
/// cannot be written, or standard output broke, it returns `Err` (binary exits
/// nonzero; the coordinator sees a worker exit without a result).
pub fn run_worker() -> Result<()> {
    let (invocation_id, host_entry) = ensure_internal_entry()?;
    // A result an earlier run left (a restored archive holds its `work`) is
    // never this run's. Best effort: the coordinator refuses a result that
    // names another invocation anyway, and a staging name left behind makes
    // the final write fail loudly.
    for stale in [crate::domain::RESULT, crate::domain::RESULT_STAGING] {
        let _ = std::fs::remove_file(crate::domain::local(stale));
    }
    // The result's space is taken now, while the run's storage is still
    // free: a recipe that later fills the quota can still report that.
    let result_file = reserve_result()?;

    // (1) The request file. When the request itself is unusable, still write
    // a failed result so the coordinator learns why.
    let request = match read_request() {
        Ok(req) => req,
        Err(err) => {
            let failure = request_failure(format!("bad request: {err:#}"));
            return write_result(result_file, &invocation_id, Err(failure));
        }
    };
    if request.invocation_id != invocation_id || request.host.is_some() != host_entry {
        let failure =
            request_failure("request invocation id or mode does not match worker bootstrap".into());
        return write_result(result_file, &invocation_id, Err(failure));
    }
    crate::diagnostics::trace(&format!(
        "worker: invocation={} operation={} recipe={} ({})",
        request.invocation_id,
        request.operation,
        request.recipe_name,
        crate::domain::MAIN_RECIPE
    ));

    // A host-mode worker sets up its confinement before any recipe code:
    // its directories, its network grant, and a kernel filter that refuses
    // to start any program at all.
    if let Some(host) = &request.host
        && let Err(error) = enter_host_mode(host)
    {
        let failure = request_failure(format!("cannot enter host mode: {error:#}"));
        return write_result(result_file, &invocation_id, Err(failure));
    }
    if let Some(cache) = &request.cache
        && let Err(error) = crate::offline_cache::set_worker(PathBuf::from(cache))
    {
        let failure = request_failure(format!("cannot select the offline cache: {error:#}"));
        return write_result(result_file, &invocation_id, Err(failure));
    }

    let channel = Arc::new(Mutex::new(WorkerChannel {
        budget: Default::default(),
        failed: false,
        terminal: false,
    }));
    install_diagnostics(&channel)?;
    install_progress(&channel);
    crate::core::output::set_machine_events(request.options.execution.machine_events);
    crate::helpers::acquire::net::set_limit(request.options.execution.network_max_bytes);
    crate::diagnostics::line("recipe: managed worker started");
    let outcome = run_operation(&request);
    crate::core::output::flush_progress();
    let mut channel = channel
        .lock()
        .map_err(|_| anyhow!("worker channel poisoned"))?;
    channel.terminal = true;
    if channel.failed {
        bail!("worker diagnostic output failed");
    }
    write_result(result_file, &invocation_id, outcome)
}

/// Require the private argv entry, the coordinator's bootstrap flag, and an
/// invocation id. Public backend selection never uses these environment values.
/// Also says whether the coordinator started a host-mode worker, and selects
/// its host root ([`crate::domain::ENV_HOST_ROOT`]) so the request file can
/// be read below it.
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
    match (host, std::env::var(crate::domain::ENV_HOST_ROOT)) {
        (true, Ok(root)) if crate::domain::valid_host_path(&root) => {
            crate::domain::set_host_root(PathBuf::from(root))?;
        }
        (false, Err(std::env::VarError::NotPresent)) => {}
        _ => bail!(
            "private worker entry requires {} exactly for a host-mode worker, as an absolute path",
            crate::domain::ENV_HOST_ROOT
        ),
    }
    Ok((id, host))
}

/// Take the host run's directories and network grant, then forbid `execve`.
fn enter_host_mode(host: &crate::worker::records::HostRun) -> Result<()> {
    shed_privileges()?;
    let root = crate::domain::host_root()
        .context("a host-mode worker has no host root")?
        .to_path_buf();
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

/// Read and validate the request file.
fn read_request() -> Result<WorkerRequest> {
    let path = crate::domain::local(crate::domain::REQUEST);
    let file = crate::fsx::open_regular(&path)
        .with_context(|| format!("cannot open {}", path.display()))?;
    let bytes = crate::worker::records::read_bounded(file, crate::domain::MAX_REQUEST_BYTES)
        .with_context(|| format!("cannot read {}", path.display()))?;
    Ok(WorkerRequest::decode(&bytes)?)
}

/// Run the requested operation. Returns what the result reports;
/// engine and request problems become a [`FailureInfo`].
fn run_operation(req: &WorkerRequest) -> std::result::Result<Outcome, FailureInfo> {
    let recipe = crate::domain::local(crate::domain::MAIN_RECIPE);
    let build_dir = crate::domain::local(crate::domain::BUILD);
    let search_path = crate::domain::local(crate::domain::INPUTS);
    if !recipe.is_file() || !build_dir.is_dir() {
        return Err(request_failure(format!(
            "the domain lacks {} or {}",
            recipe.display(),
            build_dir.display()
        )));
    }
    let programs = req.host.is_none();
    let make_engine = || build_engine(&search_path, programs);
    let load = Load {
        name: &req.recipe_name,
        search_path: Some(&search_path),
        make_engine: &make_engine,
        defines: &req.options.defines,
        dependencies: &req.dependencies,
        options: &req.options.execution,
        lenient: req.operation == Operation::Latest,
    };
    use crate::core::executor as engine_ops;
    let mut outcome = Outcome::default();
    match req.operation {
        Operation::Download => {
            outcome.steps = engine_ops::download(&recipe, &load).map_err(engine_failure)?;
        }
        Operation::Build | Operation::Install => {
            let built = engine_ops::install(&recipe, &load, req.operation == Operation::Install)
                .map_err(engine_failure)?;
            outcome.outputs = built.outputs;
            outcome.steps = built.steps;
        }
        Operation::Cleanup => engine_ops::cleanup(&recipe, &load).map_err(engine_failure)?,
        Operation::Latest => {
            outcome.latest = Some(engine_ops::latest(&recipe, &load).map_err(engine_failure)?);
        }
    }
    Ok(outcome)
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

/// The result's staging file, created and allocated before any recipe code
/// runs ([`reserve_result`]); [`write_result`] fills it.
struct ResultFile(std::fs::File);

/// Create [`crate::domain::RESULT_STAGING`] and allocate
/// [`crate::domain::RESULT_RESERVE_BYTES`] for it: `fallocate`, or written
/// zeros on a filesystem without it. `create_new` with no-follow: a name a
/// program planted fails the run instead of redirecting the result.
fn reserve_result() -> Result<ResultFile> {
    let staging = crate::domain::local(crate::domain::RESULT_STAGING);
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .custom_flags(libc::O_NOFOLLOW)
        .mode(0o644)
        .open(&staging)
        .with_context(|| format!("cannot create {}", staging.display()))?;
    let size = crate::domain::RESULT_RESERVE_BYTES;
    let allocated = unsafe { libc::fallocate(file.as_raw_fd(), 0, 0, size as libc::off_t) };
    let error = (allocated != 0).then(std::io::Error::last_os_error);
    let unsupported = error.as_ref().is_some_and(|error| {
        matches!(
            error.raw_os_error(),
            Some(libc::EOPNOTSUPP | libc::ENOSYS | libc::EINVAL)
        )
    });
    let error = match error {
        Some(_) if unsupported => file.write_all_at(&vec![0u8; size as usize], 0).err(),
        error => error,
    };
    if let Some(error) = error {
        let full = matches!(error.raw_os_error(), Some(libc::ENOSPC | libc::EDQUOT));
        return Err(error).with_context(|| {
            format!(
                "cannot reserve {size} B for the result file {}{}",
                staging.display(),
                if full {
                    " before the recipe started: the run's storage is already full \
                     (raise its storage limit)"
                } else {
                    ""
                }
            )
        });
    }
    Ok(ResultFile(file))
}

/// Write the result into its reserved staging file, then rename it into
/// place, so the coordinator finds a whole result or none. The record is
/// written over the reserved bytes and the file then cut to its length, so
/// no new space is needed; a record larger than the reserve is replaced by
/// a failed result that says so.
fn write_result(
    result_file: ResultFile,
    invocation_id: &str,
    outcome: std::result::Result<Outcome, FailureInfo>,
) -> Result<()> {
    // The outputs are read from disk: write deferred manifests first.
    let outcome = match crate::target::flush_pending() {
        Ok(()) => outcome,
        Err(error) => Err(outcome.err().unwrap_or_else(|| FailureInfo {
            kind: FailureKind::Recipe,
            phase: None,
            message: format!("cannot write the target metadata: {error:#}"),
        })),
    };
    let (ok, failure, outcome) = match outcome {
        Ok(outcome) => (true, None, outcome),
        Err(failure) => (false, Some(failure), Outcome::default()),
    };
    let mut result = WorkerResult {
        version: RECORDS_VERSION,
        invocation_id: invocation_id.to_owned(),
        ok,
        failure,
        outputs: outcome.outputs,
        steps: outcome.steps,
        latest: outcome.latest,
        network_usage: crate::helpers::acquire::net::usage(),
    };
    let mut bytes = result.encode().map_err(|e| anyhow!("encode result: {e}"))?;
    let reserve = crate::domain::RESULT_RESERVE_BYTES;
    if bytes.len() as u64 > reserve {
        result.ok = false;
        result.failure = Some(FailureInfo {
            kind: FailureKind::Recipe,
            phase: None,
            message: format!(
                "the result record is {} B, more than the {reserve} B reserved for it \
                 (too many outputs or steps, or names too long)",
                bytes.len()
            ),
        });
        result.outputs.clear();
        result.steps.clear();
        result.latest = None;
        bytes = result.encode().map_err(|e| anyhow!("encode result: {e}"))?;
        anyhow::ensure!(
            bytes.len() as u64 <= reserve,
            "even the failed result exceeds the {reserve} B reserved for it"
        );
    }
    let staging = crate::domain::local(crate::domain::RESULT_STAGING);
    let path = crate::domain::local(crate::domain::RESULT);
    let file = result_file.0;
    file.write_all_at(&bytes, 0)
        .and_then(|()| file.set_len(bytes.len() as u64))
        .and_then(|()| file.sync_all())
        .with_context(|| format!("cannot write {}", staging.display()))?;
    drop(file);
    std::fs::rename(&staging, &path)
        .with_context(|| format!("cannot rename the result to {}", path.display()))?;
    Ok(())
}
