//! Writing into a system tree (docs/recipe-format.md, section 5, decision
//! 17): `put`, `put_link`, `tree_text` and `enable_service`, with the tree as
//! their first argument. They replace the copies 45 group recipes carried,
//! which took `ctx` instead of the tree.
//!
//! The tree is the system output `configure(tree)` receives (or any
//! directory a recipe passes); `path` is relative to it. A mode goes into the
//! tree's target metadata, as `target_mode` records it.

use super::confine::{self, Access};
use rhai::EvalAltResult;
use std::cell::Cell;
use std::path::{Component, Path, PathBuf};

thread_local! {
    /// `PACKAGE_FORMAT` of the recipe this worker runs (`rpm`, `apk`), set
    /// with the engine's constants; `enable_service` chooses by it.
    static FORMAT: Cell<Option<&'static str>> = const { Cell::new(None) };
}

/// Set `PACKAGE_FORMAT` for [`enable_service`]: `None` without `PACKAGES`.
pub(crate) fn set_package_format(format: Option<&'static str>) {
    FORMAT.with(|cell| cell.set(format));
}

/// Who a missing file belongs to, in an error: the recipe being run.
fn owner() -> String {
    let name = crate::core::output::event_recipe();
    if name.is_empty() {
        "this recipe".into()
    } else {
        name
    }
}

/// `tree/path`, confined for `access`; `path` is relative and has no `.` or
/// `..` part.
fn member(
    tree: &str,
    path: &str,
    access: Access,
    helper: &str,
) -> Result<PathBuf, Box<EvalAltResult>> {
    let relative = Path::new(path);
    if path.is_empty()
        || relative.is_absolute()
        || relative
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(format!(
            "{helper}: {path:?} is a path inside the tree, relative, without . or .. parts"
        )
        .into());
    }
    confine::derived(&Path::new(tree).join(relative), access)
}

fn text(path: &Path) -> Result<&str, Box<EvalAltResult>> {
    path.to_str()
        .ok_or_else(|| format!("{}: not a UTF-8 path", path.display()).into())
}

/// Make the folder `dest` goes into.
fn parent_of(dest: &Path) -> Result<(), Box<EvalAltResult>> {
    crate::helpers::internal::fs_utils::ensure_parent_dir(dest)
}

/// Remove what `dest` names (a file, a link or a folder), if anything.
fn clear(dest: &Path) -> Result<(), Box<EvalAltResult>> {
    match confine::lstat(dest) {
        Ok(metadata) if metadata.is_dir() => confine::remove_dir_all(dest),
        Ok(_) => confine::remove_file(dest),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
    .map_err(|error| format!("cannot replace {}: {error}", dest.display()).into())
}

/// `put(tree, path, text, mode)`: `text` at `path`, replacing what is there,
/// with `mode` in the target metadata.
pub fn put(tree: &str, path: &str, content: &str, mode: i64) -> Result<(), Box<EvalAltResult>> {
    let tree = confine::write(tree)?;
    let dest = member(&tree, path, Access::Entry, "put")?;
    parent_of(&dest)?;
    clear(&dest)?;
    super::install::write_file(text(&dest)?, content)?;
    super::target::mode(&tree, path, mode)
}

/// `put_link(tree, path, target)`: a symbolic link at `path` to `target`.
pub fn put_link(tree: &str, path: &str, target: &str) -> Result<(), Box<EvalAltResult>> {
    let tree = confine::write(tree)?;
    let dest = member(&tree, path, Access::Entry, "put_link")?;
    parent_of(&dest)?;
    super::install::symlink_force(target, text(&dest)?)
}

/// `tree_text(tree, path)`: the text of a packaged file; fails naming it when
/// the packages no longer ship it.
pub fn tree_text(tree: &str, path: &str) -> Result<String, Box<EvalAltResult>> {
    let tree = confine::read(tree)?;
    let file = member(&tree, path, Access::Read, "tree_text")?;
    if !super::install::is_file(text(&file)?) {
        return Err(format!("{}: its packages no longer ship /{path}", owner()).into());
    }
    super::install::read_file(text(&file)?)
}

/// `enable_service(tree, unit, service)`: start a system service at boot in
/// this system's init's own form. On an `rpm` tree (systemd) the links
/// `systemctl enable` makes for `unit`, its `Alias=` names included (D-Bus
/// activation finds `dbus-org.*` through them); on an `apk` tree (OpenRC)
/// `service` in the default runlevel. An empty name skips that system.
pub fn enable_service(tree: &str, unit: &str, service: &str) -> Result<(), Box<EvalAltResult>> {
    match FORMAT.with(Cell::get) {
        Some("rpm") => {
            if unit.is_empty() {
                return Ok(());
            }
            let file = format!("usr/lib/systemd/system/{unit}");
            let text = tree_text(tree, &file)?;
            let target = format!("/{file}");
            put_link(
                tree,
                &format!("etc/systemd/system/multi-user.target.wants/{unit}"),
                &target,
            )?;
            for line in text.lines() {
                if let Some(aliases) = line.strip_prefix("Alias=") {
                    for alias in aliases.split_whitespace() {
                        put_link(tree, &format!("etc/systemd/system/{alias}"), &target)?;
                    }
                }
            }
            Ok(())
        }
        Some("apk") => {
            if service.is_empty() {
                return Ok(());
            }
            let script = format!("etc/init.d/{service}");
            let root = confine::read(tree)?;
            let found = member(&root, &script, Access::Read, "enable_service")?;
            if !super::install::is_file(text(&found)?) {
                return Err(format!(
                    "{}: its packages no longer ship the OpenRC service {service}",
                    owner()
                )
                .into());
            }
            put_link(
                tree,
                &format!("etc/runlevels/default/{service}"),
                &format!("/{script}"),
            )
        }
        _ => Err(
            "enable_service chooses systemd or OpenRC by PACKAGE_FORMAT, which only a recipe \
             with PACKAGES has; write the links with put_link instead"
                .into(),
        ),
    }
}
