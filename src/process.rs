//! The one way this crate runs a child process (other than the Ginux launcher,
//! whose fd inheritance belongs to `launch`).
//!
//! A [`Run`] states its policy explicitly: what stdin it gets, where live
//! output goes ([`Route`]), how much output is kept ([`Keep`]), whether the
//! child gets its own process group, and an optional deadline. The executing
//! thread's environment overlay ([`push_environment`]) applies to every child.

use crate::diagnostics::{self, Stream};
use std::collections::{BTreeMap, VecDeque};
use std::ffi::{OsStr, OsString};
use std::io::{Read, Write};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

thread_local! {
    static COMMAND_ENV: std::cell::RefCell<BTreeMap<OsString, OsString>> =
        const { std::cell::RefCell::new(BTreeMap::new()) };
}

/// Restores the executing thread's previous environment overlay on drop.
/// Guards nest: drop them in reverse order of creation.
pub(crate) struct EnvironmentGuard(BTreeMap<OsString, OsString>);

impl Drop for EnvironmentGuard {
    fn drop(&mut self) {
        COMMAND_ENV.with(|env| *env.borrow_mut() = std::mem::take(&mut self.0));
    }
}

/// Layer `values` over the executing thread's environment for child processes.
pub(crate) fn push_environment(
    values: impl IntoIterator<Item = (OsString, OsString)>,
) -> EnvironmentGuard {
    COMMAND_ENV.with(|env| {
        let mut env = env.borrow_mut();
        let previous = env.clone();
        env.extend(values);
        EnvironmentGuard(previous)
    })
}

/// A variable as a child would see it: the thread overlay, then the process env.
pub(crate) fn environment(name: &str) -> Option<OsString> {
    COMMAND_ENV
        .with(|env| env.borrow().get(OsStr::new(name)).cloned())
        .or_else(|| std::env::var_os(name))
}

/// Variables that point git at a repository other than its working directory.
/// Every git child this crate starts removes them.
pub(crate) const GIT_REPOSITORY_LOCATORS: [&str; 6] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_COMMON_DIR",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
];

/// Find an executable regular file named `program` on the child's PATH.
pub(crate) fn find_program(program: &str) -> Option<std::path::PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    std::env::split_paths(&environment("PATH")?)
        .map(|dir| dir.join(program))
        .find(|path| {
            std::fs::metadata(path)
                .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        })
}

/// Where live child output goes while it runs.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Route {
    /// Into the diagnostic stream as child stdout/stderr.
    Diagnostics,
    /// To this process's own stderr, outside the diagnostic stream.
    OutOfBand,
    /// Nowhere.
    Silent,
}

/// How much child output is returned.
#[derive(Clone, Copy)]
pub(crate) enum Keep {
    /// Everything.
    All,
    /// The last `n` bytes of stdout and stderr combined, for failure messages.
    Tail(usize),
    /// Everything, but fail the run once a stream exceeds `n` bytes.
    Limit(usize),
}

/// How a child ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Exit {
    Code(i32),
    Signal(i32),
}

impl Exit {
    pub(crate) fn success(self) -> bool {
        self == Self::Code(0)
    }
}

impl std::fmt::Display for Exit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Code(code) => write!(f, "exit status: {code}"),
            Self::Signal(signal) => write!(f, "signal: {signal}"),
        }
    }
}

/// A finished child.
pub(crate) struct Finished {
    pub exit: Exit,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    /// [`Keep::Tail`]: interleaved tail of both streams.
    pub tail: Vec<u8>,
}

/// Why a run produced no [`Finished`].
#[derive(Debug)]
pub(crate) enum Error {
    /// The program could not be started.
    Spawn(String),
    /// The run started but did not complete normally.
    Failed(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn(message) | Self::Failed(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for Error {}

/// One child process run.
pub(crate) struct Run {
    command: Command,
    stdin: Option<Vec<u8>>,
    route_stdout: Route,
    route_stderr: Route,
    keep: Keep,
    deadline: Option<Duration>,
    own_group: bool,
}

impl Run {
    pub(crate) fn new(command: Command) -> Self {
        Self {
            command,
            stdin: None,
            route_stdout: Route::Silent,
            route_stderr: Route::Silent,
            keep: Keep::All,
            deadline: None,
            own_group: false,
        }
    }

    /// Feed `bytes` to the child's stdin, then close it (default: `/dev/null`).
    pub(crate) fn stdin(mut self, bytes: Vec<u8>) -> Self {
        self.stdin = Some(bytes);
        self
    }

    pub(crate) fn route(mut self, stdout: Route, stderr: Route) -> Self {
        self.route_stdout = stdout;
        self.route_stderr = stderr;
        self
    }

    pub(crate) fn keep(mut self, keep: Keep) -> Self {
        self.keep = keep;
        self
    }

    /// Kill the child (and its group, if any) when it outlives `deadline`.
    /// Deadline runs poll and also stop on cancellation; they are for the
    /// coordinator, because the Ginux domain does not emulate sleeping.
    pub(crate) fn deadline(mut self, deadline: Duration) -> Self {
        self.deadline = Some(deadline);
        self
    }

    /// Run the child in its own process group, killed with it on exit, and
    /// tied to this process's lifetime.
    pub(crate) fn own_group(mut self) -> Self {
        self.own_group = true;
        self
    }

    /// The command line as shown in messages. Environment values are left out
    /// because they can hold secrets.
    pub(crate) fn label(&self) -> String {
        let mut text = self.command.get_program().to_string_lossy().into_owned();
        for arg in self.command.get_args() {
            text.push(' ');
            text.push_str(&arg.to_string_lossy());
        }
        if let Some(dir) = self.command.get_current_dir() {
            text.push_str(&format!("\n  in: {}", dir.display()));
        }
        text
    }

    pub(crate) fn run(mut self) -> Result<Finished, Error> {
        let label = self.label();
        apply_environment(&mut self.command);
        self.command
            .stdin(if self.stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if self.own_group {
            self.command.process_group(0);
            let parent = unsafe { libc::getpid() };
            // Only async-signal-safe calls run between fork and exec.
            unsafe {
                self.command.pre_exec(move || {
                    if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    if libc::getppid() != parent {
                        libc::_exit(125);
                    }
                    Ok(())
                });
            }
        }
        let mut child = self
            .command
            .spawn()
            .map_err(|error| Error::Spawn(format!("command failed to start: {label}: {error}")))?;
        let started = Instant::now();
        let group = self.own_group.then(|| Group(child.id() as i32));

        let tail_bytes = match self.keep {
            Keep::Tail(bytes) => bytes,
            _ => 0,
        };
        let tail = Arc::new(Mutex::new(VecDeque::with_capacity(tail_bytes.min(8192))));
        let over_limit = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let reader = |pipe: Option<Box<dyn Read + Send>>, stream: Stream, route: Route| {
            let tail = Arc::clone(&tail);
            let over_limit = Arc::clone(&over_limit);
            let keep = self.keep;
            pipe.map(|pipe| {
                std::thread::Builder::new().spawn(move || {
                    drain(pipe, stream, route, keep, &tail, &over_limit)
                })
            })
        };
        let stdout = reader(
            child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>),
            Stream::Stdout,
            self.route_stdout,
        );
        let stderr = reader(
            child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>),
            Stream::Stderr,
            self.route_stderr,
        );
        let writer = match (self.stdin.take(), child.stdin.take()) {
            (Some(bytes), Some(mut pipe)) => Some(std::thread::spawn(move || {
                // Dropping the pipe after the write closes the child's stdin.
                pipe.write_all(&bytes)
            })),
            _ => None,
        };

        let mut problem = None;
        // Without a deadline, block in wait(): the Ginux domain does not
        // emulate sleeping, so only deadline-bound (coordinator) runs poll.
        let status = match self.deadline {
            None => match child.wait() {
                Ok(status) => Some(status),
                Err(error) => {
                    problem = Some(format!("command failed to run: {label}: {error}"));
                    None
                }
            },
            Some(deadline) => loop {
                match child.try_wait() {
                    Ok(Some(status)) => break Some(status),
                    Ok(None) => {}
                    Err(error) => {
                        problem = Some(format!("command failed to run: {label}: {error}"));
                        break None;
                    }
                }
                if over_limit.load(std::sync::atomic::Ordering::Relaxed) {
                    problem = Some(format!("command output exceeded its byte limit: {label}"));
                    break None;
                }
                if crate::cancellation::signal() != 0 {
                    problem = Some(format!("command cancelled: {label}"));
                    break None;
                }
                if started.elapsed() > deadline {
                    problem = Some(format!(
                        "command timed out after {}s: {label}",
                        deadline.as_secs()
                    ));
                    break None;
                }
                std::thread::sleep(Duration::from_millis(25));
            },
        };
        if status.is_none() {
            let _ = child.kill();
            let _ = child.wait();
        }
        // An exited leader cannot leave pipe-holding descendants in its group.
        drop(group);

        let mut collect = |thread: Option<std::io::Result<std::thread::JoinHandle<std::io::Result<Vec<u8>>>>>| {
            match thread {
                None => Vec::new(),
                Some(Err(error)) => {
                    problem.get_or_insert(format!("command output reader failed to start: {label}: {error}"));
                    Vec::new()
                }
                Some(Ok(handle)) => match handle.join() {
                    Ok(Ok(bytes)) => bytes,
                    Ok(Err(error)) => {
                        problem.get_or_insert(format!("command output read failed: {label}: {error}"));
                        Vec::new()
                    }
                    Err(_) => {
                        problem.get_or_insert(format!("command output drain panicked: {label}"));
                        Vec::new()
                    }
                },
            }
        };
        let stdout = collect(stdout);
        let stderr = collect(stderr);
        if let Some(writer) = writer {
            match writer.join() {
                Ok(Ok(())) => {}
                // A child that exits without reading all input is judged by its exit.
                Ok(Err(error)) if error.kind() == std::io::ErrorKind::BrokenPipe => {}
                Ok(Err(error)) => {
                    problem.get_or_insert(format!("command input write failed: {label}: {error}"));
                }
                Err(_) => {
                    problem.get_or_insert(format!("command input writer panicked: {label}"));
                }
            }
        }
        if problem.is_none() && over_limit.load(std::sync::atomic::Ordering::Relaxed) {
            problem = Some(format!("command output exceeded its byte limit: {label}"));
        }
        if let Some(problem) = problem {
            return Err(Error::Failed(problem));
        }
        let status = status.expect("no problem implies an exit status");
        let exit = match (status.code(), status.signal()) {
            (Some(code), _) => Exit::Code(code),
            (None, Some(signal)) => Exit::Signal(signal),
            (None, None) => unreachable!("an exit status has a code or a signal"),
        };
        let tail = tail
            .lock()
            .map(|tail| tail.iter().copied().collect())
            .unwrap_or_default();
        Ok(Finished {
            exit,
            stdout,
            stderr,
            tail,
        })
    }
}

/// Kills a child's process group when dropped.
struct Group(i32);

impl Drop for Group {
    fn drop(&mut self) {
        unsafe {
            libc::kill(-self.0, libc::SIGKILL);
        }
    }
}

fn apply_environment(command: &mut Command) {
    // Explicit per-command values, including removals, take precedence over
    // the thread overlay.
    let explicit: Vec<_> = command
        .get_envs()
        .map(|(key, value)| (key.to_owned(), value.map(OsStr::to_owned)))
        .collect();
    COMMAND_ENV.with(|env| command.envs(env.borrow().iter()));
    for (key, value) in explicit {
        match value {
            Some(value) => command.env(key, value),
            None => command.env_remove(key),
        };
    }
}

fn drain(
    mut pipe: Box<dyn Read + Send>,
    stream: Stream,
    route: Route,
    keep: Keep,
    tail: &Mutex<VecDeque<u8>>,
    over_limit: &std::sync::atomic::AtomicBool,
) -> std::io::Result<Vec<u8>> {
    let mut kept = Vec::new();
    let mut buffer = [0u8; 8192];
    loop {
        let count = match pipe.read(&mut buffer) {
            Ok(0) => return Ok(kept),
            Ok(count) => count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        let bytes = &buffer[..count];
        // Always drain the pipe; a failed diagnostic write is remembered by
        // the worker sink, which then refuses to report success.
        match route {
            Route::Diagnostics => {
                let _ = diagnostics::write(stream, bytes);
            }
            Route::OutOfBand => diagnostics::out_of_band(bytes),
            Route::Silent => {}
        }
        match keep {
            Keep::All => kept.extend_from_slice(bytes),
            Keep::Limit(limit) => {
                if kept.len() + count > limit {
                    over_limit.store(true, std::sync::atomic::Ordering::Relaxed);
                } else {
                    kept.extend_from_slice(bytes);
                }
            }
            Keep::Tail(limit) => {
                let mut tail = tail
                    .lock()
                    .map_err(|_| std::io::Error::other("command output lock poisoned"))?;
                for byte in bytes {
                    if tail.len() == limit {
                        tail.pop_front();
                    }
                    tail.push_back(*byte);
                }
            }
        }
    }
}
