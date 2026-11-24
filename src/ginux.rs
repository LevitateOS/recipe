//! The one module that runs the `ginux` CLI: the public instance lifecycle
//! (`docs/ginux-cutover-design.md`, sections 1.1-1.6).
//!
//! [`Ginux`] names the CLI (`RECIPE_GINUX`), the state root
//! (`RECIPE_GINUX_STATE_ROOT`) and the run root (`RECIPE_GINUX_RUN_ROOT`).
//! Every call is `CLI --state-root S --run-root R VERB ...` with standard
//! input `/dev/null`, standard output and error captured up to [`CAPTURE`]
//! bytes each, and a deadline per verb. The privileged verbs reach root
//! through `sudo` (the CLI's own business), always via Recipe's restricted
//! guard (`crate::runtime::guard_helper`, set as `GINUX_INSTANCE_HELPER` for
//! the CLI), which runs Ginux's helper on Recipe's roots. Only a root caller
//! (the OS builder's wrapper) may name another helper with
//! `GINUX_INSTANCE_HELPER`; a non-root caller's value is ignored. Before the
//! first privileged call of a process a non-root caller's sudo rule for the
//! guard is checked once ([`Ginux::require_privilege`]), so a missing rule
//! stops the run with the way forward instead of a sudo prompt or failure.
//!
//! What the coordinator reads back is untrusted and parsed strictly: the
//! `status --format json` record ([`Status`]) and its outcome record
//! ([`OutcomeRecord`]), the authority on how a run ended.

use crate::failure::Category;
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::ffi::OsStr;
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// Bytes of each captured stream kept per call.
const CAPTURE: usize = 256 * 1024;
/// Bounds on member paths in `inject` and `extract`.
pub(crate) const MAX_PATH: u64 = 4096;

const STATUS_DEADLINE: Duration = Duration::from_secs(60);
const STOP_DEADLINE: Duration = Duration::from_secs(120);
/// `delete` removes a whole root, which can be a large tree.
const DELETE_DEADLINE: Duration = Duration::from_secs(1800);
/// `import` expands the image into a fresh root (a full copy without
/// reflinks), then `chown`s it.
const CREATE_DEADLINE: Duration = Duration::from_secs(3600);
const IMPORT_IMAGE_DEADLINE: Duration = Duration::from_secs(3600);
const DIAGNOSE_DEADLINE: Duration = Duration::from_secs(60);
/// The verbs the CLI runs without the helper.
const UNPRIVILEGED: [&str; 2] = ["validate", "explain"];
/// How long `delete` waits for a stopping instance (teardown after the CLI
/// died is bounded at about 7.5 s by the helper).
const STOPPING_WAIT: Duration = Duration::from_secs(15);

/// The helper's exit codes the coordinator acts on (interim design, 1.6).
pub(crate) mod exit {
    pub const INVALID: i32 = 65;
    pub const NOT_FOUND: i32 = 66;
    pub const RUNNING: i32 = 73;
    pub const CHANGED: i32 = 75;
    pub const BOUND: i32 = 76;
}

/// The `ginux` CLI and the roots Recipe's instances live under.
#[derive(Debug, Clone)]
pub struct Ginux {
    cli: PathBuf,
    /// The helper the CLI runs through sudo (`GINUX_INSTANCE_HELPER` for the
    /// CLI): the guard, or a root caller's own value.
    helper: PathBuf,
    state_root: String,
    run_root: String,
    digest: OnceLock<String>,
}

/// Whether this process runs as root.
fn is_root() -> bool {
    (unsafe { libc::geteuid() }) == 0
}

/// A Ginux tool path from the environment variable `name`, honoured only for
/// a root caller (the OS builder's wrapper): under `sudo` the environment is
/// reset, so a non-root caller's value never names what runs.
fn root_override(name: &str) -> Option<PathBuf> {
    if !is_root() {
        return None;
    }
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

impl Ginux {
    /// The Ginux the environment names (`crate::runtime::ginux_setup`: the
    /// CLI, the state root and the run root; a variable of the fork's
    /// launcher is refused with the one that replaced it).
    pub fn from_env() -> Result<Self> {
        let setup = crate::runtime::ginux_setup()?;
        Ok(Self {
            cli: setup.cli,
            helper: root_override(crate::runtime::HELPER_ENV)
                .unwrap_or_else(crate::runtime::guard_helper),
            state_root: setup.state_root,
            run_root: setup.run_root,
            digest: OnceLock::new(),
        })
    }

    /// The CLI this value runs.
    pub fn cli(&self) -> &Path {
        &self.cli
    }

    /// The state root (instances, image cache).
    pub fn state_root(&self) -> &Path {
        Path::new(&self.state_root)
    }

    /// The exact identity of the Ginux that runs a recipe: the SHA-256 (hex)
    /// of the SHA-256s of every file that decides its behaviour, in this
    /// order: the CLI, the helper the CLI runs (the guard, or a root caller's
    /// `GINUX_INSTANCE_HELPER`), Ginux's helper behind the guard (only when
    /// the guard is that helper), the image tool and the runtime. The image
    /// tool and runtime are `GINUX_IMAGE_TOOL` / `GINUX_RUNTIME` for a root
    /// caller that sets them (a builder's wrapper), else the installed paths;
    /// a non-root caller's variables are never read, since `sudo` resets the
    /// environment and they would name files that never run.
    pub fn digest(&self) -> Result<String> {
        if let Some(digest) = self.digest.get() {
            return Ok(digest.clone());
        }
        use sha2::{Digest, Sha256};
        let tool = |name: &str, default: &str| {
            root_override(name).unwrap_or_else(|| PathBuf::from(default))
        };
        let mut files = vec![self.cli.clone(), self.helper.clone()];
        if self.helper == crate::runtime::guard_helper() {
            files.push(PathBuf::from(crate::runtime::GINUX_HELPER));
        }
        files.push(tool("GINUX_IMAGE_TOOL", crate::runtime::GINUX_IMAGE_TOOL));
        files.push(tool("GINUX_RUNTIME", crate::runtime::GINUX_RUNTIME));
        let mut outer = Sha256::new();
        for file in &files {
            let mut handle = std::fs::File::open(file).with_context(|| {
                format!(
                    "cannot read {} for the exact identity of Ginux",
                    file.display()
                )
            })?;
            let mut inner = Sha256::new();
            std::io::copy(&mut handle, &mut inner)
                .with_context(|| format!("cannot read {}", file.display()))?;
            outer.update(hex(&inner.finalize()).as_bytes());
        }
        let digest = hex(&outer.finalize());
        Ok(self.digest.get_or_init(|| digest).clone())
    }

    /// The image `reference` (`REGISTRY/REPO@sha256:<manifest hex>`) is in
    /// the state root's OCI cache, imported from `layout()` when it is not.
    /// Only root imports: the helper and the image tool apply an image as
    /// root, so the guard allows no `import-image`, and a caller other than
    /// root stops with the way forward (`sudo recipe runtime fetch`). Root
    /// imports through Ginux's helper itself (or its own
    /// `GINUX_INSTANCE_HELPER`), never through the guard. The printed manifest
    /// digest must be the reference's. Imports of one digest are serialized
    /// by a lock in Recipe's own cache directory.
    pub(crate) fn ensure_image(&self, reference: &str, layout: &Path) -> Result<()> {
        self.ensure_image_from(reference, || Ok(layout.to_path_buf()))
    }

    /// [`Ginux::ensure_image`] with the layout found (or downloaded) only
    /// when the image is not imported yet.
    pub(crate) fn ensure_image_from(
        &self,
        reference: &str,
        layout: impl FnOnce() -> Result<PathBuf>,
    ) -> Result<()> {
        let digest = manifest_hex(reference).ok_or_else(|| {
            Category::Startup.error(format!(
                "the image reference {reference:?} names no sha256 manifest digest \
                 (want REGISTRY/REPO@sha256:<64 hex>)"
            ))
        })?;
        let blob = self
            .state_root()
            .join("cache/oci/blobs/sha256")
            .join(digest);
        if blob.is_file() {
            return Ok(());
        }
        if !is_root() {
            bail!(Category::Startup.error(format!(
                "the runtime image {reference} is not installed on this host (not imported into {}): \
                 run `sudo recipe runtime fetch`, which imports it and allows it for Recipe's guard",
                self.state_root
            )));
        }
        let _lock = import_lock(digest);
        if blob.is_file() {
            return Ok(());
        }
        let layout = layout().map_err(|error| {
            Category::Startup.wrap(
                error,
                format!("the image {reference} is not imported and its layout is unavailable"),
            )
        })?;
        crate::diagnostics::diag!(
            "recipe: importing the image {reference} into {} (once per host)",
            self.state_root
        );
        let helper = if self.helper == crate::runtime::guard_helper() {
            PathBuf::from(crate::runtime::GINUX_HELPER)
        } else {
            self.helper.clone()
        };
        let call = self.call_via(
            &helper,
            &[OsStr::new("import-image"), layout.as_os_str()],
            IMPORT_IMAGE_DEADLINE,
            true,
        )?;
        if !call.ok() {
            bail!(Category::Startup.error(format!(
                "ginux import-image {} failed: {}",
                layout.display(),
                call.message()
            )));
        }
        let printed = String::from_utf8_lossy(&call.stdout);
        let printed = printed.trim_end_matches('\n');
        if printed != format!("sha256:{digest}") {
            bail!(Category::Startup.error(format!(
                "manifest digest mismatch: the image layout {} has {printed:?}, but the \
                 reference names sha256:{digest}",
                layout.display()
            )));
        }
        Ok(())
    }

    /// `ginux import NAME CONFIG`: create a stopped instance. The helper
    /// replaces the config of a stopped instance of that name and refuses
    /// (73) only a running one, which a fresh name never is.
    pub(crate) fn create(&self, name: &str, config: &Path) -> Result<()> {
        let call = self.call(
            &[OsStr::new("import"), OsStr::new(name), config.as_os_str()],
            CREATE_DEADLINE,
            false,
        )?;
        match call.code {
            Some(0) => Ok(()),
            Some(exit::RUNNING) => bail!(Category::Startup.error(format!(
                "ginux import {name} failed: an instance of that name is running ({}); \
                 Recipe's instance names are fresh per run, so another process uses it: \
                 run again",
                call.message()
            ))),
            _ => bail!(
                Category::Startup.error(format!("ginux import {name} failed: {}", call.message()))
            ),
        }
    }

    /// `ginux inject NAME --input TAR --into / --max-bytes B --max-members M
    /// --max-path 4096`.
    pub(crate) fn inject(
        &self,
        name: &str,
        input: &Path,
        max_bytes: u64,
        max_members: u64,
        deadline: Duration,
    ) -> Result<Call> {
        let (bytes, members, path) = (
            max_bytes.to_string(),
            max_members.to_string(),
            MAX_PATH.to_string(),
        );
        self.call(
            &[
                OsStr::new("inject"),
                OsStr::new(name),
                OsStr::new("--input"),
                input.as_os_str(),
                OsStr::new("--into"),
                OsStr::new("/"),
                OsStr::new("--max-bytes"),
                OsStr::new(&bytes),
                OsStr::new("--max-members"),
                OsStr::new(&members),
                OsStr::new("--max-path"),
                OsStr::new(&path),
            ],
            deadline,
            true,
        )
    }

    /// The `ginux start NAME --wait --stream-console --full-audit` command:
    /// the helper copies the instance console to its standard output, which
    /// the CLI passes through unchanged and which carries nothing else; after
    /// every run, whatever its outcome, its standard error ends with the
    /// audit log (its last 16 MiB) after [`crate::launch::worker_io`]'s
    /// marker line. The caller owns the process group, the parent-death
    /// signal and the streams, and has called [`Ginux::require_privilege`].
    pub(crate) fn start_command(&self, name: &str) -> Command {
        let mut command = self.command();
        command
            .arg("start")
            .arg(name)
            .arg("--wait")
            .arg("--stream-console")
            .arg("--full-audit");
        command
    }

    /// Whether this process may run the privileged verbs: root, or a sudo
    /// rule that lets this user run the guard without a password on
    /// Recipe's roots (`sudo -n -l -- GUARD --protocol 1 --state-root S
    /// --run-root R status recipe-<uid>-probe`: the argument string a real
    /// call matches the rule with; `-l` only lists it and runs nothing). Asked once per process; a
    /// missing rule is a `startup` failure naming the way forward.
    pub(crate) fn require_privilege(&self) -> Result<()> {
        static CHECKED: OnceLock<std::result::Result<(), String>> = OnceLock::new();
        if is_root() {
            return Ok(());
        }
        let checked = CHECKED.get_or_init(|| {
            let helper = self.helper.display();
            let way_forward = "Run Recipe as root (sudo recipe ...), or install the rule for the \
                 administrators' group with `recipe runtime stage --sudoers GROUP` (GROUP is \
                 wheel, or sudo on Debian and Ubuntu) or with Recipe's installer, and add \
                 this user to that group; recipes that start no program run on the host \
                 without it";
            match Command::new("sudo")
                .args(["-n", "-l", "--"])
                .arg(&self.helper)
                .args(["--protocol", "1", "--state-root"])
                .arg(&self.state_root)
                .arg("--run-root")
                .arg(&self.run_root)
                .arg("status")
                .arg(format!("recipe-{}-probe", unsafe { libc::getuid() }))
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .output()
            {
                Ok(output) if output.status.success() => Ok(()),
                Ok(output) => Err(format!(
                    "Ginux runs need root or the sudoers rule {} that lets this user run \
                     Recipe's guard {helper} through sudo without a password, and this user \
                     has no such rule. {way_forward}. (sudo said: {})",
                    crate::runtime::SUDOERS,
                    tail_text(&output.stderr)
                )),
                Err(error) => Err(format!(
                    "Ginux runs need root or sudo, and sudo cannot be run ({error}). \
                     Install sudo with the sudoers rule {}, or run Recipe as root. \
                     {way_forward}",
                    crate::runtime::SUDOERS
                )),
            }
        });
        checked
            .clone()
            .map_err(|message| Category::Startup.error(message).into())
    }

    /// `ginux status NAME --format json`, parsed strictly.
    pub(crate) fn status(&self, name: &str) -> Result<Status> {
        let call = self.call(
            &[
                OsStr::new("status"),
                OsStr::new(name),
                OsStr::new("--format"),
                OsStr::new("json"),
            ],
            STATUS_DEADLINE,
            false,
        )?;
        if !call.ok() {
            bail!(
                Category::Protocol.error(format!("ginux status {name} failed: {}", call.message()))
            );
        }
        Status::parse(&call.stdout, name)
    }

    /// `ginux extract NAME --output FILE --max-bytes A --max-members O
    /// --max-path 4096 PATH...`.
    pub(crate) fn extract(
        &self,
        name: &str,
        output: &Path,
        max_bytes: u64,
        max_members: u64,
        paths: &[&str],
        deadline: Duration,
    ) -> Result<Call> {
        let (bytes, members, path) = (
            max_bytes.to_string(),
            max_members.to_string(),
            MAX_PATH.to_string(),
        );
        let mut args = vec![
            OsStr::new("extract"),
            OsStr::new(name),
            OsStr::new("--output"),
            output.as_os_str(),
            OsStr::new("--max-bytes"),
            OsStr::new(&bytes),
            OsStr::new("--max-members"),
            OsStr::new(&members),
            OsStr::new("--max-path"),
            OsStr::new(&path),
        ];
        args.extend(paths.iter().map(OsStr::new));
        self.call(&args, deadline, true)
    }

    /// `ginux stop NAME`.
    pub(crate) fn stop(&self, name: &str) -> Result<Call> {
        self.call(
            &[OsStr::new("stop"), OsStr::new(name)],
            STOP_DEADLINE,
            false,
        )
    }

    /// `ginux delete NAME`, until the instance is gone. A stopping instance
    /// (73) is waited for up to [`STOPPING_WAIT`], then stopped, then deleted
    /// once more; one that is already gone (66) is fine. Never cancelled:
    /// cleanup must run after an interruption too.
    pub(crate) fn delete(&self, name: &str) -> Result<()> {
        for attempt in 0..2 {
            let call = self.call(
                &[OsStr::new("delete"), OsStr::new(name)],
                DELETE_DEADLINE,
                false,
            )?;
            match call.code {
                Some(0) | Some(exit::NOT_FOUND) => return Ok(()),
                Some(exit::RUNNING) if attempt == 0 => {
                    let end = Instant::now() + STOPPING_WAIT;
                    let mut stopped = false;
                    while Instant::now() < end {
                        if self.status(name).is_ok_and(|status| !status.running) {
                            stopped = true;
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(250));
                    }
                    if !stopped {
                        let stop = self.stop(name)?;
                        if !stop.ok() && stop.code != Some(exit::NOT_FOUND) {
                            bail!("ginux stop {name} failed before delete: {}", stop.message());
                        }
                    }
                }
                _ => bail!("ginux delete {name} failed: {}", call.message()),
            }
        }
        bail!("ginux delete {name}: the instance is still running")
    }

    /// Delete the instances earlier Recipe processes left behind
    /// (`recipe-<uid>-<pid>-<hex>` whose process is gone): this user's, or
    /// every user's for root. A running one (its helper died) is stopped
    /// first. A live owner is never touched. Problems are warnings.
    pub(crate) fn sweep(&self) {
        let directory = self.state_root().join("instances");
        let Ok(entries) = std::fs::read_dir(&directory) else {
            return;
        };
        let own = unsafe { libc::getuid() };
        for entry in entries.flatten() {
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let Some((uid, pid)) = parse_instance_name(&name) else {
                continue;
            };
            if (own != 0 && uid != own) || process_alive(pid) {
                continue;
            }
            let swept = (|| -> Result<()> {
                if self.status(&name)?.running {
                    self.stop(&name)?;
                }
                self.delete(&name)
            })();
            match swept {
                Ok(()) => crate::diagnostics::diag!(
                    "recipe: deleted the Ginux instance {name} an earlier run left behind"
                ),
                Err(error) => crate::diagnostics::diag!(
                    "recipe: warning: cannot delete the leftover Ginux instance {name}: {error:#}"
                ),
            }
        }
    }

    /// `ginux validate FILE` (unprivileged).
    pub(crate) fn validate(&self, config: &Path) -> Result<Call> {
        self.call(
            &[OsStr::new("validate"), config.as_os_str()],
            DIAGNOSE_DEADLINE,
            false,
        )
    }

    /// `ginux explain FILE OPERATION` (unprivileged).
    pub(crate) fn explain(&self, config: &Path, operation: &str) -> Result<Call> {
        self.call(
            &[
                OsStr::new("explain"),
                config.as_os_str(),
                OsStr::new(operation),
            ],
            DIAGNOSE_DEADLINE,
            false,
        )
    }

    fn command(&self) -> Command {
        self.command_via(&self.helper)
    }

    /// The CLI with `helper` as the helper it runs through sudo.
    fn command_via(&self, helper: &Path) -> Command {
        let mut command = Command::new(&self.cli);
        command
            .env(crate::runtime::HELPER_ENV, helper)
            .arg("--state-root")
            .arg(&self.state_root)
            .arg("--run-root")
            .arg(&self.run_root);
        command
    }

    /// Run one verb to its end: streams captured (bounded), stopped at
    /// `deadline`, and, when `cancellable`, at the coordinator's signal.
    fn call(&self, args: &[&OsStr], deadline: Duration, cancellable: bool) -> Result<Call> {
        self.call_via(&self.helper, args, deadline, cancellable)
    }

    /// [`Ginux::call`] with `helper` as the helper the CLI runs.
    fn call_via(
        &self,
        helper: &Path,
        args: &[&OsStr],
        deadline: Duration,
        cancellable: bool,
    ) -> Result<Call> {
        let verb = args
            .first()
            .map(|verb| verb.to_string_lossy().into_owned())
            .unwrap_or_default();
        if !UNPRIVILEGED.contains(&verb.as_str()) {
            self.require_privilege()?;
        }
        let mut command = self.command_via(helper);
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        let mut child = command.spawn().map_err(|error| {
            Category::Startup.error(format!(
                "cannot run the ginux CLI {} ({verb}): {error}",
                self.cli.display()
            ))
        })?;
        let stdout = capture(child.stdout.take());
        let stderr = capture(child.stderr.take());
        let end = Instant::now() + deadline;
        let mut stopped = None;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) => {}
                Err(error) => {
                    stopped = Some(format!("cannot watch it: {error}"));
                    break stop_group(&mut child, Duration::from_secs(10));
                }
            }
            if cancellable && crate::cancellation::signal() != 0 {
                stopped = Some(String::new());
                break stop_group(&mut child, Duration::from_secs(10));
            }
            if Instant::now() >= end {
                stopped = Some(format!("it did not finish within {}s", deadline.as_secs()));
                break stop_group(&mut child, Duration::from_secs(10));
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        let stdout = stdout.join().unwrap_or_default();
        let stderr = stderr.join().unwrap_or_default();
        match stopped {
            Some(reason) if reason.is_empty() => Err(crate::cancellation::cancelled().into()),
            Some(reason) => bail!(Category::Supervisor.error(format!(
                "ginux {verb} was stopped: {reason}; {}",
                tail_text(&stderr)
            ))),
            None => Ok(Call {
                code: status.and_then(|status| status.code()),
                stdout,
                stderr,
            }),
        }
    }
}

/// How one CLI call ended.
pub(crate) struct Call {
    /// The exit code, None when it died by a signal.
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl Call {
    pub(crate) fn ok(&self) -> bool {
        self.code == Some(0)
    }

    /// The exit and the end of its standard error, for messages.
    pub(crate) fn message(&self) -> String {
        format!(
            "{}: {}",
            match self.code {
                Some(code) => format!("exit {code}"),
                None => "killed by a signal".to_string(),
            },
            tail_text(&self.stderr)
        )
    }
}

/// How the supervised workload ended, per the outcome record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Outcome {
    Exited,
    Signaled,
    SupervisorError,
}

impl Outcome {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Exited => "exited",
            Self::Signaled => "signaled",
            Self::SupervisorError => "supervisor-error",
        }
    }
}

impl std::fmt::Display for Outcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The supervisor's outcome record (interim design, 1.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct OutcomeRecord {
    pub outcome: Outcome,
    /// Workload exit code when `exited`.
    pub code: Option<i64>,
    pub signal: Option<u64>,
}

impl OutcomeRecord {
    /// The record of a run that left none: `supervisor-error`.
    pub(crate) fn supervisor_error() -> Self {
        Self {
            outcome: Outcome::SupervisorError,
            code: None,
            signal: None,
        }
    }

    /// A short form for messages: `exited/3`, `signaled/signal 9`.
    pub(crate) fn describe(&self) -> String {
        format!(
            "{}/{}",
            self.outcome,
            self.code
                .map(|code| code.to_string())
                .or_else(|| self.signal.map(|signal| format!("signal {signal}")))
                .unwrap_or_else(|| "no status".to_string())
        )
    }
}

/// One instance's `status --format json` record. Its log paths are not
/// read: instance directories are root-private, so the console arrives on
/// `start --wait --stream-console --full-audit`'s standard output and the
/// audit log on its standard error.
#[derive(Debug, Clone)]
pub(crate) struct Status {
    pub running: bool,
    /// None before the first start and while running.
    pub outcome: Option<OutcomeRecord>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StatusWire {
    version: u32,
    name: String,
    state: String,
    // Read only so an unknown field still fails the parse.
    #[serde(rename = "contract")]
    _contract: serde_json::Value,
    #[serde(rename = "pid")]
    _pid: Option<u64>,
    outcome: Option<OutcomeWire>,
    #[serde(rename = "paths")]
    _paths: serde_json::Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OutcomeWire {
    version: u32,
    outcome: Outcome,
    code: Option<i64>,
    signal: Option<u64>,
    #[serde(rename = "generation")]
    _generation: Option<i64>,
}

impl Status {
    fn parse(bytes: &[u8], name: &str) -> Result<Self> {
        let protocol = |message: String| anyhow::Error::new(Category::Protocol.error(message));
        let text = std::str::from_utf8(bytes)
            .map_err(|_| protocol(format!("ginux status {name}: the record is not UTF-8")))?;
        let wire: StatusWire = serde_json::from_str(text.trim_end()).map_err(|error| {
            protocol(format!(
                "ginux status {name}: malformed record ({error}): {}",
                text.chars().take(300).collect::<String>()
            ))
        })?;
        if wire.version != 1 || wire.name != name {
            return Err(protocol(format!(
                "ginux status {name}: unexpected record version {} for {:?}",
                wire.version, wire.name
            )));
        }
        let running = match wire.state.as_str() {
            "running" => true,
            "stopped" => false,
            other => {
                return Err(protocol(format!(
                    "ginux status {name}: unknown state {other:?}"
                )));
            }
        };
        let outcome = match wire.outcome {
            None => None,
            Some(record) if record.version == 1 => Some(OutcomeRecord {
                outcome: record.outcome,
                code: record.code,
                signal: record.signal,
            }),
            Some(record) => {
                return Err(protocol(format!(
                    "ginux status {name}: outcome record version {} (want 1)",
                    record.version
                )));
            }
        };
        Ok(Self { running, outcome })
    }
}

/// End a child running in its own process group: SIGTERM to the group,
/// `grace` to exit, then SIGKILL. The status, when one could be read.
pub(crate) fn stop_group(child: &mut Child, grace: Duration) -> Option<ExitStatus> {
    if let Ok(Some(status)) = child.try_wait() {
        return Some(status);
    }
    let group = -(child.id() as libc::pid_t);
    unsafe {
        libc::kill(group, libc::SIGTERM);
    }
    let end = Instant::now() + grace;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) if Instant::now() < end => std::thread::sleep(Duration::from_millis(25)),
            _ => {
                unsafe {
                    libc::kill(group, libc::SIGKILL);
                }
                let _ = child.kill();
                return child.wait().ok();
            }
        }
    }
}

/// A fresh instance name: `recipe-<uid>-<pid>-<8 hex>` (at most 39 bytes;
/// Ginux allows 64). The sweep reads the uid and pid back.
pub(crate) fn instance_name() -> String {
    let mut random = [0u8; 4];
    let filled = unsafe { libc::getrandom(random.as_mut_ptr().cast(), random.len(), 0) };
    if filled != random.len() as isize {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.subsec_nanos())
            .unwrap_or_default();
        random = nanos.to_le_bytes();
    }
    format!(
        "recipe-{}-{}-{}",
        unsafe { libc::getuid() },
        std::process::id(),
        hex(&random)
    )
}

fn parse_instance_name(name: &str) -> Option<(libc::uid_t, libc::pid_t)> {
    let mut parts = name.strip_prefix("recipe-")?.split('-');
    let uid = parts.next()?.parse().ok()?;
    let pid = parts.next()?.parse().ok()?;
    let random = parts.next()?;
    (parts.next().is_none()
        && random.len() == 8
        && random.bytes().all(|byte| byte.is_ascii_hexdigit()))
    .then_some((uid, pid))
}

/// Whether `pid` names a live process (one this user may not signal counts).
fn process_alive(pid: libc::pid_t) -> bool {
    if pid <= 0 {
        return true;
    }
    let alive = unsafe { libc::kill(pid, 0) } == 0;
    alive || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

/// The 64 lowercase hex digits after `@sha256:` in an image reference.
fn manifest_hex(reference: &str) -> Option<&str> {
    let (_, digest) = reference.rsplit_once("@sha256:")?;
    (digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
    .then_some(digest)
}

/// Serialize imports of one digest among Recipe processes of this user.
/// Without a cache directory there is no lock: a racing import only renames
/// identical content.
fn import_lock(digest: &str) -> Option<std::fs::File> {
    let directory = dirs::cache_dir()?.join("recipe").join("ginux-import");
    std::fs::create_dir_all(&directory).ok()?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(directory.join(format!("{digest}.lock")))
        .ok()?;
    use std::os::fd::AsRawFd;
    loop {
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } == 0 {
            return Some(file);
        }
        if std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
            return None;
        }
    }
}

fn capture<R: Read + Send + 'static>(pipe: Option<R>) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut kept = Vec::new();
        let Some(mut pipe) = pipe else {
            return kept;
        };
        let mut buffer = [0u8; 8192];
        loop {
            match pipe.read(&mut buffer) {
                Ok(0) => return kept,
                Ok(count) => {
                    let room = CAPTURE.saturating_sub(kept.len());
                    kept.extend_from_slice(&buffer[..count.min(room)]);
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => return kept,
            }
        }
    })
}

/// The last lines of a captured stream, for messages.
fn tail_text(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let text = text.trim();
    if text.is_empty() {
        return "no message".to_string();
    }
    let start = text
        .char_indices()
        .rev()
        .nth(2000)
        .map(|(index, _)| index)
        .unwrap_or(0);
    text[start..].to_string()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
