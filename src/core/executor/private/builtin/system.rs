//! Package groups and system trees (docs/recipe-format.md, sections 2.6 and
//! 2.8): unpacking the pinned packages into a group's tree and finishing
//! every `system` output. Together with the download step and `FILES` this
//! is what the ~218 lines of group code copied into 46 recipes did.

use super::fetch::Fetched;
use super::{Outputs, helper, remove_any, text};
use crate::core::output;
use crate::declarations::Declared;
use crate::helpers::{acquire, confine};
use crate::needs::PathKind;
use crate::pins::Format;
use anyhow::{Context, Result, bail};

/// The folders a system package does not own: run-time data, mount points
/// and home folders. Removed from every `system` tree before it is finalized.
const NOT_OWNED: [&str; 12] = [
    "run",
    "tmp",
    "boot",
    "home",
    "root",
    "proc",
    "sys",
    "dev",
    "lost+found",
    "var/run",
    "var/tmp",
    "var/lock",
];

/// Section 2.6 step 2, for a recipe with `PACKAGES` and no `build`: every
/// pinned package into the one output tree, natively, without running its
/// scripts, in pin order: first the packages whose `var/` is not kept, then
/// `var/` is deleted, then the packages whose `var/` is kept as state seeds.
/// On a run restored from a checkpoint the restored tree has them already.
pub(crate) fn unpack_packages(
    declared: &Declared,
    fetched: &Fetched,
    outputs: &Outputs,
    resumed: bool,
) -> Result<()> {
    if declared.packages.is_none() || declared.functions.contains(&"build") || resumed {
        return Ok(());
    }
    let [(_, PathKind::Directory, tree)] = outputs.as_slice() else {
        bail!("a recipe with PACKAGES and no build(src, out) has one directory output");
    };
    let format = fetched
        .format
        .context("the download step chose no pins section")?;
    // A fresh tree: what a restored earlier result left is not this run's.
    remove_any(tree)?;
    confine::mkdir_all(tree).with_context(|| format!("cannot create {}", tree.display()))?;
    let total = fetched.packages.len() as u64;
    let mut done = 0;
    for kept in [false, true] {
        for package in fetched
            .packages
            .iter()
            .filter(|package| package.pin.var == kept)
        {
            output::progress_report(done, total, "packages");
            let archive = text(&package.path)?;
            acquire::verify_sha256(archive, &package.pin.sha256)
                .map_err(helper)
                .with_context(|| format!("the pinned package {}", package.pin.name))?;
            match format {
                Format::Rpm => crate::helpers::target::extract_rpm(archive, text(tree)?),
                Format::Apk => crate::helpers::target::extract_apk(archive, text(tree)?),
            }
            .map_err(helper)
            .with_context(|| format!("unpacking the pinned package {}", package.pin.name))?;
            done += 1;
        }
        if !kept {
            remove_any(&tree.join("var"))?;
        }
    }
    output::progress_report(done, total, "packages");
    crate::target::flush_pending()
}

/// Finish every `system` output (section 2.8): remove the folders a system
/// package does not own, then finalize the tree with owner names resolved in
/// the root it is written into, `var/` as state seeds, and the `ACCOUNTS` of
/// this system's C library.
pub(crate) fn finish_system(declared: &Declared, outputs: &Outputs, host_libc: &str) -> Result<()> {
    for (name, kind, tree) in outputs {
        if name != "system" {
            continue;
        }
        if *kind != PathKind::Directory {
            bail!("the system output is a directory");
        }
        for folder in NOT_OWNED {
            remove_any(&tree.join(folder))?;
        }
        let accounts = declared
            .accounts
            .get(host_libc)
            .cloned()
            .unwrap_or_default();
        crate::target::finalize_at_apply(tree, accounts, true)
            .context("finishing the system tree")?;
    }
    Ok(())
}
