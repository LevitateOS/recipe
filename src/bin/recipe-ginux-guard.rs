//! `recipe-ginux-guard`: the one program Recipe's sudoers rule lets its group
//! run as root without a password, instead of Ginux's root helper itself.
//!
//! It is called exactly like the helper (the `ginux` CLI reaches it through
//! `GINUX_INSTANCE_HELPER`):
//!
//! ```text
//! recipe-ginux-guard --protocol 1 --state-root /var/lib/recipe/ginux --run-root /run/recipe/ginux VERB ARGS...
//! ```
//!
//! and runs the real helper at its fixed path [`HELPER`] only for the verbs and
//! argument shapes Recipe uses (`src/ginux.rs`), on instances named for the
//! calling user (`recipe-<uid>-...`; root may name any uid's, for the sweep).
//! `import` reads the caller's config with the caller's identity, accepts only
//! what `InstanceConfig::render` (`src/inputs/config.rs`) emits, and hands the
//! helper a root-owned, re-rendered copy. For a caller other than root the
//! config's image must be exactly one of the references in the root-owned
//! allowlist [`IMAGE_ALLOWLIST`] (written by `recipe runtime stage`, `recipe
//! runtime fetch` as root and Recipe's installer): the helper and the image
//! tool apply an image as root, so only images root installed may be named.
//! `import-image` is not allowed at all, for anyone: root imports with `recipe
//! runtime fetch`, which reaches the helper without the guard. Everything else
//! exits 64, as the helper does for a usage error.
//!
//! `recipe-ginux-guard --check-config [--image-allowlist LIST] FILE` runs only
//! the config check (no root, no helper): exit 0, or 65 with the reason on
//! standard error. With `--image-allowlist` the image is also checked against
//! LIST under the same trust rules as the fixed allowlist; the option exists
//! only for this dry run, never for a real request.
//! `recipe-ginux-guard --version` prints its name and version (the builder's
//! compile check). A request is checked before the root check, so a refused
//! one exits 64 for anyone; an allowed one run without root exits 77.
//!
//! Diagnostics are one line on standard error; untrusted text is escaped.

use std::ffi::{CStr, CString};
use std::fs::File;
use std::io::{Read, Write};
use std::ops::RangeInclusive;
use std::os::fd::{FromRawFd, OwnedFd};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus};
use std::time::Duration;

const ME: &str = "recipe-ginux-guard";
/// The real helper; never taken from the environment.
const HELPER: &str = "/usr/libexec/ginux/ginux-helper";
const STATE_ROOT: &str = "/var/lib/recipe/ginux";
const RUN_ROOT: &str = "/run/recipe/ginux";
/// Root-only directory for the sanitized `import` configs.
const STAGING: &str = "/run/recipe/guard";
/// A staging directory older than this was left by a killed guard.
const STALE_STAGING: Duration = Duration::from_secs(24 * 3600);
const HELPER_PATH: &str = "/usr/sbin:/usr/bin:/sbin:/bin";
const MAX_CONFIG_READ: usize = 64 * 1024;
/// The runtime images a caller other than root may build an instance from:
/// a regular file owned by root, not writable by group or others, in a chain
/// of root-owned directories nobody else can write; one
/// `REGISTRY/REPO@sha256:<64 hex>` per line, `#` comments and blank lines
/// ignored, at most [`MAX_ALLOWLIST_READ`] bytes. Missing or untrusted, it
/// allows no image. Also `levitate_recipe::runtime::IMAGE_ALLOWLIST`.
const IMAGE_ALLOWLIST: &str = "/usr/libexec/recipe/runtime-images";
const MAX_ALLOWLIST_READ: usize = 64 * 1024;
/// Ginux's instance identifier bound (`IDENTIFIER_BYTES`).
const MAX_NAME: usize = 64;
const MAX_MEMBER_PATH: usize = 4096;
const MAX_EXTRACT_PATHS: usize = 16;

const USAGE: i32 = 64;
const INVALID: i32 = 65;
const UNREADABLE: i32 = 66;
const UNAVAILABLE: i32 = 69;
const OSERR: i32 = 71;
const NOPERM: i32 = 77;

/// The instance config Recipe renders, the only one `import` accepts. The
/// source of truth is `InstanceConfig::render` in `src/inputs/config.rs`, with
/// the values it takes from `src/domain.rs` (`WORKER`, `WORKER_ARGS`, `UID`,
/// `WORK`, `OUT`, `MAX_NETWORK_ENDPOINTS`), `src/inputs/env.rs`
/// (`allowlisted_env`), `src/options.rs` (`jobs`, `memory_mib` bounds) and
/// `src/needs/contract.rs` (`Contract::derived`, `render_lists`, `Outbound`).
/// Change this together with them.
mod shape {
    use std::ops::RangeInclusive;

    pub const TOP: [&str; 11] = [
        "schema_version",
        "instance",
        "resources",
        "execution",
        "access",
        "contract",
        "storage",
        "devices",
        "network",
        "audit",
        "failure",
    ];
    pub const INIT: &str = "/inputs/recipe-worker";
    pub const ARGS: [&str; 2] = ["--__recipe-worker", "/inputs/main.rhai"];
    pub const ENV_NAMES: [&str; 4] = ["PATH", "RECIPE_WORKER", "RECIPE_INVOCATION", "TMPDIR"];
    pub const MAX_ENV_ENTRY: usize = 256;
    pub const MAX_IMAGE: usize = 128;
    pub const VCPUS: RangeInclusive<i64> = 1..=16;
    pub const MEMORY_MIB: RangeInclusive<i64> = 16..=1_048_576;
    pub const PROFILE: &str = "balanced";
    pub const BACKEND: &str = "ptrace-native";
    pub const ACCESS_ID: i64 = 1000;
    pub const CATALOG: i64 = 2;
    pub const ALLOW: [&str; 9] = [
        "syscall.*",
        "filesystem.path.read",
        "filesystem.path.write",
        "device.console.read",
        "device.console.write",
        "time.clock.read",
        "signal.virtual.deliver",
        "credential.virtual.set",
        "memory.virtual.manage",
    ];
    /// Added to [`ALLOW`] with an outbound grant.
    pub const ALLOW_NETWORK: &str = "network.inet.tcp.connect";
    pub const DENY: [&str; 6] = [
        "device.mmio.direct",
        "device.dma.direct",
        "network.packet.raw",
        "syscall.accept*",
        "syscall.bind",
        "syscall.listen",
    ];
    /// Added to [`DENY`] without an outbound grant.
    pub const DENY_OFFLINE: [&str; 6] = [
        "syscall.connect",
        "syscall.*msg",
        "syscall.shutdown",
        "syscall.getsockname",
        "syscall.getpeername",
        "syscall.*sockopt",
    ];
    pub const FS_READ: [&str; 1] = ["/"];
    pub const FS_WRITE: [&str; 2] = ["/work", "/out"];
    pub const DEVICES: [&str; 3] = ["null", "random", "urandom"];
    pub const MAX_PORTS: usize = 8;
}

struct Failure {
    code: i32,
    line: String,
}

type Checked<T> = Result<T, Failure>;

fn fail(code: i32, line: impl Into<String>) -> Failure {
    Failure {
        code,
        line: line.into(),
    }
}

fn usage(reason: &str) -> Failure {
    fail(
        USAGE,
        format!(
            "usage: {ME} --protocol 1 --state-root {STATE_ROOT} --run-root {RUN_ROOT} COMMAND [ARGUMENT]... ({reason})"
        ),
    )
}

fn invalid(reason: impl std::fmt::Display) -> Failure {
    fail(INVALID, format!("{ME}: refused instance config: {reason}"))
}

fn os_error(what: &str, error: impl std::fmt::Display) -> Failure {
    fail(OSERR, format!("{ME}: {what}: {error}"))
}

/// Untrusted text for a diagnostic: printable ASCII, everything else escaped.
fn shown(text: &str) -> String {
    text.as_bytes().escape_ascii().to_string()
}

fn main() {
    harden_process();
    let code = match run() {
        Ok(code) => code,
        Err(failure) => {
            eprintln!("{}", failure.line);
            failure.code
        }
    };
    std::process::exit(code);
}

fn run() -> Checked<i32> {
    let args = std::env::args_os()
        .skip(1)
        .map(|arg| {
            arg.into_string()
                .map_err(|arg| usage(&format!("{} is not UTF-8", shown(&arg.to_string_lossy()))))
        })
        .collect::<Checked<Vec<String>>>()?;
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    if args.first() == Some(&"--check-config") {
        let (images, file) = match args.as_slice() {
            [_, file] => (Images::Any, *file),
            [_, "--image-allowlist", list, file] => (Images::Listed(list), *file),
            _ => {
                return Err(fail(
                    USAGE,
                    format!("usage: {ME} --check-config [--image-allowlist LIST] FILE"),
                ));
            }
        };
        let path = CString::new(file).map_err(|_| invalid("the path has a NUL byte"))?;
        let bytes = read_bounded(&path).map_err(|error| error.failure(file, INVALID))?;
        sanitize(&bytes, None, images)?;
        return Ok(0);
    }
    if args.as_slice() == ["--version"] {
        println!("{ME} {}", env!("CARGO_PKG_VERSION"));
        return Ok(0);
    }
    // The request is checked before the root check, so a refused request
    // exits 64 whoever runs the guard; only an allowed one needs root.
    let caller = caller()?;
    let request = parse(&args, caller.uid)?;
    require_root()?;
    match request {
        Request::Exec(verb) => {
            std::env::set_current_dir("/").map_err(|error| os_error("cannot enter /", error))?;
            let error = helper(&verb).exec();
            Err(fail(
                UNAVAILABLE,
                format!("{ME}: cannot run {HELPER}: {error}"),
            ))
        }
        Request::Import { name, config } => import(caller, name, config),
    }
}

/// The process state a caller could have set, reset before anything else:
/// standard descriptors open, no other descriptor inherited, default signal
/// handling and no blocked signal, umask 022, no file-size or CPU limit that
/// could cut the helper off half-way, no core files.
fn harden_process() {
    // SAFETY: plain syscalls on this process's own descriptor table, signal
    // dispositions, mask, umask and limits, before any other code runs and
    // with no other thread; every pointer is to a local value.
    unsafe {
        for fd in 0..3 {
            if libc::fcntl(fd, libc::F_GETFD) == -1 {
                libc::open(c"/dev/null".as_ptr(), libc::O_RDWR);
            }
        }
        if libc::syscall(libc::SYS_close_range, 3u32, u32::MAX, 0u32) != 0 {
            let max = libc::sysconf(libc::_SC_OPEN_MAX).clamp(1024, 1 << 20);
            for fd in 3..max as libc::c_int {
                libc::close(fd);
            }
        }
        let mut empty: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut empty);
        libc::sigprocmask(libc::SIG_SETMASK, &empty, std::ptr::null_mut());
        for signal in 1..=libc::SIGRTMAX() {
            if signal != libc::SIGKILL && signal != libc::SIGSTOP {
                libc::signal(signal, libc::SIG_DFL);
            }
        }
        libc::umask(0o022);
        for (resource, value) in [
            (libc::RLIMIT_FSIZE, libc::RLIM_INFINITY),
            (libc::RLIMIT_CPU, libc::RLIM_INFINITY),
            (libc::RLIMIT_CORE, 0),
        ] {
            let limit = libc::rlimit {
                rlim_cur: value,
                rlim_max: value,
            };
            libc::setrlimit(resource, &limit);
        }
    }
}

#[derive(Clone, Copy)]
struct Caller {
    uid: u32,
    gid: u32,
}

/// The guard runs only as root (real and effective uid 0).
fn require_root() -> Checked<()> {
    // SAFETY: getuid and geteuid cannot fail.
    let (real, effective) = unsafe { (libc::getuid(), libc::geteuid()) };
    if real != 0 || effective != 0 {
        return Err(fail(
            NOPERM,
            format!("{ME}: runs only as root, through sudo (uid {real}, effective {effective})"),
        ));
    }
    Ok(())
}

/// The calling user, from `SUDO_UID`/`SUDO_GID` only; root itself (no
/// `SUDO_UID`) is uid 0. Trusted only once [`require_root`] holds: sudo
/// sets both and a root caller's environment is root's.
fn caller() -> Checked<Caller> {
    let id = |name: &str| -> Checked<Option<u32>> {
        let Some(value) = std::env::var_os(name) else {
            return Ok(None);
        };
        value
            .to_str()
            .and_then(number)
            .and_then(|value| u32::try_from(value).ok())
            .filter(|value| *value != u32::MAX)
            .map(Some)
            .ok_or_else(|| {
                fail(
                    NOPERM,
                    format!(
                        "{ME}: {name} is not an id number: {}",
                        shown(&value.to_string_lossy())
                    ),
                )
            })
    };
    match (id("SUDO_UID")?, id("SUDO_GID")?) {
        (Some(uid), Some(gid)) => Ok(Caller { uid, gid }),
        (None, None) => Ok(Caller { uid: 0, gid: 0 }),
        _ => Err(fail(
            NOPERM,
            format!("{ME}: SUDO_UID and SUDO_GID must both be set by sudo"),
        )),
    }
}

/// A decimal number without leading zeros ("0" itself is one).
fn number(text: &str) -> Option<u64> {
    let canonical = !text.is_empty()
        && text.len() <= 20
        && text.bytes().all(|byte| byte.is_ascii_digit())
        && (text == "0" || !text.starts_with('0'));
    canonical.then(|| text.parse().ok()).flatten()
}

enum Request<'a> {
    /// Exec the helper with these arguments after the roots.
    Exec(Vec<&'a str>),
    Import {
        name: &'a str,
        config: &'a str,
    },
}

/// Check the arguments: the protocol, Recipe's two roots, then one of the
/// verbs Recipe uses (`src/ginux.rs`) in the exact shape the `ginux` CLI
/// passes it to the helper. `list` is not among them: the sweep reads the
/// state root's `instances` directory itself. Nor is `import-image`: an image
/// is applied as root, so only root imports one (`recipe runtime fetch`).
fn parse<'a>(args: &[&'a str], caller: u32) -> Checked<Request<'a>> {
    if args.first() != Some(&"--protocol") {
        return Err(usage("the first argument is --protocol"));
    }
    match args.get(1) {
        Some(&"1") => {}
        other => {
            return Err(fail(
                USAGE,
                format!(
                    "unsupported helper protocol: {}",
                    shown(other.unwrap_or(&""))
                ),
            ));
        }
    }
    let root = |index: usize, option: &str, kind: &str, fixed: &str| -> Checked<()> {
        if args.get(index) != Some(&option) {
            return Err(usage(&format!("{option} {fixed} is required")));
        }
        match args.get(index + 1) {
            Some(value) if *value == fixed => Ok(()),
            Some(value) => Err(fail(
                USAGE,
                format!(
                    "untrusted {kind} root: {} (only Recipe's {fixed} is allowed)",
                    shown(value)
                ),
            )),
            None => Err(usage(&format!("{option} {fixed} is required"))),
        }
    };
    root(2, "--state-root", "state", STATE_ROOT)?;
    root(4, "--run-root", "run", RUN_ROOT)?;
    let Some((&verb, rest)) = args.get(6..).and_then(<[&str]>::split_first) else {
        return Err(usage("COMMAND is missing"));
    };
    let shape = |reason: &str| usage(&format!("{verb}: {reason}"));
    let exec = |name: &str| -> Checked<Request<'a>> {
        check_name(name, caller)?;
        Ok(Request::Exec([&[verb][..], rest].concat()))
    };
    match (verb, rest) {
        ("import", [name, config]) => {
            check_name(name, caller)?;
            if config.is_empty() || config.len() > MAX_MEMBER_PATH {
                return Err(shape("CONFIG is an empty or overlong path"));
            }
            Ok(Request::Import { name, config })
        }
        (
            "inject",
            [
                name,
                "--max-bytes",
                bytes,
                "--max-members",
                members,
                "--max-path",
                path_bound,
                path,
            ],
        ) => {
            check_bounds(verb, [bytes, members, path_bound])?;
            check_member_path(verb, path)?;
            exec(name)
        }
        (
            "start",
            [name, "--wait", pid, start]
            | [name, "--audit", "--wait", pid, start]
            | [name, "--wait", pid, start, "--stream-console"]
            | [name, "--audit", "--wait", pid, start, "--stream-console"],
        ) => {
            if number(pid).is_none() || number(start).is_none() {
                return Err(shape("--wait takes a PID and a START number"));
            }
            exec(name)
        }
        ("status", [name] | [name, "--format", "json"]) => exec(name),
        ("stop" | "delete", [name]) => exec(name),
        (
            "extract",
            [
                name,
                "--max-bytes",
                bytes,
                "--max-members",
                members,
                "--max-path",
                path_bound,
                paths @ ..,
            ],
        ) => {
            check_bounds(verb, [bytes, members, path_bound])?;
            if paths.is_empty() || paths.len() > MAX_EXTRACT_PATHS {
                return Err(shape(&format!("1 to {MAX_EXTRACT_PATHS} PATHs")));
            }
            for path in paths {
                check_member_path(verb, path)?;
            }
            exec(name)
        }
        ("import" | "inject" | "start" | "status" | "stop" | "delete" | "extract", _) => {
            Err(shape("arguments are not in the shape Recipe passes"))
        }
        _ => Err(usage(&format!(
            "COMMAND {} is not one Recipe uses",
            shown(verb)
        ))),
    }
}

/// `recipe-<caller uid>-[A-Za-z0-9_.-]+`, at most 64 bytes; root may name any
/// uid's instance (its sweep deletes every user's leftovers).
fn check_name(name: &str, caller: u32) -> Checked<()> {
    let owner = name
        .strip_prefix("recipe-")
        .and_then(|rest| rest.split_once('-'))
        .filter(|(_, tail)| !tail.is_empty())
        .map(|(uid, _)| uid);
    let allowed = name.len() <= MAX_NAME
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_.-".contains(&byte))
        && owner.is_some_and(|uid| {
            if caller == 0 {
                number(uid).is_some_and(|uid| u32::try_from(uid).is_ok())
            } else {
                uid == caller.to_string()
            }
        });
    if allowed {
        return Ok(());
    }
    let owner = if caller == 0 {
        "UID".to_string()
    } else {
        caller.to_string()
    };
    Err(fail(
        USAGE,
        format!(
            "invalid instance name: {} (this caller may use recipe-{owner}-NAME, at most {MAX_NAME} bytes of [A-Za-z0-9_.-])",
            shown(name)
        ),
    ))
}

/// The `--max-bytes`, `--max-members` and `--max-path` values: positive,
/// without leading zeros, as the helper requires.
fn check_bounds(verb: &str, values: [&str; 3]) -> Checked<()> {
    for value in values {
        if number(value).is_none_or(|value| value == 0) {
            return Err(usage(&format!(
                "{verb}: bound {} is not a positive number",
                shown(value)
            )));
        }
    }
    Ok(())
}

/// An absolute member path without empty, `.` or `..` components or control
/// bytes, at most 4096 bytes (`/` itself is allowed).
fn check_member_path(verb: &str, path: &str) -> Checked<()> {
    let components_ok = path == "/"
        || path.strip_prefix('/').is_some_and(|rest| {
            rest.split('/')
                .all(|part| !part.is_empty() && part != "." && part != "..")
        });
    if components_ok
        && path.len() <= MAX_MEMBER_PATH
        && !path.bytes().any(|byte| byte < 32 || byte == 127)
    {
        return Ok(());
    }
    Err(usage(&format!(
        "{verb}: PATH {} is not a plain absolute path",
        shown(path)
    )))
}

/// The real helper with a fresh environment and Recipe's roots.
fn helper(verb: &[&str]) -> Command {
    let mut command = Command::new(HELPER);
    command
        .env_clear()
        .env("PATH", HELPER_PATH)
        .env("LC_ALL", "C")
        .args([
            "--protocol",
            "1",
            "--state-root",
            STATE_ROOT,
            "--run-root",
            RUN_ROOT,
        ])
        .args(verb);
    command
}

/// `import NAME CONFIG`: read CONFIG as the caller, check it (for a caller
/// other than root, its image against [`IMAGE_ALLOWLIST`]), write the
/// re-rendered config into a fresh root-only directory, run the helper on
/// that copy and remove it afterwards, then end as the helper ended.
fn import(caller: Caller, name: &str, config: &str) -> Checked<i32> {
    let bytes = read_as_caller(config, caller)?;
    // Root gains nothing from the rule, so its images are not limited.
    let images = if caller.uid == 0 {
        Images::Any
    } else {
        Images::Listed(IMAGE_ALLOWLIST)
    };
    let text = sanitize(&bytes, Some(name), images)?;
    std::env::set_current_dir("/").map_err(|error| os_error("cannot enter /", error))?;
    // Blocked from here on, so a signal cannot end the guard between writing
    // the copy and removing it; one that arrives is passed to the helper.
    let signals = Signals::block()?;
    let staged = Staged::write(&text)?;
    let spawned = helper(&["import", name, &staged.file.to_string_lossy()]).spawn();
    let status = spawned.and_then(|child| signals.wait_forwarding(child));
    staged.remove();
    match status {
        Ok(status) => Ok(signals.end_as(status)),
        Err(error) => Err(fail(
            UNAVAILABLE,
            format!("{ME}: cannot run {HELPER}: {error}"),
        )),
    }
}

/// Why a config could not be read; also the reader child's exit code.
#[derive(Clone, Copy)]
enum ReadError {
    Unreadable = 2,
    NotRegular = 3,
    TooLarge = 4,
    ReadFailed = 5,
}

impl ReadError {
    fn from_code(code: i32) -> Option<Self> {
        [
            Self::Unreadable,
            Self::NotRegular,
            Self::TooLarge,
            Self::ReadFailed,
        ]
        .into_iter()
        .find(|error| *error as i32 == code)
    }

    /// The diagnostic; `unreadable` is the exit code for a file that cannot
    /// be opened or read (66 like the helper, 65 for `--check-config`).
    fn failure(self, path: &str, unreadable: i32) -> Failure {
        let path = shown(path);
        match self {
            Self::Unreadable => fail(
                unreadable,
                format!(
                    "configuration is not readable: {path} (missing, not permitted to the caller, or a symbolic link)"
                ),
            ),
            Self::ReadFailed => fail(unreadable, format!("configuration cannot be read: {path}")),
            Self::NotRegular => invalid(format!("{path} is not a regular file")),
            Self::TooLarge => invalid(format!("{path} is larger than {MAX_CONFIG_READ} bytes")),
        }
    }
}

/// Open without following a final symbolic link or blocking on a FIFO, check
/// for a regular file, read at most [`MAX_CONFIG_READ`] bytes.
fn read_bounded(path: &CStr) -> Result<Vec<u8>, ReadError> {
    let flags =
        libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_NOCTTY | libc::O_CLOEXEC;
    // SAFETY: `path` is a valid NUL-terminated string.
    let fd = unsafe { libc::open(path.as_ptr(), flags) };
    if fd < 0 {
        return Err(ReadError::Unreadable);
    }
    // SAFETY: `fd` was just opened and is owned by nothing else.
    let file = unsafe { File::from_raw_fd(fd) };
    if !file.metadata().is_ok_and(|meta| meta.file_type().is_file()) {
        return Err(ReadError::NotRegular);
    }
    let mut bytes = Vec::new();
    file.take(MAX_CONFIG_READ as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ReadError::ReadFailed)?;
    if bytes.len() > MAX_CONFIG_READ {
        return Err(ReadError::TooLarge);
    }
    Ok(bytes)
}

/// Read the caller-chosen `path` in a child that has become the caller (uid,
/// gid, no supplementary groups), so root never opens a path the caller could
/// not; the bytes come back through a pipe.
fn read_as_caller(path: &str, caller: Caller) -> Checked<Vec<u8>> {
    let c_path = CString::new(path).map_err(|_| usage("import: CONFIG has a NUL byte"))?;
    let mut fds = [0 as libc::c_int; 2];
    // SAFETY: pipe2 writes two descriptors into the two-element array.
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(os_error(
            "cannot create a pipe",
            std::io::Error::last_os_error(),
        ));
    }
    // SAFETY: both descriptors are fresh and owned by nothing else.
    let (reader, writer) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
    // SAFETY: the guard has a single thread, so the child may run ordinary
    // code (it allocates) until it leaves through `_exit`.
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        return Err(os_error("cannot fork", std::io::Error::last_os_error()));
    }
    if pid == 0 {
        drop(reader);
        let code = read_in_child(&c_path, writer, caller);
        // SAFETY: ends the child without running the parent's exit path.
        unsafe { libc::_exit(code) }
    }
    drop(writer);
    let mut bytes = Vec::new();
    let read = File::from(reader)
        .take(MAX_CONFIG_READ as u64 + 1)
        .read_to_end(&mut bytes);
    let mut status = 0;
    loop {
        // SAFETY: waits for the child forked above; `status` is a local.
        if unsafe { libc::waitpid(pid, &mut status, 0) } == pid {
            break;
        }
        if std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
            return Err(os_error(
                "cannot wait for the reader",
                std::io::Error::last_os_error(),
            ));
        }
    }
    let code = libc::WIFEXITED(status).then(|| libc::WEXITSTATUS(status));
    match code {
        Some(0) if read.is_ok() && bytes.len() <= MAX_CONFIG_READ => Ok(bytes),
        Some(0) => Err(ReadError::ReadFailed.failure(path, UNREADABLE)),
        Some(6) => Err(fail(
            OSERR,
            format!(
                "{ME}: cannot take the caller's identity (uid {}) to read the config",
                caller.uid
            ),
        )),
        Some(code) => match ReadError::from_code(code) {
            Some(error) => Err(error.failure(path, UNREADABLE)),
            None => Err(fail(OSERR, format!("{ME}: the config reader failed"))),
        },
        None => Err(fail(OSERR, format!("{ME}: the config reader was killed"))),
    }
}

fn read_in_child(path: &CStr, writer: OwnedFd, caller: Caller) -> libc::c_int {
    // SAFETY: credential syscalls on this child process alone; the group list
    // pointer is null with a zero count.
    let dropped = unsafe {
        libc::setgroups(0, std::ptr::null()) == 0
            && libc::setresgid(caller.gid, caller.gid, caller.gid) == 0
            && libc::setresuid(caller.uid, caller.uid, caller.uid) == 0
            && libc::getuid() == caller.uid
            && libc::geteuid() == caller.uid
            && libc::getegid() == caller.gid
            && (caller.uid == 0 || libc::setresuid(0, 0, 0) != 0)
    };
    if !dropped {
        return 6;
    }
    match read_bounded(path) {
        Ok(bytes) => match File::from(writer).write_all(&bytes) {
            Ok(()) => 0,
            Err(_) => ReadError::ReadFailed as libc::c_int,
        },
        Err(error) => error as libc::c_int,
    }
}

/// The sanitized copy of a config: `STAGING/<random>/config.toml`, root-owned,
/// in a 0700 directory.
struct Staged {
    directory: PathBuf,
    file: PathBuf,
}

impl Staged {
    fn write(text: &str) -> Checked<Self> {
        trusted_directory(Path::new("/run"), None)?;
        trusted_directory(Path::new("/run/recipe"), Some(0o755))?;
        trusted_directory(Path::new(STAGING), Some(0o700))?;
        sweep_stale();
        let mut random = [0u8; 16];
        // SAFETY: getrandom fills the local buffer of the given length.
        let filled = unsafe { libc::getrandom(random.as_mut_ptr().cast(), random.len(), 0) };
        if filled != random.len() as isize {
            return Err(os_error("no random bytes", std::io::Error::last_os_error()));
        }
        let directory = Path::new(STAGING).join(
            random
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>(),
        );
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .map_err(|error| os_error(&format!("cannot create {}", directory.display()), error))?;
        let staged = Staged {
            file: directory.join("config.toml"),
            directory,
        };
        let written = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&staged.file)
            .and_then(|mut file| file.write_all(text.as_bytes()));
        match written {
            Ok(()) => Ok(staged),
            Err(error) => {
                let failure = os_error(&format!("cannot write {}", staged.file.display()), error);
                staged.remove();
                Err(failure)
            }
        }
    }

    fn remove(self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

/// `path` is a real directory owned by root and not writable by group or
/// others (created with `create` mode first when given); its parent was
/// checked before, so nobody but root can replace it afterwards.
fn trusted_directory(path: &Path, create: Option<u32>) -> Checked<()> {
    if let Some(mode) = create {
        match std::fs::DirBuilder::new().mode(mode).create(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(os_error(
                    &format!("cannot create {}", path.display()),
                    error,
                ));
            }
        }
    }
    let trusted = std::fs::symlink_metadata(path)
        .is_ok_and(|meta| meta.file_type().is_dir() && meta.uid() == 0 && meta.mode() & 0o022 == 0);
    if trusted {
        Ok(())
    } else {
        Err(fail(
            OSERR,
            format!(
                "{ME}: {} is not a root-owned directory closed to group and others",
                path.display()
            ),
        ))
    }
}

/// Remove staging directories a killed guard left behind.
fn sweep_stale() {
    let Ok(entries) = std::fs::read_dir(STAGING) else {
        return;
    };
    for entry in entries.flatten() {
        let old = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age > STALE_STAGING);
        if old {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// The terminating signals, blocked and passed on to the helper, plus
/// `SIGCHLD` to learn that it ended.
struct Signals {
    set: libc::sigset_t,
}

const FORWARDED: [libc::c_int; 6] = [
    libc::SIGHUP,
    libc::SIGINT,
    libc::SIGQUIT,
    libc::SIGTERM,
    libc::SIGUSR1,
    libc::SIGUSR2,
];

impl Signals {
    fn block() -> Checked<Self> {
        // SAFETY: builds a local signal set and blocks it in this
        // single-threaded process. `std` empties the mask again in the
        // helper child before exec.
        unsafe {
            let mut set: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut set);
            for signal in FORWARDED.into_iter().chain([libc::SIGCHLD]) {
                libc::sigaddset(&mut set, signal);
            }
            if libc::sigprocmask(libc::SIG_BLOCK, &set, std::ptr::null_mut()) != 0 {
                return Err(os_error(
                    "cannot block signals",
                    std::io::Error::last_os_error(),
                ));
            }
            Ok(Self { set })
        }
    }

    fn wait_forwarding(&self, mut child: Child) -> std::io::Result<ExitStatus> {
        loop {
            if let Some(status) = child.try_wait()? {
                return Ok(status);
            }
            let mut signal = 0;
            // SAFETY: `self.set` is initialized and blocked; `signal` is a local.
            if unsafe { libc::sigwait(&self.set, &mut signal) } != 0 || signal == libc::SIGCHLD {
                continue;
            }
            // SAFETY: signals the helper child, which is not reaped yet.
            unsafe {
                libc::kill(child.id() as libc::pid_t, signal);
            }
        }
    }

    /// The exit code for `status`; a helper killed by a signal kills the
    /// guard by the same signal.
    fn end_as(&self, status: ExitStatus) -> i32 {
        if let Some(code) = status.code() {
            return code;
        }
        let signal = status.signal().unwrap_or(libc::SIGKILL);
        // SAFETY: restores the default action for `signal`, unblocks
        // everything and raises it on this process.
        unsafe {
            libc::signal(signal, libc::SIG_DFL);
            libc::sigprocmask(libc::SIG_UNBLOCK, &self.set, std::ptr::null_mut());
            libc::raise(signal);
        }
        128 + signal
    }
}

/// One TOML table of the config, named by its dotted path in messages.
struct Table<'a> {
    path: &'static str,
    map: &'a toml::Table,
}

impl<'a> Table<'a> {
    fn key(&self, key: &str) -> String {
        if self.path.is_empty() {
            shown(key)
        } else {
            format!("{}.{}", self.path, shown(key))
        }
    }

    fn only(&self, keys: &[&str]) -> Checked<()> {
        match self.map.keys().find(|key| !keys.contains(&key.as_str())) {
            Some(key) => Err(invalid(format!("unknown key {}", self.key(key)))),
            None => Ok(()),
        }
    }

    fn value(&self, key: &str) -> Checked<&'a toml::Value> {
        self.map
            .get(key)
            .ok_or_else(|| invalid(format!("{} is missing", self.key(key))))
    }

    fn table(&self, key: &str, path: &'static str) -> Checked<Table<'a>> {
        match self.value(key)? {
            toml::Value::Table(map) => Ok(Table { path, map }),
            _ => Err(invalid(format!("{} is not a table", self.key(key)))),
        }
    }

    fn string(&self, key: &str) -> Checked<&'a str> {
        self.value(key)?
            .as_str()
            .ok_or_else(|| invalid(format!("{} is not a string", self.key(key))))
    }

    fn integer(&self, key: &str, range: RangeInclusive<i64>) -> Checked<i64> {
        self.value(key)?
            .as_integer()
            .filter(|value| range.contains(value))
            .ok_or_else(|| {
                invalid(format!(
                    "{} must be an integer from {} to {}",
                    self.key(key),
                    range.start(),
                    range.end()
                ))
            })
    }

    fn strings(&self, key: &str) -> Checked<Vec<&'a str>> {
        self.value(key)?
            .as_array()
            .and_then(|items| items.iter().map(toml::Value::as_str).collect())
            .ok_or_else(|| invalid(format!("{} is not a list of strings", self.key(key))))
    }

    fn exact(&self, key: &str, expected: &str) -> Checked<()> {
        if self.string(key)? == expected {
            Ok(())
        } else {
            Err(invalid(format!(
                "{} must be {}",
                self.key(key),
                quote(expected)
            )))
        }
    }

    fn exact_list(&self, key: &str, expected: &[&str]) -> Checked<()> {
        if self.strings(key)? == expected {
            Ok(())
        } else {
            Err(invalid(format!(
                "{} must be {}",
                self.key(key),
                list(expected)
            )))
        }
    }
}

/// The values of an accepted config that vary; everything else is fixed by
/// [`shape`].
struct Config {
    id: String,
    root: String,
    environment: Vec<String>,
    vcpus: i64,
    memory_mib: i64,
    ports: Vec<u16>,
}

/// Which image references a config may name.
#[derive(Clone, Copy)]
enum Images<'a> {
    /// Any reference of the right shape (root, and the plain dry run).
    Any,
    /// Only the references listed in this allowlist file.
    Listed(&'a str),
}

/// Check `bytes` against [`shape`] and return the config re-rendered from
/// the checked values alone. The instance id becomes `name` (the helper sets
/// it to the instance name anyway); without a name it is kept.
fn sanitize(bytes: &[u8], name: Option<&str>, images: Images<'_>) -> Checked<String> {
    let text = std::str::from_utf8(bytes).map_err(|_| invalid("it is not UTF-8"))?;
    let document: toml::Table = text.parse().map_err(|error: toml::de::Error| {
        invalid(format!("it is not TOML: {}", shown(error.message())))
    })?;
    let top = Table {
        path: "",
        map: &document,
    };
    top.only(&shape::TOP)?;
    top.integer("schema_version", 1..=1)?;

    let instance = top.table("instance", "instance")?;
    instance.only(&["id", "root", "init", "args", "environment"])?;
    let id = instance.string("id")?;
    if id.is_empty()
        || id.len() > MAX_NAME
        || id.starts_with('.')
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_.-".contains(&byte))
    {
        return Err(invalid("instance.id is not an instance identifier"));
    }
    let root = instance.string("root")?;
    check_image(root)?;
    if let Images::Listed(list) = images {
        check_allowlisted(root, list)?;
    }
    instance.exact("init", shape::INIT)?;
    instance.exact_list("args", &shape::ARGS)?;
    let environment = instance.strings("environment")?;
    let mut seen = Vec::new();
    for entry in &environment {
        let variable = entry.split_once('=').map(|(variable, _)| variable);
        let fits =
            entry.len() <= shape::MAX_ENV_ENTRY && !entry.chars().any(|c| c < ' ' || c == '\x7f');
        match variable {
            Some(variable)
                if fits && shape::ENV_NAMES.contains(&variable) && !seen.contains(&variable) =>
            {
                seen.push(variable);
            }
            _ => {
                return Err(invalid(format!(
                    "instance.environment entry {} is not one of {} once, at most {} bytes without control characters",
                    shown(entry),
                    shape::ENV_NAMES.join(", "),
                    shape::MAX_ENV_ENTRY
                )));
            }
        }
    }

    let resources = top.table("resources", "resources")?;
    resources.only(&["vcpus", "memory_mib"])?;
    let vcpus = resources.integer("vcpus", shape::VCPUS)?;
    let memory_mib = resources.integer("memory_mib", shape::MEMORY_MIB)?;

    let execution = top.table("execution", "execution")?;
    execution.only(&["profile", "backend"])?;
    execution.exact("profile", shape::PROFILE)?;
    execution.exact("backend", shape::BACKEND)?;

    let access = top.table("access", "access")?;
    access.only(&["uid", "gid"])?;
    access.integer("uid", shape::ACCESS_ID..=shape::ACCESS_ID)?;
    access.integer("gid", shape::ACCESS_ID..=shape::ACCESS_ID)?;

    let network = top.table("network", "network")?;
    network.only(&["backend"])?;
    let shared = match network.string("backend")? {
        "none" => false,
        "shared" => true,
        _ => return Err(invalid("network.backend must be \"none\" or \"shared\"")),
    };

    let contract = top.table("contract", "contract")?;
    if shared {
        contract.only(&[
            "version",
            "catalog",
            "default",
            "allow",
            "deny",
            "filesystem",
            "network",
        ])?;
    } else {
        if contract.map.contains_key("network") {
            return Err(invalid(
                "contract.network needs network.backend = \"shared\"",
            ));
        }
        contract.only(&[
            "version",
            "catalog",
            "default",
            "allow",
            "deny",
            "filesystem",
        ])?;
    }
    contract.integer("version", 1..=1)?;
    contract.integer("catalog", shape::CATALOG..=shape::CATALOG)?;
    contract.exact("default", "deny")?;
    contract.exact_list("allow", &allow(shared))?;
    contract.exact_list("deny", &deny(shared))?;

    let filesystem = contract.table("filesystem", "contract.filesystem")?;
    filesystem.only(&["default", "read", "write", "create"])?;
    filesystem.exact("default", "deny")?;
    filesystem.exact_list("read", &shape::FS_READ)?;
    filesystem.exact_list("write", &shape::FS_WRITE)?;
    filesystem.exact_list("create", &shape::FS_WRITE)?;

    let mut ports = Vec::new();
    if shared {
        let outbound = contract.table("network", "contract.network")?;
        outbound.only(&["default", "outbound"])?;
        outbound.exact("default", "deny")?;
        for entry in outbound.strings("outbound")? {
            let port = entry
                .strip_prefix("tcp:")
                .and_then(number)
                .and_then(|port| u16::try_from(port).ok())
                .filter(|port| *port != 0 && ports.last().is_none_or(|last| port > last));
            match port {
                Some(port) => ports.push(port),
                None => {
                    return Err(invalid(format!(
                        "contract.network.outbound entry {} is not tcp:PORT in ascending order",
                        shown(entry)
                    )));
                }
            }
        }
        if ports.is_empty() || ports.len() > shape::MAX_PORTS {
            return Err(invalid(format!(
                "contract.network.outbound must name 1 to {} ports",
                shape::MAX_PORTS
            )));
        }
    }

    let storage = top.table("storage", "storage")?;
    storage.only(&["assignments"])?;
    storage.exact_list("assignments", &[])?;

    let devices = top.table("devices", "devices")?;
    devices.only(&["assignments"])?;
    devices.exact_list("assignments", &shape::DEVICES)?;

    let audit = top.table("audit", "audit")?;
    audit.only(&["enabled"])?;
    if audit.value("enabled")?.as_bool() != Some(true) {
        return Err(invalid("audit.enabled must be true"));
    }

    let failure = top.table("failure", "failure")?;
    failure.only(&["policy"])?;
    failure.exact("policy", "stop")?;

    Ok(render(&Config {
        id: name.unwrap_or(id).to_string(),
        root: root.to_string(),
        environment: environment.into_iter().map(str::to_string).collect(),
        vcpus,
        memory_mib,
        ports,
    }))
}

fn allow(shared: bool) -> Vec<&'static str> {
    let mut allow = shape::ALLOW.to_vec();
    if shared {
        allow.push(shape::ALLOW_NETWORK);
    }
    allow
}

fn deny(shared: bool) -> Vec<&'static str> {
    let mut deny = shape::DENY.to_vec();
    if !shared {
        deny.extend(shape::DENY_OFFLINE);
    }
    deny
}

/// `REGISTRY/REPO@sha256:<64 lowercase hex>` in the instance parser's
/// charset, at most 128 bytes (`crate::runtime::root_digest`).
fn check_image(reference: &str) -> Checked<()> {
    let charset = reference
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || b"_.:/@-".contains(&byte));
    let named = reference
        .split_once("@sha256:")
        .is_some_and(|(image, digest)| {
            digest.len() == 64
                && digest
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                && image.split_once('/').is_some_and(|(registry, repository)| {
                    !registry.is_empty()
                        && !registry.contains(':')
                        && repository.split('/').all(|part| !part.is_empty())
                })
        });
    if charset
        && named
        && reference.len() <= shape::MAX_IMAGE
        && !reference.starts_with('/')
        && !reference.contains("..")
    {
        Ok(())
    } else {
        Err(invalid(format!(
            "instance.root {} is not REGISTRY/REPO@sha256:<64 hex>",
            shown(reference)
        )))
    }
}

/// `reference` is exactly one of the lines of the allowlist `list`, which
/// must be trusted ([`read_allowlist`]); a missing or untrusted list allows
/// nothing.
fn check_allowlisted(reference: &str, list: &str) -> Checked<()> {
    let way_forward =
        "an administrator installs the runtime image with `sudo recipe runtime fetch`";
    let text = read_allowlist(list).map_err(|reason| {
        invalid(format!(
            "untrusted image allowlist {}: {reason}; no image is allowed for this caller ({way_forward})",
            shown(list)
        ))
    })?;
    let listed = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .any(|line| line == reference);
    if listed {
        Ok(())
    } else {
        Err(invalid(format!(
            "instance.root {} is not a runtime image root installed on this host (it is not listed in {}); {way_forward}",
            shown(reference),
            shown(list)
        )))
    }
}

/// The text of the allowlist at `list`: an absolute path whose every parent
/// directory is a real directory owned by root and not writable by group or
/// others, opened without following a final link, a regular file owned by
/// root and not writable by group or others, valid UTF-8 of at most
/// [`MAX_ALLOWLIST_READ`] bytes. The error is the reason it is not trusted.
fn read_allowlist(list: &str) -> Result<String, String> {
    let path = Path::new(list);
    if !path.is_absolute() {
        return Err("it is not an absolute path".into());
    }
    for directory in path.ancestors().skip(1) {
        let meta = std::fs::symlink_metadata(directory)
            .map_err(|error| format!("{}: {error}", directory.display()))?;
        if !meta.file_type().is_dir() || meta.uid() != 0 || meta.mode() & 0o022 != 0 {
            return Err(format!(
                "{} is not a root-owned directory closed to group and others",
                directory.display()
            ));
        }
    }
    let c_path = CString::new(list).map_err(|_| "the path has a NUL byte".to_string())?;
    let flags =
        libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_NOCTTY | libc::O_CLOEXEC;
    // SAFETY: `c_path` is a valid NUL-terminated string.
    let fd = unsafe { libc::open(c_path.as_ptr(), flags) };
    if fd < 0 {
        return Err(format!(
            "it cannot be opened (missing, or a symbolic link): {}",
            std::io::Error::last_os_error()
        ));
    }
    // SAFETY: `fd` was just opened and is owned by nothing else.
    let file = unsafe { File::from_raw_fd(fd) };
    let meta = file
        .metadata()
        .map_err(|error| format!("cannot stat it: {error}"))?;
    if !meta.file_type().is_file() || meta.uid() != 0 || meta.mode() & 0o022 != 0 {
        return Err("it is not a regular file owned by root and closed to group and others".into());
    }
    let mut bytes = Vec::new();
    file.take(MAX_ALLOWLIST_READ as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read it: {error}"))?;
    if bytes.len() > MAX_ALLOWLIST_READ {
        return Err(format!("it is larger than {MAX_ALLOWLIST_READ} bytes"));
    }
    String::from_utf8(bytes).map_err(|_| "it is not UTF-8".to_string())
}

/// The accepted config in exactly the layout `InstanceConfig::render` writes.
fn render(config: &Config) -> String {
    let environment: Vec<&str> = config.environment.iter().map(String::as_str).collect();
    let mut out = format!(
        "schema_version = 1\n\n[instance]\nid = {}\nroot = {}\ninit = {}\nargs = {}\nenvironment = {}\n",
        quote(&config.id),
        quote(&config.root),
        quote(shape::INIT),
        list(&shape::ARGS),
        list(&environment),
    );
    out.push_str(&format!(
        "\n[resources]\nvcpus = {}\nmemory_mib = {}\n",
        config.vcpus, config.memory_mib
    ));
    out.push_str(&format!(
        "\n[execution]\nprofile = {}\nbackend = {}\n",
        quote(shape::PROFILE),
        quote(shape::BACKEND)
    ));
    out.push_str(&format!(
        "\n[access]\nuid = {id}\ngid = {id}\n",
        id = shape::ACCESS_ID
    ));
    let shared = !config.ports.is_empty();
    out.push_str(&format!(
        "\n[contract]\nversion = 1\ncatalog = {}\ndefault = \"deny\"\nallow = {}\ndeny = {}\n",
        shape::CATALOG,
        list(&allow(shared)),
        list(&deny(shared)),
    ));
    out.push_str(&format!(
        "\n[contract.filesystem]\ndefault = \"deny\"\nread = {}\nwrite = {write}\ncreate = {write}\n",
        list(&shape::FS_READ),
        write = list(&shape::FS_WRITE),
    ));
    if shared {
        let outbound: Vec<String> = config
            .ports
            .iter()
            .map(|port| format!("tcp:{port}"))
            .collect();
        let outbound: Vec<&str> = outbound.iter().map(String::as_str).collect();
        out.push_str(&format!(
            "\n[contract.network]\ndefault = \"deny\"\noutbound = {}\n",
            list(&outbound)
        ));
    }
    out.push_str("\n[storage]\nassignments = []\n");
    out.push_str(&format!(
        "\n[devices]\nassignments = {}\n",
        list(&shape::DEVICES)
    ));
    out.push_str(&format!(
        "\n[network]\nbackend = \"{}\"\n",
        if shared { "shared" } else { "none" }
    ));
    out.push_str("\n[audit]\nenabled = true\n");
    out.push_str("\n[failure]\npolicy = \"stop\"\n");
    out
}

/// A TOML basic string; the checked values hold no control characters.
fn quote(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        if c == '"' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

fn list(values: &[&str]) -> String {
    let items: Vec<String> = values.iter().map(|value| quote(value)).collect();
    format!("[{}]", items.join(", "))
}
