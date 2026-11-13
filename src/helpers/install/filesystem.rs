//! Filesystem operation helpers

use crate::core::output;
use crate::helpers::confine::{self, Access};
use rhai::EvalAltResult;
use std::path::{Path, PathBuf};

/// The paths `pattern` matches, sorted. An invalid pattern or an unreadable
/// directory on the way is an error; no match is an empty list.
pub(crate) fn glob_matches(pattern: &str) -> Result<Vec<PathBuf>, Box<EvalAltResult>> {
    // A host run expands its (already checked) pattern itself, never
    // listing or probing anything outside the run's directories.
    if confine::active() {
        return confine::expand(pattern);
    }
    let mut paths = glob::glob(pattern)
        .map_err(|e| format!("invalid pattern: {e}"))?
        .map(|path| path.map_err(|e| format!("glob error: {e}")))
        .collect::<Result<Vec<_>, _>>()?;
    paths.sort();
    Ok(paths)
}

/// Check if a path exists
pub fn exists(path: &str) -> bool {
    // The name itself, so a dangling symlink exists too.
    confine::lstat(Path::new(path)).is_ok()
}

/// Check whether the selected path resolves to a regular file (the caller
/// resolved a final link already in a host run).
pub fn is_file(path: &str) -> bool {
    if confine::active() {
        return confine::lstat(Path::new(path)).is_ok_and(|m| m.is_file());
    }
    Path::new(path).is_file()
}
/// Check whether the selected path resolves to a directory.
pub fn is_dir(path: &str) -> bool {
    if confine::active() {
        return confine::lstat(Path::new(path)).is_ok_and(|m| m.is_dir());
    }
    Path::new(path).is_dir()
}

/// Create a directory and all parent directories
pub fn mkdir(path: &str) -> Result<(), Box<EvalAltResult>> {
    output::detail(&format!("mkdir {}", path));
    confine::mkdir_all(Path::new(path)).map_err(|e| format!("mkdir failed: {}", e).into())
}

/// Remove files matching a glob pattern
pub fn rm_files(pattern: &str) -> Result<(), Box<EvalAltResult>> {
    for path in glob_matches(pattern)? {
        let path = confine::derived(&path, Access::Entry)?;
        output::detail(&format!("rm {}", path.display()));
        if confine::lstat(&path).is_ok_and(|m| m.is_dir()) {
            confine::remove_dir_all(&path)
        } else {
            confine::remove_file(&path)
        }
        .map_err(|e| format!("rm failed: {}", e))?;
    }
    Ok(())
}

/// Check if a glob pattern matches at least one path
pub fn glob_exists(pattern: &str) -> Result<bool, Box<EvalAltResult>> {
    Ok(!glob_matches(pattern)?.is_empty())
}

fn copy_file_impl(src: &Path, dest: &Path) -> Result<(), Box<EvalAltResult>> {
    if !is_file_path(src) {
        return Err(format!("copy source is not a file: {}", src.display()).into());
    }
    crate::helpers::internal::fs_utils::ensure_parent_dir(dest)?;
    output::detail(&format!("cp {} {}", src.display(), dest.display()));
    confine::copy(src, dest).map(|_| ()).map_err(|e| {
        format!(
            "copy failed: {} -> {}: {}",
            src.display(),
            dest.display(),
            e
        )
        .into()
    })
}

fn is_file_path(path: &Path) -> bool {
    is_file(&path.to_string_lossy())
}

fn clone_file_reflink(src: &Path, dest: &Path) -> std::io::Result<()> {
    use std::fs::OpenOptions;
    use std::os::fd::AsRawFd;

    const FICLONE: libc::c_ulong = 0x4004_9409;

    let src_file = confine::open_read(src)?;
    // FICLONE replaces the whole destination, so it needs no truncation first;
    // a failed clone leaves existing bytes in place.
    let dest_file = confine::open(
        dest,
        OpenOptions::new().create(true).write(true).truncate(false),
        libc::O_WRONLY | libc::O_CREAT,
        0o666,
    )?;

    let request = FICLONE as libc::Ioctl;
    let rc = unsafe { libc::ioctl(dest_file.as_raw_fd(), request, src_file.as_raw_fd()) };
    if rc == -1 {
        return Err(std::io::Error::last_os_error());
    }
    dest_file.set_permissions(src_file.metadata()?.permissions())
}

fn copy_path_to_dir(src: &Path, dest_dir: &Path) -> Result<(), Box<EvalAltResult>> {
    let Some(name) = src.file_name() else {
        return Err(format!("copy source has no file name: {}", src.display()).into());
    };
    let src = confine::derived(src, Access::Read)?;
    let dest = confine::derived(&dest_dir.join(name), Access::Write)?;
    copy_file_impl(&src, &dest)
}

/// Copy files matching a glob pattern into a directory
pub fn copy_into_dir(pattern: &str, dest_dir: &str) -> Result<(), Box<EvalAltResult>> {
    let dest_dir = Path::new(dest_dir);
    if !is_dir(&dest_dir.to_string_lossy()) {
        return Err(format!(
            "copy_into_dir destination is not a directory: {}",
            dest_dir.display()
        )
        .into());
    }

    let mut matched = false;
    for path in glob_matches(pattern)? {
        if !is_file_path(&confine::derived(&path, Access::Read)?) {
            continue;
        }
        matched = true;
        copy_path_to_dir(&path, dest_dir)?;
    }

    if !matched {
        return Err(format!("copy_into_dir matched no files: {}", pattern).into());
    }

    Ok(())
}

/// Copy a file to an exact destination path
pub fn copy_file(src: &str, dest: &str) -> Result<(), Box<EvalAltResult>> {
    copy_file_impl(Path::new(src), Path::new(dest))
}

/// Copy a file using reflink semantics when available, falling back to a plain copy
pub fn copy_file_reflink(src: &str, dest: &str) -> Result<(), Box<EvalAltResult>> {
    let src = Path::new(src);
    let dest = Path::new(dest);
    if !is_file_path(src) {
        return Err(format!("copy_file_reflink source is not a file: {}", src.display()).into());
    }
    crate::helpers::internal::fs_utils::ensure_parent_dir(dest)?;
    output::detail(&format!(
        "cp --reflink=auto {} {}",
        src.display(),
        dest.display()
    ));
    match clone_file_reflink(src, dest) {
        Ok(()) => Ok(()),
        // Only "this filesystem cannot clone" falls back to copying.
        Err(e)
            if matches!(
                e.raw_os_error(),
                Some(libc::EOPNOTSUPP | libc::EXDEV | libc::EINVAL | libc::ENOTTY | libc::ENOSYS)
            ) =>
        {
            copy_file_impl(src, dest)
        }
        Err(e) => Err(format!(
            "copy_file_reflink failed: {} -> {}: {}",
            src.display(),
            dest.display(),
            e
        )
        .into()),
    }
}

fn copy_symlink(link_path: &Path, dest_path: &Path) -> Result<(), Box<EvalAltResult>> {
    let target = confine::read_link(link_path)
        .map_err(|e| format!("read_link failed for {}: {}", link_path.display(), e))?;
    output::detail(&format!(
        "ln -s {} {}",
        target.display(),
        dest_path.display()
    ));
    crate::helpers::internal::fs_utils::replace_symlink(&target, dest_path).map_err(|e| {
        format!(
            "symlink copy failed: {} -> {}: {}",
            target.display(),
            dest_path.display(),
            e
        )
        .into()
    })
}

/// Recursively copy the contents of src_dir into dst_dir
pub fn copy_tree_contents(src_dir: &str, dst_dir: &str) -> Result<(), Box<EvalAltResult>> {
    use walkdir::WalkDir;

    let src_root = Path::new(src_dir);
    let dst_root = Path::new(dst_dir);

    if !is_dir(src_dir) {
        return Err(format!(
            "copy_tree_contents source is not a directory: {}",
            src_root.display()
        )
        .into());
    }
    if !is_dir(dst_dir) {
        return Err(format!(
            "copy_tree_contents destination is not a directory: {}",
            dst_root.display()
        )
        .into());
    }

    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let inputs = crate::domain::local(crate::domain::INPUTS);
    let input_modes: crate::domain::InputModes = if src_root.starts_with(&inputs) {
        use std::io::Read;
        let file = confine::open_read(&crate::domain::local(crate::domain::INPUT_MODES))
            .map_err(|e| format!("cannot read immutable input modes: {e}"))?;
        let mut bytes = Vec::new();
        file.take(crate::domain::MAX_INPUT_MODES_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| format!("cannot read immutable input modes: {e}"))?;
        if bytes.len() as u64 > crate::domain::MAX_INPUT_MODES_BYTES {
            return Err("input mode metadata exceeds bound".into());
        }
        serde_json::from_slice(&bytes).map_err(|e| format!("invalid immutable input modes: {e}"))?
    } else {
        Default::default()
    };
    let mut inodes = std::collections::HashMap::<(u64, u64), PathBuf>::new();
    let mut directories = Vec::new();
    for entry in WalkDir::new(src_root).follow_links(false).min_depth(1) {
        let entry = entry.map_err(|e| format!("copy_tree_contents walk failed: {}", e))?;
        let path = entry.path();
        let relative = path.strip_prefix(src_root).map_err(|e| {
            format!(
                "copy_tree_contents strip_prefix failed for {}: {}",
                path.display(),
                e
            )
        })?;
        let dest_path: PathBuf = dst_root.join(relative);
        let metadata = confine::lstat(path).map_err(|e| {
            format!(
                "copy_tree_contents metadata failed for {}: {}",
                path.display(),
                e
            )
        })?;

        let permissions = if let Ok(relative) = path.strip_prefix(&inputs) {
            let name = relative
                .to_str()
                .ok_or("non-Unicode immutable input path")?;
            let mode = *input_modes
                .get(name)
                .ok_or("immutable input mode is missing")?;
            std::fs::Permissions::from_mode(mode & 0o777)
        } else {
            metadata.permissions()
        };
        // Directories are entered and written through; files and links are
        // replaced, never written through.
        let dest_path = confine::derived(
            &dest_path,
            if metadata.is_dir() {
                Access::Write
            } else {
                Access::Entry
            },
        )?;
        if metadata.is_dir() {
            output::detail(&format!("mkdir {}", dest_path.display()));
            confine::mkdir_all(&dest_path).map_err(|e| -> Box<EvalAltResult> {
                format!(
                    "copy_tree_contents mkdir failed for {}: {}",
                    dest_path.display(),
                    e
                )
                .into()
            })?;
            directories.push((dest_path, permissions));
            continue;
        }

        crate::helpers::internal::fs_utils::ensure_parent_dir(&dest_path)?;
        if metadata.file_type().is_symlink() {
            copy_symlink(path, &dest_path)?;
            continue;
        }

        let identity = (metadata.dev(), metadata.ino());
        if let Some(first) = inodes.get(&identity) {
            crate::helpers::internal::fs_utils::remove_existing(&dest_path)
                .map_err(|e| format!("cannot replace hardlink destination: {e}"))?;
            confine::hard_link(first, &dest_path)
                .map_err(|e| format!("copy_tree_contents hardlink failed: {e}"))?;
        } else {
            // Replace rather than write through an existing link or shared inode.
            crate::helpers::internal::fs_utils::remove_existing(&dest_path)
                .map_err(|e| format!("cannot replace copy destination: {e}"))?;
            copy_file_impl(path, &dest_path)?;
            inodes.insert(identity, dest_path.clone());
        }
        confine::chmod(&dest_path, permissions.mode()).map_err(|e| -> Box<EvalAltResult> {
            format!(
                "copy_tree_contents set_permissions failed for {}: {}",
                dest_path.display(),
                e
            )
            .into()
        })?;
    }

    for (path, permissions) in directories.into_iter().rev() {
        confine::chmod(&path, permissions.mode())
            .map_err(|e| format!("copy_tree_contents directory permissions failed: {e}"))?;
    }
    Ok(())
}

/// Copy the first existing source file to an exact destination path
pub fn copy_first_existing(sources: rhai::Array, dest: &str) -> Result<String, Box<EvalAltResult>> {
    let dest = Path::new(dest);
    let mut candidates = Vec::new();

    for item in sources {
        let source = item.to_string();
        let trimmed = source.trim();
        if trimmed.is_empty() {
            continue;
        }
        candidates.push(trimmed.to_owned());
        let resolved = confine::read(trimmed)?;
        let src_path = Path::new(&resolved);
        if is_file_path(src_path) {
            copy_file_impl(src_path, dest)?;
            return Ok(trimmed.to_owned());
        }
    }

    Err(format!(
        "copy_first_existing found no existing file for destination {} in {:?}",
        dest.display(),
        candidates
    )
    .into())
}

/// Move/rename a file
pub fn move_file(src: &str, dest: &str) -> Result<(), Box<EvalAltResult>> {
    output::detail(&format!("mv {} -> {}", src, dest));
    confine::rename(Path::new(src), Path::new(dest)).map_err(|e| format!("mv failed: {}", e).into())
}

/// Create a symbolic link
pub fn symlink(src: &str, dest: &str) -> Result<(), Box<EvalAltResult>> {
    output::detail(&format!("ln -s {} {}", src, dest));
    confine::symlink(Path::new(src), Path::new(dest))
        .map_err(|e| format!("symlink failed: {}", e).into())
}

/// Create or replace a symbolic link
pub fn symlink_force(src: &str, dest: &str) -> Result<(), Box<EvalAltResult>> {
    let dest_path = Path::new(dest);
    output::detail(&format!("ln -sfn {} {}", src, dest));

    if confine::lstat(dest_path).is_ok_and(|meta| meta.is_dir()) {
        return Err(format!("ln_force destination is an existing directory: {}", dest).into());
    }
    crate::helpers::internal::fs_utils::replace_symlink(Path::new(src), dest_path)
        .map_err(|e| format!("symlink failed: {}", e).into())
}

/// Change file permissions
pub fn chmod_file(path: &str, mode: i64) -> Result<(), Box<EvalAltResult>> {
    output::detail(&format!("chmod {:o} {}", mode, path));
    confine::chmod(Path::new(path), mode as u32).map_err(|e| format!("chmod failed: {}", e).into())
}
