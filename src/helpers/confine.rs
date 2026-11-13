//! The path rule of a host run: every path a helper is given resolves beneath
//! the invocation's own directories, or the helper refuses it.
//!
//! In a Ginux run the domain confines paths and this module does nothing: its
//! file operations are the plain `std::fs` ones. In a host run
//! (`crate::execution`) the worker is an ordinary process, and these checks
//! are the boundary:
//!
//! - Paths are absolute, as everywhere in Recipe, and may not use `..`: a
//!   recipe builds its paths from `RUNTIME.paths`. `.` and repeated slashes
//!   are ignored.
//! - [`resolve`] examines each component with `lstat`. A symbolic link among
//!   the parents is expanded (a `..` in its target is applied to the real
//!   directory the walk is in), and the walk continues through its target; a
//!   link that leads outside the allowed directories is refused. Once a
//!   component is missing, everything after it is a new name: nothing after
//!   it is followed, and a `..` after it is refused. The resolved path's
//!   parents are all real directories beneath an allowed root.
//! - The last component is followed the same way by operations that open or
//!   change what it names (read, copy source, verify, extract, write, chmod),
//!   and not followed by operations on the name itself (rm, mv, ln, exists).
//! - Inputs are read-only; work and out are read-write. The roots themselves
//!   cannot be removed, moved or replaced.
//!
//! The operations then act on the resolved path through descriptors, so the
//! kernel holds them to the same rule even if the walk were wrong: the
//! parent directory is opened from the root's directory descriptor with
//! `openat2(RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS | RESOLVE_NO_MAGICLINKS)`
//! (a component-by-component `openat(O_NOFOLLOW)` where the kernel lacks
//! `openat2`), and the final name is used with the `*at` call on that
//! descriptor, never following a link (the walk already resolved any link
//! the operation follows). See [`open`], [`lstat`], [`mkdir_all`], [`chmod`],
//! [`remove_file`], [`remove_dir_all`], [`rename`], [`symlink`],
//! [`hard_link`], [`read_link`], [`copy`] and [`Staged`].

use rhai::EvalAltResult;
use std::collections::VecDeque;
use std::ffi::{CStr, CString, OsStr, OsString};
use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};

use crate::fsx::{check, chmod_fd, cstring, list_at, lstat_at, openat};

/// What an operation does with the path it is given.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Access {
    /// Reads what the path names, following a final link (read, verify, copy
    /// source, extract source). Any allowed root.
    Read,
    /// Looks at the name itself without following a final link (exists, list).
    /// Any allowed root.
    Look,
    /// Creates or changes what the path names, following a final link (write,
    /// copy destination, chmod, mkdir, extract destination). Work or out.
    Write,
    /// Removes, moves or creates the name itself without following a final
    /// link (rm, mv, ln). Work or out, strictly below the root.
    Entry,
}

impl Access {
    fn follows(self) -> bool {
        matches!(self, Self::Read | Self::Write)
    }
    fn writes(self) -> bool {
        matches!(self, Self::Write | Self::Entry)
    }
}

/// The invocation's directories below the host root.
struct Roots {
    root: PathBuf,
    inputs: PathBuf,
    work: PathBuf,
    out: PathBuf,
}

fn roots() -> Option<Roots> {
    let root = crate::domain::host_root()?.to_path_buf();
    Some(Roots {
        inputs: crate::domain::local(crate::domain::INPUTS),
        work: crate::domain::local(crate::domain::WORK),
        out: crate::domain::local(crate::domain::OUT),
        root,
    })
}

impl Roots {
    /// The root `path` lies in (or is), and whether that root is writable.
    fn containing(&self, path: &Path) -> Option<(&Path, bool)> {
        [(&self.inputs, false), (&self.work, true), (&self.out, true)]
            .into_iter()
            .find(|(root, _)| path.starts_with(root))
            .map(|(root, writable)| (root.as_path(), writable))
    }

    /// A position the walk may pass through: the host root's own ancestors
    /// (to reach an absolute link target) or anything within a root.
    fn passable(&self, path: &Path) -> bool {
        self.root.starts_with(path) || self.containing(path).is_some()
    }
}

/// Whether helpers confine paths in this worker (a host run).
pub(crate) fn active() -> bool {
    crate::domain::host_root().is_some()
}

fn refuse(path: &str, why: &str) -> Box<EvalAltResult> {
    let domain_spelling = ["/work", "/out", "/inputs"]
        .iter()
        .any(|domain| path == *domain || path.starts_with(&format!("{domain}/")));
    let hint = if domain_spelling {
        " (this run is on the host: spell domain paths with RUNTIME.paths, \
         e.g. RUNTIME.paths.build, RUNTIME.paths.output, RUNTIME.paths.inputs)"
    } else {
        ""
    };
    format!("{path}: {why}{hint}").into()
}

const OUTSIDE: &str = "is outside this run's directories (its inputs, work and out); \
                       a recipe running on the host reaches only those";

const PARENT: &str = "uses `..`; paths may not use `..` in a host run; \
                      build the path from RUNTIME.paths";

/// Refuse a host-run path that is not absolute or that uses `..`.
fn check_spelling(path: &str) -> Result<(), Box<EvalAltResult>> {
    if !path.starts_with('/') || path.contains('\0') {
        return Err(refuse(path, "is not an absolute path"));
    }
    if Path::new(path)
        .components()
        .any(|component| component == Component::ParentDir)
    {
        return Err(refuse(path, PARENT));
    }
    Ok(())
}

/// Resolve `path` for `access`. In a Ginux run the path is returned as given.
pub(crate) fn resolve(path: &str, access: Access) -> Result<PathBuf, Box<EvalAltResult>> {
    let Some(roots) = roots() else {
        return Ok(PathBuf::from(path));
    };
    check_spelling(path)?;
    let mut pending: VecDeque<OsString> = VecDeque::new();
    push_front(&mut pending, Path::new(path));
    let mut current = PathBuf::from("/");
    let mut links = 0usize;
    let mut missing = false;
    while let Some(part) = pending.pop_front() {
        if part == ".." {
            // Only a link target brings a `..` here (the path itself may not
            // use one). After a missing component the directory it would
            // climb out of does not exist, so nothing after it may be
            // resolved against what does.
            if missing {
                return Err(refuse(
                    path,
                    "passes through a link whose target climbs out of a missing directory with `..`",
                ));
            }
            current.pop();
            continue;
        }
        // Skip straight to the host root: it was created canonical by the
        // coordinator, so its ancestors are real directories.
        if current == Path::new("/") && !missing {
            let joined: PathBuf = std::iter::once(OsString::from("/"))
                .chain(std::iter::once(part.clone()))
                .chain(pending.iter().cloned())
                .collect();
            if let Ok(rest) = joined.strip_prefix(&roots.root)
                && !rest.components().any(|c| c == Component::ParentDir)
            {
                let skipped = roots.root.components().count() - 1;
                current = roots.root.clone();
                for _ in 1..skipped {
                    pending.pop_front();
                }
                continue;
            }
        }
        let next = current.join(&part);
        if !roots.passable(&next) {
            return Err(refuse(path, OUTSIDE));
        }
        if missing {
            // Below a missing directory: a new name, never examined or
            // followed.
            current = next;
            continue;
        }
        let last = pending.is_empty();
        match std::fs::symlink_metadata(&next) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                // Everything from here on is a new name the operation creates.
                missing = true;
                current = next;
            }
            Err(error) => {
                return Err(format!("{path}: cannot inspect {}: {error}", next.display()).into());
            }
            Ok(metadata) if metadata.file_type().is_symlink() && (!last || access.follows()) => {
                links += 1;
                if links > 40 {
                    return Err(refuse(path, "has too many levels of symbolic links"));
                }
                let target = std::fs::read_link(&next)
                    .map_err(|e| format!("{path}: cannot read link {}: {e}", next.display()))?;
                if target.is_absolute() {
                    current = PathBuf::from("/");
                    if roots.containing(&lexical(&target)).is_none() {
                        return Err(refuse(
                            path,
                            &format!(
                                "passes through the link {} -> {}, which leads outside this run's directories",
                                next.display(),
                                target.display()
                            ),
                        ));
                    }
                }
                push_front(&mut pending, &target);
            }
            Ok(_) => current = next,
        }
    }
    let Some((root, writable)) = roots.containing(&current) else {
        return Err(refuse(path, OUTSIDE));
    };
    if access.writes() && !writable {
        return Err(refuse(path, "is in this run's inputs, which are read-only"));
    }
    if access == Access::Entry && current == root {
        return Err(refuse(
            path,
            "is one of this run's own directories and cannot be removed or replaced",
        ));
    }
    Ok(current)
}

fn push_front(pending: &mut VecDeque<OsString>, path: &Path) {
    let parts: Vec<OsString> = path
        .components()
        .filter_map(|component| match component {
            Component::Normal(name) => Some(name.to_owned()),
            Component::ParentDir => Some(OsString::from("..")),
            Component::RootDir | Component::CurDir | Component::Prefix(_) => None,
        })
        .collect();
    for part in parts.into_iter().rev() {
        pending.push_front(part);
    }
}

/// `path` with `.` and `..` applied lexically (for a quick containment test).
fn lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::from("/");
    for component in path.components() {
        match component {
            Component::Normal(name) => out.push(name),
            Component::ParentDir => {
                out.pop();
            }
            _ => {}
        }
    }
    out
}

/// [`resolve`] as a string, for helpers that take `&str`.
pub(crate) fn text(path: &str, access: Access) -> Result<String, Box<EvalAltResult>> {
    if !active() {
        return Ok(path.to_owned());
    }
    resolve(path, access)?
        .into_os_string()
        .into_string()
        .map_err(|_| format!("{path}: resolves to a non-UTF-8 path").into())
}

/// Shorthands for the common accesses.
pub(crate) fn read(path: &str) -> Result<String, Box<EvalAltResult>> {
    text(path, Access::Read)
}
pub(crate) fn look(path: &str) -> Result<String, Box<EvalAltResult>> {
    text(path, Access::Look)
}
pub(crate) fn write(path: &str) -> Result<String, Box<EvalAltResult>> {
    text(path, Access::Write)
}
pub(crate) fn entry(path: &str) -> Result<String, Box<EvalAltResult>> {
    text(path, Access::Entry)
}

/// Resolve a path a multi-file helper derived itself (a copied tree's member,
/// a glob match); the helper then uses the resolved path. In a Ginux run the
/// path is returned as given.
pub(crate) fn derived(path: &Path, access: Access) -> Result<PathBuf, Box<EvalAltResult>> {
    if !active() {
        return Ok(path.to_path_buf());
    }
    let text = path
        .to_str()
        .ok_or_else(|| format!("{}: non-UTF-8 path", path.display()))?;
    resolve(text, access)
}

/// Check the glob pattern a host run may expand: absolute, without `..`.
/// The expansion itself is [`expand`]. In a Ginux run the pattern passes.
pub(crate) fn pattern(pattern: &str) -> Result<String, Box<EvalAltResult>> {
    if !active() {
        return Ok(pattern.to_owned());
    }
    if pattern.contains('\0') || !pattern.starts_with('/') {
        return Err(refuse(pattern, "is not an absolute path"));
    }
    if Path::new(pattern)
        .components()
        .any(|component| component == Component::ParentDir)
    {
        return Err(refuse(
            pattern,
            "uses `..`, which a host run's patterns may not",
        ));
    }
    Ok(pattern.to_owned())
}

fn has_wildcard(part: &OsStr) -> bool {
    part.as_bytes()
        .iter()
        .any(|byte| matches!(byte, b'*' | b'?' | b'['))
}

/// Expand a host run's glob pattern without leaving the run's directories:
/// the literal directory part is resolved like any path; below it, a
/// wildcard component lists only real directories, a directory link is
/// descended only when it resolves inside the run (others are skipped, so
/// nothing outside is listed or probed), and `**` descends only real
/// directories. A pattern without wildcards names the one path itself (rm of
/// a link removes the link). Sorted; no match is an empty list.
pub(crate) fn expand(pattern: &str) -> Result<Vec<PathBuf>, Box<EvalAltResult>> {
    let parts: Vec<OsString> = Path::new(pattern)
        .components()
        .filter_map(|component| match component {
            Component::Normal(name) => Some(name.to_owned()),
            _ => None,
        })
        .collect();
    let split = parts
        .iter()
        .position(|part| has_wildcard(part))
        .unwrap_or(parts.len());
    let literal: PathBuf = std::iter::once(OsString::from("/"))
        .chain(parts[..split].iter().cloned())
        .collect();
    let literal = literal
        .to_str()
        .ok_or_else(|| format!("{pattern}: non-UTF-8 pattern"))?
        .to_owned();
    let rest = &parts[split..];
    if rest.is_empty() {
        let resolved = resolve(&literal, Access::Look)?;
        return Ok(match lstat(&resolved) {
            Ok(_) => vec![resolved],
            Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(format!("{pattern}: {error}").into()),
        });
    }
    let mut compiled = Vec::with_capacity(rest.len());
    for part in rest {
        let text = part
            .to_str()
            .ok_or_else(|| format!("{pattern}: non-UTF-8 pattern"))?;
        compiled.push(if text == "**" {
            Part::Any
        } else if has_wildcard(part) {
            Part::Pattern(glob::Pattern::new(text).map_err(|e| format!("invalid pattern: {e}"))?)
        } else {
            Part::Literal(part.clone())
        });
    }
    let base = resolve(&literal, Access::Read)?;
    let mut matches = Vec::new();
    if lstat(&base).is_ok_and(|metadata| metadata.is_dir()) {
        walk(&base, &compiled, &mut matches)?;
    }
    matches.sort();
    matches.dedup();
    Ok(matches)
}

enum Part {
    Literal(OsString),
    Pattern(glob::Pattern),
    Any,
}

/// A real directory to descend into for `path`, a match below a real
/// directory: itself, or the in-run directory a link there resolves to.
fn descend(path: &Path) -> Option<PathBuf> {
    let metadata = lstat(path).ok()?;
    if metadata.is_dir() {
        return Some(path.to_path_buf());
    }
    if !metadata.file_type().is_symlink() {
        return None;
    }
    let resolved = resolve(path.to_str()?, Access::Read).ok()?;
    lstat(&resolved)
        .is_ok_and(|metadata| metadata.is_dir())
        .then_some(resolved)
}

fn walk(dir: &Path, parts: &[Part], out: &mut Vec<PathBuf>) -> Result<(), Box<EvalAltResult>> {
    let Some((part, rest)) = parts.split_first() else {
        out.push(dir.to_path_buf());
        return Ok(());
    };
    let names = || -> Result<Vec<OsString>, Box<EvalAltResult>> {
        let mut names = std::fs::read_dir(dir)
            .map_err(|e| format!("glob error: cannot list {}: {e}", dir.display()))?
            .map(|entry| entry.map(|entry| entry.file_name()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("glob error: cannot list {}: {e}", dir.display()))?;
        names.sort();
        Ok(names)
    };
    match part {
        Part::Literal(name) => {
            let path = dir.join(name);
            if rest.is_empty() {
                if lstat(&path).is_ok() {
                    out.push(path);
                }
            } else if let Some(next) = descend(&path) {
                walk(&next, rest, out)?;
            }
        }
        Part::Pattern(pattern) => {
            for name in names()? {
                let Some(text) = name.to_str() else { continue };
                if !pattern.matches_with(text, glob::MatchOptions::new()) {
                    continue;
                }
                let path = dir.join(&name);
                if rest.is_empty() {
                    out.push(path);
                } else if let Some(next) = descend(&path) {
                    walk(&next, rest, out)?;
                }
            }
        }
        Part::Any => {
            // Zero directories, then each real subdirectory in turn.
            walk(dir, rest, out)?;
            if rest.is_empty() {
                // A trailing `**` names everything below.
                for name in names()? {
                    let path = dir.join(&name);
                    if lstat(&path).is_ok_and(|metadata| metadata.is_dir()) {
                        walk(&path, parts, out)?;
                    } else {
                        out.push(path);
                    }
                }
                return Ok(());
            }
            for name in names()? {
                let path = dir.join(&name);
                if lstat(&path).is_ok_and(|metadata| metadata.is_dir()) {
                    walk(&path, parts, out)?;
                }
            }
        }
    }
    Ok(())
}

// ============================================================================
// Descriptor-based operations on resolved paths
// ============================================================================

fn outside() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "path is outside this run's directories",
    )
}

/// Where a resolved path lies: its root's directory descriptor and the path
/// below it, all plain names. None outside a host run.
struct Located {
    root: OwnedFd,
    relative: PathBuf,
}

fn locate(path: &Path) -> io::Result<Option<Located>> {
    let Some(roots) = roots() else {
        return Ok(None);
    };
    let Some((root, _)) = roots.containing(path) else {
        return Err(outside());
    };
    let relative = path
        .strip_prefix(root)
        .map_err(|_| outside())?
        .to_path_buf();
    if relative
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(outside());
    }
    let directory = cstring(root.as_os_str().as_bytes())?;
    let fd = check(unsafe {
        libc::open(
            directory.as_ptr(),
            libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    })?;
    Ok(Some(Located {
        root: unsafe { OwnedFd::from_raw_fd(fd) },
        relative,
    }))
}

/// Open `relative` (plain names) below `dir`, following no link at all and
/// never leaving `dir`: `openat2` where the kernel has it, else one
/// `openat(O_NOFOLLOW)` per component. With `O_PATH | O_NOFOLLOW` a final
/// link is opened itself.
fn beneath(
    dir: BorrowedFd<'_>,
    relative: &Path,
    flags: libc::c_int,
    mode: libc::mode_t,
) -> io::Result<OwnedFd> {
    let name = if relative.as_os_str().is_empty() {
        cstring(b".")?
    } else {
        cstring(relative.as_os_str().as_bytes())?
    };
    let mut how: libc::open_how = unsafe { std::mem::zeroed() };
    how.flags = (flags | libc::O_CLOEXEC) as u64;
    how.mode = if flags & (libc::O_CREAT | libc::O_TMPFILE) != 0 {
        mode as u64
    } else {
        0
    };
    how.resolve = libc::RESOLVE_BENEATH | libc::RESOLVE_NO_SYMLINKS | libc::RESOLVE_NO_MAGICLINKS;
    let fd = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            dir.as_raw_fd(),
            name.as_ptr(),
            &how as *const libc::open_how,
            std::mem::size_of::<libc::open_how>(),
        )
    };
    if fd >= 0 {
        return Ok(unsafe { OwnedFd::from_raw_fd(fd as libc::c_int) });
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() != Some(libc::ENOSYS) {
        return Err(error);
    }
    // No openat2: walk the components without following any link.
    let parts: Vec<&OsStr> = relative.iter().collect();
    let Some((last, parents)) = parts.split_last() else {
        return openat(dir, c".", flags, mode);
    };
    let mut current: Option<OwnedFd> = None;
    for part in parents {
        let base = current.as_ref().map_or(dir, |fd| fd.as_fd());
        current = Some(openat(
            base,
            &cstring(part.as_bytes())?,
            libc::O_PATH | libc::O_DIRECTORY | libc::O_NOFOLLOW,
            0,
        )?);
    }
    let base = current.as_ref().map_or(dir, |fd| fd.as_fd());
    openat(
        base,
        &cstring(last.as_bytes())?,
        flags | libc::O_NOFOLLOW,
        mode,
    )
}

/// A resolved path's parent directory (opened beneath its root) and final
/// name. The root itself is `(root, ".")`.
struct Parent {
    dir: OwnedFd,
    name: CString,
}

fn parent(located: Located) -> io::Result<Parent> {
    let Some(name) = located.relative.file_name() else {
        return Ok(Parent {
            dir: located.root,
            name: cstring(b".")?,
        });
    };
    let name = cstring(name.as_bytes())?;
    let above = located.relative.parent().unwrap_or(Path::new(""));
    let dir = if above.as_os_str().is_empty() {
        located.root
    } else {
        beneath(
            located.root.as_fd(),
            above,
            libc::O_PATH | libc::O_DIRECTORY,
            0,
        )?
    };
    Ok(Parent { dir, name })
}

fn parent_of(path: &Path) -> io::Result<Option<Parent>> {
    locate(path)?.map(parent).transpose()
}

/// Open a file a resolved path names, following no link (the walk already
/// resolved them all) and never leaving its root. In a Ginux run, a plain
/// open with `options`.
pub(crate) fn open(
    path: &Path,
    options: &std::fs::OpenOptions,
    flags: libc::c_int,
    mode: libc::mode_t,
) -> io::Result<std::fs::File> {
    let Some(located) = locate(path)? else {
        return options.open(path);
    };
    let fd = beneath(located.root.as_fd(), &located.relative, flags, mode)?;
    Ok(std::fs::File::from(fd))
}

/// Open a resolved path for reading.
pub(crate) fn open_read(path: &Path) -> io::Result<std::fs::File> {
    open(
        path,
        std::fs::OpenOptions::new().read(true),
        libc::O_RDONLY,
        0,
    )
}

/// The metadata of the name itself (`lstat`).
pub(crate) fn lstat(path: &Path) -> io::Result<std::fs::Metadata> {
    let Some(located) = locate(path)? else {
        return std::fs::symlink_metadata(path);
    };
    let fd = beneath(
        located.root.as_fd(),
        &located.relative,
        libc::O_PATH | libc::O_NOFOLLOW,
        0,
    )?;
    std::fs::File::from(fd).metadata()
}

/// Create a directory and its missing parents, none of them through a link.
pub(crate) fn mkdir_all(path: &Path) -> io::Result<()> {
    let Some(located) = locate(path)? else {
        return std::fs::create_dir_all(path);
    };
    let mut current = located.root;
    for part in located.relative.iter() {
        let name = cstring(part.as_bytes())?;
        let flags = libc::O_PATH | libc::O_DIRECTORY | libc::O_NOFOLLOW;
        current = match openat(current.as_fd(), &name, flags, 0) {
            Ok(fd) => fd,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let made = unsafe { libc::mkdirat(current.as_raw_fd(), name.as_ptr(), 0o777) };
                if made < 0 {
                    let error = io::Error::last_os_error();
                    if error.kind() != io::ErrorKind::AlreadyExists {
                        return Err(error);
                    }
                }
                openat(current.as_fd(), &name, flags, 0)?
            }
            Err(error) => return Err(error),
        };
    }
    Ok(())
}

/// Change the mode of what a resolved path names; a symbolic link is refused.
pub(crate) fn chmod(path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let Some(Parent { dir, name }) = parent_of(path)? else {
        return std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
    };
    const SYS_FCHMODAT2: libc::c_long = 452;
    let changed = unsafe {
        libc::syscall(
            SYS_FCHMODAT2,
            dir.as_raw_fd(),
            name.as_ptr(),
            mode as libc::mode_t,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if changed == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    match error.raw_os_error() {
        Some(libc::ENOSYS) => {}
        Some(libc::EOPNOTSUPP) => {
            return Err(io::Error::other(
                "is a symbolic link; its mode cannot be changed",
            ));
        }
        _ => return Err(error),
    }
    // No fchmodat2: the final name itself, opened without following it; a
    // link is refused, and anything else is changed through its descriptor,
    // so a swap after the check cannot redirect the change.
    let (fd, metadata) = lstat_at(dir.as_fd(), &name)?;
    if metadata.file_type().is_symlink() {
        return Err(io::Error::other(
            "is a symbolic link; its mode cannot be changed",
        ));
    }
    chmod_fd(fd.as_fd(), mode)
}

/// Remove the name itself (a file or a link).
pub(crate) fn remove_file(path: &Path) -> io::Result<()> {
    let Some(Parent { dir, name }) = parent_of(path)? else {
        return std::fs::remove_file(path);
    };
    check(unsafe { libc::unlinkat(dir.as_raw_fd(), name.as_ptr(), 0) }).map(drop)
}

/// Remove a directory tree, never following a link inside it.
pub(crate) fn remove_dir_all(path: &Path) -> io::Result<()> {
    let Some(Parent { dir, name }) = parent_of(path)? else {
        return std::fs::remove_dir_all(path);
    };
    remove_tree_at(dir.as_fd(), &name)
}

fn remove_tree_at(dir: BorrowedFd<'_>, name: &CStr) -> io::Result<()> {
    let child = match openat(
        dir,
        name,
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW,
        0,
    ) {
        Ok(fd) => fd,
        // Not a directory (or a link to one): remove the name itself.
        Err(error) if matches!(error.raw_os_error(), Some(libc::ENOTDIR | libc::ELOOP)) => {
            return check(unsafe { libc::unlinkat(dir.as_raw_fd(), name.as_ptr(), 0) }).map(drop);
        }
        Err(error) => return Err(error),
    };
    for entry in list_at(child.as_fd())? {
        remove_tree_at(child.as_fd(), &entry)?;
    }
    drop(child);
    check(unsafe { libc::unlinkat(dir.as_raw_fd(), name.as_ptr(), libc::AT_REMOVEDIR) }).map(drop)
}

/// Rename one name to another (both below the run's writable roots).
pub(crate) fn rename(from: &Path, to: &Path) -> io::Result<()> {
    let (Some(source), Some(target)) = (parent_of(from)?, parent_of(to)?) else {
        return std::fs::rename(from, to);
    };
    check(unsafe {
        libc::renameat(
            source.dir.as_raw_fd(),
            source.name.as_ptr(),
            target.dir.as_raw_fd(),
            target.name.as_ptr(),
        )
    })
    .map(drop)
}

/// Create the symbolic link `path` pointing at `target` (any text: a link is
/// data until something follows it, and the walk checks it then).
pub(crate) fn symlink(target: &Path, path: &Path) -> io::Result<()> {
    let Some(Parent { dir, name }) = parent_of(path)? else {
        return std::os::unix::fs::symlink(target, path);
    };
    let target = cstring(target.as_os_str().as_bytes())?;
    check(unsafe { libc::symlinkat(target.as_ptr(), dir.as_raw_fd(), name.as_ptr()) }).map(drop)
}

/// Create the hard link `path` to the file `existing`, not following a link.
pub(crate) fn hard_link(existing: &Path, path: &Path) -> io::Result<()> {
    let (Some(source), Some(target)) = (parent_of(existing)?, parent_of(path)?) else {
        return std::fs::hard_link(existing, path);
    };
    check(unsafe {
        libc::linkat(
            source.dir.as_raw_fd(),
            source.name.as_ptr(),
            target.dir.as_raw_fd(),
            target.name.as_ptr(),
            0,
        )
    })
    .map(drop)
}

/// The target of the symbolic link `path`.
pub(crate) fn read_link(path: &Path) -> io::Result<PathBuf> {
    let Some(Parent { dir, name }) = parent_of(path)? else {
        return std::fs::read_link(path);
    };
    let mut buffer = vec![0u8; libc::PATH_MAX as usize];
    let length = unsafe {
        libc::readlinkat(
            dir.as_raw_fd(),
            name.as_ptr(),
            buffer.as_mut_ptr().cast(),
            buffer.len(),
        )
    };
    if length < 0 {
        return Err(io::Error::last_os_error());
    }
    buffer.truncate(length as usize);
    Ok(PathBuf::from(
        <OsString as std::os::unix::ffi::OsStringExt>::from_vec(buffer),
    ))
}

/// Copy the regular file `from` to `to` (created or truncated), with the
/// source's permissions, as `std::fs::copy` does.
pub(crate) fn copy(from: &Path, to: &Path) -> io::Result<u64> {
    if !active() {
        return std::fs::copy(from, to);
    }
    let mut source = open(
        from,
        std::fs::OpenOptions::new().read(true),
        libc::O_RDONLY | libc::O_NONBLOCK,
        0,
    )?;
    let metadata = source.metadata()?;
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the source is not a regular file",
        ));
    }
    let mut target = open(
        to,
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true),
        libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC,
        0o666,
    )?;
    let copied = io::copy(&mut source, &mut target)?;
    target.set_permissions(metadata.permissions())?;
    Ok(copied)
}

/// A new file beside its destination, published by [`Staged::persist`] and
/// removed if dropped first.
pub(crate) struct Staged {
    file: std::fs::File,
    path: PathBuf,
    kept: bool,
}

impl Staged {
    /// Create `DIR/PREFIX<random>` exclusively, readable and writable.
    pub(crate) fn new(dir: &Path, prefix: &str) -> io::Result<Self> {
        use std::hash::{BuildHasher, Hasher};
        for _ in 0..64 {
            let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
            hasher.write_u32(std::process::id());
            let path = dir.join(format!("{prefix}{:016x}", hasher.finish()));
            match open(
                &path,
                std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create_new(true),
                libc::O_RDWR | libc::O_CREAT | libc::O_EXCL,
                0o600,
            ) {
                Ok(file) => {
                    return Ok(Self {
                        file,
                        path,
                        kept: false,
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::other("cannot choose a staging name"))
    }

    pub(crate) fn file(&mut self) -> &mut std::fs::File {
        &mut self.file
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Rename the staged file over `dest`.
    pub(crate) fn persist(mut self, dest: &Path) -> io::Result<()> {
        rename(&self.path, dest)?;
        self.kept = true;
        Ok(())
    }
}

impl Drop for Staged {
    fn drop(&mut self) {
        if !self.kept {
            let _ = remove_file(&self.path);
        }
    }
}
