//! Who a host run's worker is, and what it may reach.
//!
//! The worker never keeps privilege. Started by an ordinary user it runs as
//! that user (there is nothing to drop) but holds no capability. Started by
//! root (a system-wide install) it runs under an identity of its own: a uid
//! and gid from [`RUN_ID_BASE`] that no other run, account or service has
//! while it holds the slot ([`Slot`]), with no supplementary groups, an empty
//! capability bounding set, no capabilities and locked securebits, so nothing
//! it does regains root or reaches another run.
//!
//! A root run's directories (`crate::temporary::host_pool`) are then laid
//! out so that only its own worker reaches them: the pool is root's and
//! searchable only (0711), the run's own directory in it is root's and
//! searchable by the run's group only while the worker lives ([`Gate`]:
//! 0710 `root:<run gid>`, else 0700 `root:root`), the inputs stay root's and
//! read-only, and `work` and `out` are handed to the run's uid after a
//! restore ([`hand_over`]). Root's own walks of those trees are
//! descriptor-based (`super::snapshot`, `crate::fsx::make_removable`), so they
//! never depend on the worker keeping still; at a checkpoint the worker is
//! stopped anyway ([`Stopped`]) so the archive is consistent. Once the worker
//! has been reaped, anything still running as the run's uid is killed
//! ([`kill_run_processes`]) before the directory is closed and read.

use std::collections::BTreeSet;
use std::ffi::CStr;
use std::fs::{self, File};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use fs2::FileExt;

use crate::fsx::{check, cstring, list_at, lstat_at, open_dir, open_dir_at};

/// Root runs take their uid and gid from a range reserved here:
/// `RUN_ID_BASE .. RUN_ID_BASE + RUN_ID_SLOTS`. The base, 0x70000000
/// (1879048192), lies just above the range systemd reserves for container
/// user namespaces (524288–1879048191) and far above the ids any
/// distribution allocates to accounts, so no file, account or service on
/// the host normally has these ids. A slot is held by an exclusive lock on
/// `<host pool>/slots/<slot>.lock` for the whole run, so concurrent root runs
/// never share an id; a slot whose id an account in /etc/passwd or
/// /etc/group uses is never taken.
pub(crate) const RUN_ID_BASE: u32 = 0x7000_0000;
/// How many root host runs can run at once; more wait for a free slot.
pub(crate) const RUN_ID_SLOTS: u32 = 64;

/// A root run's own uid and gid (the same number), held while it lives.
pub(crate) struct Slot {
    pub(crate) id: u32,
    _lock: File,
}

/// The ids account files use, from `path` (`/etc/passwd` or `/etc/group`:
/// the third field).
fn account_ids(path: &str) -> BTreeSet<u32> {
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| line.split(':').nth(2)?.parse().ok())
        .collect()
}

/// The subordinate id ranges `path` delegates (`/etc/subuid` or
/// `/etc/subgid`: `name:start:count`), as half-open ranges.
fn delegated_ranges(path: &str) -> Vec<std::ops::Range<u64>> {
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| {
            let mut fields = line.split(':').skip(1);
            let start: u64 = fields.next()?.trim().parse().ok()?;
            let count: u64 = fields.next()?.trim().parse().ok()?;
            Some(start..start.saturating_add(count))
        })
        .collect()
}

/// Whether the name service (files, LDAP, sssd, …) knows `id` as a user or a
/// group.
fn known_to_nss(id: u32) -> bool {
    let mut buffer = vec![0 as libc::c_char; 16 * 1024];
    let mut user: libc::passwd = unsafe { std::mem::zeroed() };
    let mut found_user: *mut libc::passwd = std::ptr::null_mut();
    let mut group: libc::group = unsafe { std::mem::zeroed() };
    let mut found_group: *mut libc::group = std::ptr::null_mut();
    unsafe {
        libc::getpwuid_r(
            id,
            &mut user,
            buffer.as_mut_ptr(),
            buffer.len(),
            &mut found_user,
        );
        if !found_user.is_null() {
            return true;
        }
        libc::getgrgid_r(
            id,
            &mut group,
            buffer.as_mut_ptr(),
            buffer.len(),
            &mut found_group,
        );
    }
    !found_group.is_null()
}

/// Whether `id` cannot be a run id: an account or group has it, or it lies
/// in a delegated subordinate range.
fn id_in_use(id: u32, accounts: &BTreeSet<u32>, delegated: &[std::ops::Range<u64>]) -> bool {
    accounts.contains(&id)
        || delegated.iter().any(|range| range.contains(&u64::from(id)))
        || known_to_nss(id)
}

/// Take a free slot, waiting while all are busy. Before it is used, no
/// process runs as its id, and no other run directory in `pool` is open to
/// its group (one an earlier crashed run left open is closed).
pub(crate) fn take_slot(pool: &Path, own: &Path) -> Result<Slot> {
    let slots = pool.join("slots");
    crate::fsx::private_dir(&slots, false)?;
    let accounts: BTreeSet<u32> = account_ids("/etc/passwd")
        .into_iter()
        .chain(account_ids("/etc/group"))
        .collect();
    let delegated: Vec<_> = delegated_ranges("/etc/subuid")
        .into_iter()
        .chain(delegated_ranges("/etc/subgid"))
        .collect();
    let usable: Vec<u32> = (0..RUN_ID_SLOTS)
        .filter(|slot| !id_in_use(RUN_ID_BASE + slot, &accounts, &delegated))
        .collect();
    if usable.is_empty() {
        bail!(
            "every host run id {RUN_ID_BASE}..{} is used by an account or group (/etc/passwd, \
             /etc/group or the name service) or delegated in /etc/subuid or /etc/subgid",
            RUN_ID_BASE + RUN_ID_SLOTS
        );
    }
    let mut waiting = false;
    loop {
        for &slot in &usable {
            let lock = crate::fsx::open_lease(&slots.join(format!("{slot}.lock")), true)?;
            match lock.try_lock_exclusive() {
                Ok(()) => {
                    let id = RUN_ID_BASE + slot;
                    close_other_gates(pool, own, id)?;
                    kill_run_processes(id)?;
                    return Ok(Slot { id, _lock: lock });
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(error) => return Err(error).context("cannot lock a host run slot"),
            }
        }
        if !waiting {
            crate::diagnostics::diag!(
                "recipe: all {RUN_ID_SLOTS} root host run slots are busy; waiting for one"
            );
            waiting = true;
        }
        crate::cancellation::check()?;
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// Close every run directory in `pool` but `own` that is open to the group
/// `id` (left so by a run that crashed): 0700 `root:root`.
fn close_other_gates(pool: &Path, own: &Path, id: u32) -> Result<()> {
    for entry in fs::read_dir(pool)? {
        let entry = entry?;
        let path = entry.path();
        if path == own || !entry.file_type()?.is_dir() {
            continue;
        }
        if fs::symlink_metadata(&path)?.gid() == id {
            Gate::shut(Some(path))?;
        }
    }
    Ok(())
}

/// Whether `/proc/<pid>/status` names `id` among its real, effective, saved
/// and filesystem uids.
fn runs_as(pid: libc::pid_t, id: u32) -> bool {
    fs::read_to_string(format!("/proc/{pid}/status"))
        .ok()
        .and_then(|status| {
            status
                .lines()
                .find_map(|line| line.strip_prefix("Uid:"))
                .map(|ids| {
                    ids.split_whitespace()
                        .any(|uid| uid.parse::<u32>() == Ok(id))
                })
        })
        .unwrap_or(false)
}

/// SIGKILL `pid` if it still runs as `id`, never a process that took over
/// the number: the process is pinned by a pidfd first, its uid checked
/// again, and the signal sent through the pidfd (to the pinned process
/// only; ESRCH once it is gone). Without pidfds (Linux before 5.3), a plain
/// `kill` after the check.
fn kill_if_runs_as(pid: libc::pid_t, id: u32) {
    const SYS_PIDFD_OPEN: libc::c_long = 434;
    const SYS_PIDFD_SEND_SIGNAL: libc::c_long = 424;
    let fd = unsafe { libc::syscall(SYS_PIDFD_OPEN, pid, 0) };
    if fd < 0 {
        if std::io::Error::last_os_error().raw_os_error() == Some(libc::ENOSYS) && runs_as(pid, id)
        {
            unsafe { libc::kill(pid, libc::SIGKILL) };
        }
        return;
    }
    let pidfd = unsafe { <std::os::fd::OwnedFd as std::os::fd::FromRawFd>::from_raw_fd(fd as i32) };
    if runs_as(pid, id) {
        unsafe {
            libc::syscall(
                SYS_PIDFD_SEND_SIGNAL,
                pidfd.as_raw_fd(),
                libc::SIGKILL,
                std::ptr::null::<libc::siginfo_t>(),
                0,
            );
        }
    }
}

/// Kill every process whose real, effective, saved or filesystem uid is
/// `id`, and wait until none is left. A root run's worker is the only process
/// that should ever have that uid, and its seccomp filter refuses new
/// processes; this sweep is the defence if one escaped anyway.
pub(crate) fn kill_run_processes(id: u32) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let mut found = false;
        for entry in fs::read_dir("/proc")? {
            let Ok(entry) = entry else { continue };
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|name| name.parse::<libc::pid_t>().ok())
            else {
                continue;
            };
            if runs_as(pid, id) {
                found = true;
                kill_if_runs_as(pid, id);
            }
        }
        if !found {
            return Ok(());
        }
        if Instant::now() > deadline {
            bail!("processes running as host run uid {id} did not end");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The coordinator's teardown of a started worker: until [`reaped`], leaving
/// kills the worker's process group; until [`sweep`] has run, leaving kills
/// whatever still runs as a root run's uid. (If the coordinator itself dies,
/// the worker's parent-death signal and coordinator watch end it, and the
/// next run that takes the slot sweeps the uid before using it.)
///
/// [`reaped`]: Teardown::reaped
/// [`sweep`]: Teardown::sweep
pub(super) struct Teardown {
    group: Option<libc::pid_t>,
    identity: Option<u32>,
}

impl Teardown {
    pub(super) fn new(worker: libc::pid_t, identity: Option<u32>) -> Self {
        Self {
            group: Some(worker),
            identity,
        }
    }

    /// The worker has been reaped: its process group is no longer killed
    /// (its number may be reused).
    pub(super) fn reaped(&mut self) {
        self.group = None;
    }

    /// Kill everything still running as the run's uid (a root run).
    pub(super) fn sweep(&mut self) -> Result<()> {
        if let Some(id) = self.identity.take() {
            kill_run_processes(id)?;
        }
        Ok(())
    }
}

impl Drop for Teardown {
    fn drop(&mut self) {
        if let Some(group) = self.group.take() {
            unsafe { libc::kill(-group, libc::SIGKILL) };
        }
        let _ = self.sweep();
    }
}

#[derive(Clone, Copy)]
pub(super) struct Limits {
    pub(super) memory: u64,
    pub(super) file_size: Option<u64>,
}

#[repr(C)]
struct CapabilityHeader {
    version: u32,
    pid: libc::c_int,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CapabilityData {
    effective: u32,
    permitted: u32,
    inheritable: u32,
}

/// Empty the calling thread's effective, permitted and inheritable
/// capability sets (`capset`; lowering is always allowed). Async-signal-safe.
pub(crate) fn clear_capabilities() -> std::io::Result<()> {
    const VERSION_3: u32 = 0x2008_0522;
    let header = CapabilityHeader {
        version: VERSION_3,
        pid: 0,
    };
    let data = [CapabilityData {
        effective: 0,
        permitted: 0,
        inheritable: 0,
    }; 2];
    if unsafe { libc::syscall(libc::SYS_capset, &header, data.as_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

fn last_error() -> std::io::Error {
    std::io::Error::last_os_error()
}

/// Set the soft and hard limit of `resource` to `value`, or to the current
/// hard limit when that is lower (an ordinary user cannot raise it).
unsafe fn limit(resource: libc::__rlimit_resource_t, value: u64) -> std::io::Result<()> {
    let mut current = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    if unsafe { libc::getrlimit(resource, &mut current) } < 0 {
        return Err(last_error());
    }
    let value = value.min(current.rlim_max);
    let new = libc::rlimit {
        rlim_cur: value,
        rlim_max: value,
    };
    if unsafe { libc::setrlimit(resource, &new) } < 0 {
        return Err(last_error());
    }
    Ok(())
}

/// The worker's identity and limits, set in the child between `fork` and
/// `exec` (so async-signal-safe: system calls only).
///
/// For a root coordinator (`identity` is the run's slot id): lock the
/// securebits (no privilege for uid 0, no keeping or raising capabilities
/// across the identity change, all locked), empty the bounding set and the
/// ambient set, drop the supplementary groups, take the id as real,
/// effective and saved uid and gid (which empties the remaining capability
/// sets), and check that it holds. For an ordinary user: clear any ambient
/// or held capability. Then, for both: `no_new_privs`, the parent-death
/// signal (after the identity change, which would reset it) and the limits.
pub(super) fn enter_worker_identity(
    identity: Option<u32>,
    parent: libc::pid_t,
    limits: Limits,
) -> std::io::Result<()> {
    unsafe {
        // Every inherited descriptor beyond stdin, stdout and stderr closes
        // at exec, whoever opened it without O_CLOEXEC (close_range with
        // CLOSE_RANGE_CLOEXEC, Linux 5.11; ignored where missing). Marking
        // rather than closing keeps the standard library's own exec-error
        // pipe working until the exec.
        const SYS_CLOSE_RANGE: libc::c_long = 436;
        const CLOSE_RANGE_CLOEXEC: libc::c_uint = 1 << 2;
        libc::syscall(
            SYS_CLOSE_RANGE,
            3 as libc::c_uint,
            libc::c_uint::MAX,
            CLOSE_RANGE_CLOEXEC,
        );
        if let Some(id) = identity {
            const SECBIT_NOROOT: libc::c_ulong = 1 << 0;
            const SECBIT_NOROOT_LOCKED: libc::c_ulong = 1 << 1;
            const SECBIT_NO_SETUID_FIXUP_LOCKED: libc::c_ulong = 1 << 3;
            const SECBIT_KEEP_CAPS_LOCKED: libc::c_ulong = 1 << 5;
            const SECBIT_NO_CAP_AMBIENT_RAISE: libc::c_ulong = 1 << 6;
            const SECBIT_NO_CAP_AMBIENT_RAISE_LOCKED: libc::c_ulong = 1 << 7;
            let bits = SECBIT_NOROOT
                | SECBIT_NOROOT_LOCKED
                | SECBIT_NO_SETUID_FIXUP_LOCKED
                | SECBIT_KEEP_CAPS_LOCKED
                | SECBIT_NO_CAP_AMBIENT_RAISE
                | SECBIT_NO_CAP_AMBIENT_RAISE_LOCKED;
            if libc::prctl(libc::PR_SET_SECUREBITS, bits, 0, 0, 0) < 0 {
                return Err(last_error());
            }
            for capability in 0..64 {
                if libc::prctl(libc::PR_CAPBSET_DROP, capability, 0, 0, 0) < 0
                    && *libc::__errno_location() != libc::EINVAL
                {
                    return Err(last_error());
                }
            }
            if libc::prctl(
                libc::PR_CAP_AMBIENT,
                libc::PR_CAP_AMBIENT_CLEAR_ALL as libc::c_ulong,
                0,
                0,
                0,
            ) < 0
            {
                return Err(last_error());
            }
            if libc::setgroups(0, std::ptr::null()) < 0 {
                return Err(last_error());
            }
            if libc::setresgid(id, id, id) < 0 {
                return Err(last_error());
            }
            if libc::setresuid(id, id, id) < 0 {
                return Err(last_error());
            }
            let (mut ruid, mut euid, mut suid) = (0, 0, 0);
            let (mut rgid, mut egid, mut sgid) = (0, 0, 0);
            if libc::getresuid(&mut ruid, &mut euid, &mut suid) < 0
                || libc::getresgid(&mut rgid, &mut egid, &mut sgid) < 0
                || [ruid, euid, suid, rgid, egid, sgid] != [id; 6]
                || libc::getgroups(0, std::ptr::null_mut()) != 0
                || libc::setresuid(0, 0, 0) == 0
            {
                return Err(std::io::Error::other(
                    "the worker did not take its unprivileged identity",
                ));
            }
        } else {
            // Ignored when unsupported: there is then nothing ambient.
            libc::prctl(
                libc::PR_CAP_AMBIENT,
                libc::PR_CAP_AMBIENT_CLEAR_ALL as libc::c_ulong,
                0,
                0,
                0,
            );
        }
        clear_capabilities()?;
        if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) < 0 {
            return Err(last_error());
        }
        // Last of the credential changes above: the kernel clears the
        // parent-death signal whenever the uid, gid or capabilities change,
        // so it is set only now, then the parent is checked once more (it
        // may have died before the signal was armed).
        if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) < 0 {
            return Err(last_error());
        }
        let mut armed: libc::c_int = 0;
        if libc::prctl(libc::PR_GET_PDEATHSIG, &mut armed as *mut libc::c_int) < 0
            || armed != libc::SIGKILL
        {
            return Err(std::io::Error::other(
                "the parent-death signal did not hold",
            ));
        }
        if libc::getppid() != parent {
            libc::_exit(125);
        }
        let address_space = libc::rlimit {
            rlim_cur: limits.memory,
            rlim_max: limits.memory,
        };
        if libc::setrlimit(libc::RLIMIT_AS, &address_space) < 0 {
            return Err(last_error());
        }
        limit(libc::RLIMIT_CORE, 0)?;
        limit(libc::RLIMIT_NOFILE, 1024)?;
        if let Some(bytes) = limits.file_size {
            if libc::signal(libc::SIGXFSZ, libc::SIG_IGN) == libc::SIG_ERR {
                return Err(last_error());
            }
            limit(libc::RLIMIT_FSIZE, bytes.max(1))?;
        }
    }
    Ok(())
}

/// Prepare a root run's directories for its worker, by descriptor: refuse
/// if anything in the run directory `container` already has the run's id
/// (left by an earlier run), make the run root traversable, give `work` and
/// `out` (everything in them, links themselves included) to the run's id,
/// and take group and other write and setuid/setgid bits off the inputs,
/// which stay root's (their logical modes are in the input mode manifest).
/// Runs while the directory is closed to the worker.
pub(super) fn hand_over(container: &Path, root: &Path, id: u32) -> Result<()> {
    let container_fd = open_dir(container)?;
    unowned(container_fd.as_fd(), id)
        .with_context(|| format!("cannot hand {} to its worker", container.display()))?;
    let root_fd = open_dir(root)?;
    crate::fsx::chmod_fd(root_fd.as_fd(), 0o755)?;
    for top in crate::domain::WRITABLE_ROOTS {
        give(root_fd.as_fd(), &cstring(top.as_bytes())?, id)?;
    }
    let inputs = crate::domain::INPUTS.trim_start_matches('/');
    seal(root_fd.as_fd(), &cstring(inputs.as_bytes())?)?;
    Ok(())
}

/// Nothing below the open directory `dir` has the uid or gid `id`.
fn unowned(dir: BorrowedFd<'_>, id: u32) -> Result<()> {
    for name in list_at(dir)? {
        let (_, metadata) = lstat_at(dir, &name)?;
        if metadata.uid() == id || metadata.gid() == id {
            bail!(
                "{} is already owned by the run id {id}",
                name.to_string_lossy()
            );
        }
        if metadata.is_dir() {
            unowned(open_dir_at(dir, &name)?.as_fd(), id)?;
        }
    }
    Ok(())
}

/// Give the entry `name` below `dir`, and everything below it, to `id`.
fn give(dir: BorrowedFd<'_>, name: &CStr, id: u32) -> Result<()> {
    let (_, metadata) = lstat_at(dir, name)?;
    check(unsafe {
        libc::fchownat(
            dir.as_raw_fd(),
            name.as_ptr(),
            id,
            id,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    })?;
    if metadata.is_dir() {
        let directory = open_dir_at(dir, name)?;
        for child in list_at(directory.as_fd())? {
            give(directory.as_fd(), &child, id)?;
        }
    }
    Ok(())
}

/// Take group and other write and setuid/setgid bits off the entry `name`
/// below `dir` and everything below it (links are left alone).
fn seal(dir: BorrowedFd<'_>, name: &CStr) -> Result<()> {
    let (entry, metadata) = lstat_at(dir, name)?;
    if metadata.file_type().is_symlink() {
        return Ok(());
    }
    if metadata.mode() & 0o6022 != 0 {
        crate::fsx::chmod_fd(entry.as_fd(), metadata.mode() & 0o755)?;
    }
    if metadata.is_dir() {
        let directory = open_dir_at(dir, name)?;
        for child in list_at(directory.as_fd())? {
            seal(directory.as_fd(), &child)?;
        }
    }
    Ok(())
}

/// The run's own directory (root's, in the root-only pool, so its name
/// cannot be replaced) while a root run's worker lives: searchable by the
/// run's group only. [`Gate::close`] (or dropping it, on an early return)
/// makes it root's alone again.
pub(super) struct Gate(Option<PathBuf>);

impl Gate {
    pub(super) fn open(directory: &Path, id: u32) -> Result<Self> {
        let gate = Self(Some(directory.to_path_buf()));
        std::os::unix::fs::lchown(directory, Some(0), Some(id))?;
        fs::set_permissions(
            directory,
            std::os::unix::fs::PermissionsExt::from_mode(0o710),
        )?;
        Ok(gate)
    }

    pub(super) fn close(mut self) -> Result<()> {
        Self::shut(self.0.take())
    }

    fn shut(directory: Option<PathBuf>) -> Result<()> {
        if let Some(directory) = directory {
            fs::set_permissions(
                &directory,
                std::os::unix::fs::PermissionsExt::from_mode(0o700),
            )?;
            std::os::unix::fs::lchown(&directory, Some(0), Some(0))?;
        }
        Ok(())
    }
}

impl Drop for Gate {
    fn drop(&mut self) {
        let _ = Self::shut(self.0.take());
    }
}

/// The worker stopped (SIGSTOP, and waited for until every thread is
/// stopped), continued again when dropped.
pub(super) struct Stopped(libc::pid_t);

impl Stopped {
    pub(super) fn stop(pid: libc::pid_t) -> Result<Self> {
        if unsafe { libc::kill(pid, libc::SIGSTOP) } != 0 {
            return Err(last_error()).context("cannot stop the worker for a checkpoint");
        }
        let stopped = Self(pid);
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        loop {
            let waited = unsafe {
                libc::waitid(
                    libc::P_PID,
                    pid as libc::id_t,
                    &mut info,
                    libc::WSTOPPED | libc::WEXITED | libc::WNOWAIT,
                )
            };
            if waited == 0 {
                break;
            }
            let error = last_error();
            if error.kind() != std::io::ErrorKind::Interrupted {
                return Err(error).context("cannot wait for the worker to stop");
            }
        }
        if info.si_code != libc::CLD_STOPPED {
            bail!("the worker ended during a checkpoint");
        }
        Ok(stopped)
    }
}

impl Drop for Stopped {
    fn drop(&mut self) {
        unsafe {
            libc::kill(self.0, libc::SIGCONT);
        }
    }
}
