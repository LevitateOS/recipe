//! `FILES` (docs/recipe-format.md, section 2.5, decisions 6 and 7): each
//! named file or folder of the sources into the one output, before `build`.
//!
//! Files are moved, not copied, from an unpacked archive (plan section 2,
//! requirement 3); a rename within one filesystem, a copy and a removal only
//! across two. What is moved is gone from `src.NAME`: `build` and `configure`
//! see what `FILES` left there. A file `FILES` names again later is taken
//! from where the earlier entry put it, and copied (section 2.5: "a source
//! file named by two entries is copied for the later one"); a later entry
//! that names the place it already is at only sets its mode. A file (or
//! folder) entry beside a folder entry of the same source renames it: it is
//! moved out of where the folder entry put it, and a folder entry placed
//! after it leaves out what it took, so it is not also at its old place. A
//! plain-file source and a `git` checkout are the downloads themselves and
//! are copied (see [`super::fetch::Held`]).

use super::fetch::{Held, Unpacked};
use super::{Outputs, helper, remove_any, text};
use crate::declarations::{Declared, FileEntry};
use crate::helpers::confine;
use crate::needs::PathKind;
use anyhow::{Context, Result, bail, ensure};
use std::path::{Path, PathBuf};

/// Place every `FILES` entry into the output. On a run restored from a
/// checkpoint the restored output holds them already.
pub(crate) fn place_files(
    declared: &Declared,
    unpacked: &Unpacked,
    outputs: &Outputs,
    resumed: bool,
) -> Result<()> {
    if declared.files.is_empty() || resumed {
        return Ok(());
    }
    let [(output, kind, out)] = outputs.as_slice() else {
        bail!("FILES is for a recipe with one output");
    };
    let system = output == "system";
    // Each entry's source key ("NAME" or "NAME/PATH") and where it put it.
    let mut placed: Vec<(String, PathBuf)> = Vec::new();
    for (index, entry) in declared.files.iter().enumerate() {
        let from = if entry.path.is_empty() {
            entry.source.clone()
        } else {
            format!("{}/{}", entry.source, entry.path)
        };
        let what = format!("FILES[{index}] ({from})");
        // A source of another C library is not fetched on this system; its
        // entries go with it (the declaration reader checked the name).
        let Some((origin, held)) = unpacked.sources.get(&entry.source) else {
            continue;
        };
        // Named again by an exact earlier entry: copied. Inside a folder an
        // earlier entry placed: renamed out of the output (it is ours). Else
        // from the source.
        let (current, copy) = match locate(&placed, &from) {
            Some((at, exact)) => (at, exact),
            None if entry.path.is_empty() => (origin.clone(), *held == Held::Stored),
            None => (origin.join(&entry.path), *held == Held::Stored),
        };
        let metadata = confine::lstat(&current)
            .with_context(|| format!("{what}: the source has no {}", display(entry)))?;
        if entry.folder {
            ensure!(
                metadata.is_dir(),
                "{what}: {} is not a folder",
                display(entry)
            );
            let dest = if entry.to.is_empty() {
                out.clone()
            } else {
                out.join(entry.to.trim_end_matches('/'))
            };
            if copy {
                copy_into(&current, &dest)?;
            } else {
                move_into(&current, &dest)?;
            }
            // What an earlier entry took out of this folder was renamed: it
            // is not also at its old place (a copied source still has it).
            let inner = format!("{from}/");
            for (earlier, at) in placed.iter().filter(|(key, _)| key.starts_with(&inner)) {
                let old = dest.join(&earlier[inner.len()..]);
                if old != *at && confine::lstat(&old).is_ok() {
                    remove_any(&old)?;
                }
            }
            placed.push((from, dest));
            continue;
        }
        ensure!(
            !metadata.is_dir(),
            "{what}: {} is a folder; end from with / to place all of it",
            display(entry)
        );
        let dest = if *kind == PathKind::File {
            out.clone()
        } else if entry.to.is_empty() || entry.to.ends_with('/') {
            // The file keeps its name in the source (a plain-file source:
            // its saved file's name), wherever an earlier entry put it.
            let named = if entry.path.is_empty() {
                origin.as_path()
            } else {
                Path::new(&entry.path)
            };
            let name = named
                .file_name()
                .with_context(|| format!("{what}: the source has no file name"))?;
            out.join(entry.to.trim_end_matches('/')).join(name)
        } else {
            out.join(&entry.to)
        };
        if dest != current {
            if copy {
                copy_any(&current, &dest)?;
            } else {
                move_any(&current, &dest)?;
            }
        }
        if !confine::lstat(&dest)?.file_type().is_symlink() {
            let mode = entry.mode.unwrap_or(0o644);
            if system {
                // The mode goes into the tree's target metadata (a setuid
                // Xorg.wrap keeps working), as target_mode records it.
                let relative = dest.strip_prefix(out).context("placed outside the tree")?;
                crate::helpers::target::mode(text(out)?, text(relative)?, i64::from(mode))
                    .map_err(helper)?;
            } else {
                confine::chmod(&dest, mode)
                    .with_context(|| format!("{what}: cannot set the mode {mode:o}"))?;
            }
        }
        placed.push((from, dest));
    }
    Ok(())
}

fn display(entry: &FileEntry) -> String {
    match (entry.path.is_empty(), entry.folder) {
        (true, true) => format!("{}/", entry.source),
        (true, false) => entry.source.clone(),
        (false, true) => format!("{}/{}/", entry.source, entry.path),
        (false, false) => format!("{}/{}", entry.source, entry.path),
    }
}

/// Where an earlier entry put `from` (or the folder it is in), if one did,
/// and whether that entry named `from` itself (rather than its folder).
fn locate(placed: &[(String, PathBuf)], from: &str) -> Option<(PathBuf, bool)> {
    placed.iter().rev().find_map(|(key, at)| {
        if key == from {
            Some((at.clone(), true))
        } else {
            from.strip_prefix(key.as_str())
                .and_then(|rest| rest.strip_prefix('/'))
                .map(|rest| (at.join(rest), false))
        }
    })
}

fn make_parent(path: &Path) -> Result<()> {
    crate::helpers::internal::fs_utils::ensure_parent_dir(path).map_err(helper)
}

/// Move `from` (a file or link) to `to`, replacing what is there.
fn move_any(from: &Path, to: &Path) -> Result<()> {
    make_parent(to)?;
    remove_any(to)?;
    rename_or_copy(from, to)
}

/// Rename; across two filesystems, copy and remove the source.
fn rename_or_copy(from: &Path, to: &Path) -> Result<()> {
    match confine::rename(from, to) {
        Ok(()) => Ok(()),
        Err(error) if error.raw_os_error() == Some(libc::EXDEV) => {
            copy_any(from, to)?;
            remove_any(from)
        }
        Err(error) => Err(error)
            .with_context(|| format!("cannot move {} to {}", from.display(), to.display())),
    }
}

/// Move everything in the folder `from` into the folder `to`, merging with
/// folders already there; `from` stays, empty (it may be `src.NAME`).
fn move_into(from: &Path, to: &Path) -> Result<()> {
    if confine::lstat(to).is_err() {
        make_parent(to)?;
        rename_or_copy(from, to)?;
        return confine::mkdir_all(from)
            .with_context(|| format!("cannot keep the folder {}", from.display()));
    }
    merge(from, to)
}

fn merge(from: &Path, to: &Path) -> Result<()> {
    ensure!(
        confine::lstat(to)?.is_dir(),
        "{} is in the way of a folder",
        to.display()
    );
    for entry in
        std::fs::read_dir(from).with_context(|| format!("cannot list {}", from.display()))?
    {
        let name = entry?.file_name();
        let (source, dest) = (from.join(&name), to.join(&name));
        let folders = confine::lstat(&source)?.is_dir()
            && confine::lstat(&dest).is_ok_and(|metadata| metadata.is_dir());
        if folders {
            merge(&source, &dest)?;
        } else {
            remove_any(&dest)?;
            rename_or_copy(&source, &dest)?;
        }
    }
    Ok(())
}

/// Copy everything in the folder `from` into the folder `to`.
fn copy_into(from: &Path, to: &Path) -> Result<()> {
    confine::mkdir_all(to).with_context(|| format!("cannot create {}", to.display()))?;
    crate::helpers::install::copy_tree_contents(text(from)?, text(to)?).map_err(helper)
}

/// Copy `from` (a file, a link or a folder) to `to`, replacing what is
/// there; a file is cloned where the filesystem allows it.
fn copy_any(from: &Path, to: &Path) -> Result<()> {
    let metadata = confine::lstat(from)?;
    if metadata.is_dir() {
        remove_any(to)?;
        return copy_into(from, to);
    }
    make_parent(to)?;
    remove_any(to)?;
    if metadata.file_type().is_symlink() {
        let target = confine::read_link(from)?;
        return crate::helpers::internal::fs_utils::replace_symlink(&target, to)
            .with_context(|| format!("cannot copy the link {}", from.display()));
    }
    crate::helpers::install::copy_file_reflink(text(from)?, text(to)?).map_err(helper)
}
