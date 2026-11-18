//! `recipe --system apply-release FOLDER` and `record-base`: a system release
//! applied to a root as one Recipe operation (`docs/release-engineering.md`
//! sections 6 and 10). The root is chosen as for any system change: the other
//! root of an A/B system, the running root of a system without A/B, or
//! `--root DIR`.
//!
//! 1. The manifest is read and checked against the root (its os-release
//!    `ID`, and a serial not lower than the one the root records), and both
//!    archives against the manifest's SHA-256, before anything changes.
//! 2. The base: `base.tar.zst` is unpacked into a staging folder in the root
//!    and replaces the receipt entry `base` (`levitate_recipe::release`,
//!    `system::plan_base`). The manifest becomes the root's
//!    `usr/lib/levitate/release.json`, a file of `base`.
//! 3. Every package the root's receipt holds (but `base` and the effects
//!    package) that the release has a recipe for is installed from it; one the
//!    root already holds from the same recipe is skipped, one the release has
//!    no recipe for stays. The base is in place by now (step 2 wrote it, or
//!    the root had it already), so the release's recipes are the root's own
//!    `usr/lib/recipe/release-recipes/` (base files, the same recipes as
//!    `recipes.tar.zst`): what the receipt records, and what later commands
//!    use as the release source. Only a base without that folder has
//!    `recipes.tar.zst` unpacked into `/var/lib/recipe/release/SERIAL/`
//!    instead, for this apply, and removed at its end.
//! 4. The root's effects run once.
//!
//! A root without a `base` entry was installed from an image made before
//! releases existed: nothing says which of its files are the base's, so the
//! apply is refused and the release is installed from its ISO.
//!
//! On a running root (no A/B), the step being done is recorded first in
//! `/var/lib/recipe/apply-release.json` ([`release::Progress`]); running the
//! same command again finishes an interrupted apply: a base already recorded
//! is not applied again, base files already as the release has them count as
//! done, and groups already at the release's recipe are skipped.
use anyhow::{Context, Result};
use levitate_recipe::release;
use levitate_recipe::system::{self, BASE_PACKAGE};
use std::path::{Path, PathBuf};

use super::cli::Cli;
use super::system_route::{
    Roots, effects, finish, immutable, load_record, now, pending_json, producer_args, roots,
    save_record, settle, tell, timed, write_into,
};

/// Where the root is, and what kind of root it is.
struct Target {
    root: PathBuf,
    /// Whose `var/` is the system's `/var` (seeds go there).
    state_root: PathBuf,
    /// `other-root`, `running` or `target`.
    route: &'static str,
    place: String,
    ab: Option<Roots>,
}

fn booted_root() -> PathBuf {
    std::env::var_os("RECIPE_BOOTED_ROOT").map_or_else(|| "/".into(), PathBuf::from)
}

fn target(cli: &Cli) -> Result<Target> {
    if let Some(dir) = &cli.root {
        let root = std::fs::canonicalize(dir)
            .with_context(|| format!("the root {} does not exist", dir.display()))?;
        anyhow::ensure!(
            root.is_dir(),
            "the root {} is not a directory",
            root.display()
        );
        if immutable() {
            anyhow::ensure!(
                std::fs::canonicalize(booted_root()).ok().as_deref() != Some(root.as_path()),
                "{} is the booted root of this A/B system, which is locked; `recipe --system apply-release` without --root writes into the other root",
                root.display()
            );
        }
        return Ok(Target {
            state_root: root.clone(),
            place: format!("the target root {}", root.display()),
            root,
            route: "target",
            ab: None,
        });
    }
    if !immutable() {
        let root = std::fs::canonicalize(booted_root())?;
        return Ok(Target {
            state_root: root.clone(),
            root,
            route: "running",
            place: "this system's root".into(),
            ab: None,
        });
    }
    let roots = roots()?;
    Ok(Target {
        root: roots.other.clone(),
        state_root: roots.booted.clone(),
        route: "other-root",
        place: format!(
            "the other root ({}, {})",
            roots.other_name,
            roots.other.display()
        ),
        ab: Some(roots),
    })
}

/// The release of the running kernel (`uname -r`); `RECIPE_KERNEL_RELEASE`
/// names another.
fn running_kernel() -> Option<String> {
    if let Some(release) = std::env::var_os("RECIPE_KERNEL_RELEASE") {
        return release.into_string().ok();
    }
    let mut name: libc::utsname = unsafe { std::mem::zeroed() };
    if unsafe { libc::uname(&mut name) } != 0 {
        return None;
    }
    let release = unsafe { std::ffi::CStr::from_ptr(name.release.as_ptr()) };
    release.to_str().ok().map(str::to_owned)
}

/// A JSON line of `docs/release-engineering.md` section 10 on stdout, with
/// `--json` (or when stdout is not a terminal); the result object is the last
/// line.
fn event(cli: &Cli, value: serde_json::Value) {
    use std::io::{IsTerminal, Write};
    if cli.json || !std::io::stdout().is_terminal() {
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "{value}");
        let _ = out.flush();
    }
}

fn step_event(cli: &Cli, n: u32, text: &str) {
    event(
        cli,
        serde_json::json!({"event": "step", "n": n, "of": 4, "text": text}),
    );
}

fn progress_path() -> Result<PathBuf> {
    Ok(super::metadata::default_package_root()?.join("apply-release.json"))
}

/// What the base step did.
#[derive(Default)]
struct BaseDone {
    applied: bool,
    files: usize,
    removed: usize,
    adopted: usize,
    left_out: usize,
    left_to_others: Vec<(String, String)>,
    kept: Vec<String>,
    beside: Vec<String>,
    created: Vec<(String, system::Account)>,
    seeded: Vec<String>,
    /// The running kernel and how many of its module files were kept.
    kept_modules: Option<(String, usize)>,
}

impl BaseDone {
    fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "status": if self.applied { "applied" } else { "already" },
            "files": self.files,
            "removed": self.removed,
            "adopted": self.adopted,
            "left_out": self.left_out,
            "left_to_others": self.left_to_others.iter().map(|(path, package)| serde_json::json!({"path": path, "package": package})).collect::<Vec<_>>(),
            "kept_config": self.kept,
            "new_config_beside": self.beside.iter().map(|p| format!("{p}.recipe-new")).collect::<Vec<_>>(),
            "accounts_created": self.created.iter().map(|(p, a)| serde_json::json!({"package": p, "kind": a.kind, "name": a.name, "id": a.id, "group": a.group})).collect::<Vec<_>>(),
            "var_seeded": self.seeded,
            "previous_kernel": self.kept_modules.as_ref().map(|(release, files)| serde_json::json!({"release": release, "files": files, "package": system::PREVIOUS_KERNEL_PACKAGE})),
        })
    }
}

/// Replace the root's `base` with the release's base tree.
fn apply_base(folder: &Path, release: &release::Release, target: &Target) -> Result<BaseDone> {
    let dir = release::staging(&target.root)?;
    let result = (|| -> Result<BaseDone> {
        eprintln!("Unpacking the release's base...");
        let unpacked = timed("unpack base", || {
            release::unpack_base(folder, release, &target.root, &dir)
        })?;
        let mut payload = unpacked.payload;
        let old = system::read_receipt(&target.root)?;
        eprintln!("Comparing the base with what the root holds...");
        // On the running root the kernel that runs stays as the second boot
        // entry, so its modules stay with it.
        let kernel = (target.route == "running").then(running_kernel).flatten();
        let based = timed("plan base", || {
            system::plan_base(&target.root, &old, &mut payload, kernel.as_deref())
        })?;
        if based.kept_modules > 0 {
            eprintln!(
                "Kept the modules of the running kernel {} ({} paths) as {}: it stays as the second boot entry.",
                kernel.as_deref().unwrap_or_default(),
                based.kept_modules,
                system::PREVIOUS_KERNEL_PACKAGE
            );
        }
        let payloads = std::slice::from_ref(&payload);
        let seeded = system::seed(&target.state_root, payloads)?;
        eprintln!("Writing the base...");
        timed("apply base", || {
            system::apply(&target.root, payloads, &based.plan)
        })?;
        let created: Vec<(String, system::Account)> = payload
            .package
            .accounts
            .iter()
            .map(|a| (BASE_PACKAGE.to_owned(), a.clone()))
            .collect();
        tell(&based.plan, &created);
        for (path, package) in &based.left_to_others {
            eprintln!(
                "Left /{path} to {package}, which owns it; the release's version of it was not written."
            );
        }
        Ok(BaseDone {
            applied: true,
            files: payload.package.files.len(),
            removed: based.plan.removals.len(),
            adopted: based.adopted.len(),
            left_out: unpacked.left_out,
            left_to_others: based.left_to_others,
            kept: based.plan.kept.clone(),
            beside: based.plan.beside.iter().cloned().collect(),
            created,
            seeded,
            kept_modules: kernel
                .filter(|_| based.kept_modules > 0)
                .map(|k| (k, based.kept_modules)),
        })
    })();
    release::discard(&dir);
    result
}

pub(crate) fn apply_release(cli: &mut Cli, folder: &Path) -> Result<()> {
    let result = apply(cli, folder);
    if let Err(error) = &result {
        let line = format!("{error:#}");
        event(
            cli,
            serde_json::json!({"event": "failed", "error": line.lines().next().unwrap_or_default()}),
        );
    }
    result
}

fn apply(cli: &mut Cli, folder: &Path) -> Result<()> {
    let folder = std::fs::canonicalize(folder)
        .with_context(|| format!("the release folder {} does not exist", folder.display()))?;
    let release = release::read(&folder.join("release.json"))?;
    let m = release.manifest.clone();
    let target = target(cli)?;
    let root = target.root.clone();
    // Changes waiting for the other root that a copy wiped are written
    // first, so the release lands on top of them.
    let not_done = match &target.ab {
        Some(roots) => settle(cli, roots, None)?,
        None => Vec::new(),
    };
    for (package, why) in &not_done {
        eprintln!(
            "{package} was waiting to be written into the other root and writing it again failed: {why}"
        );
    }
    let host = levitate_recipe::clibrary::detect_root(&root);
    anyhow::ensure!(
        host.os_id.as_deref() == Some(m.os.as_str()),
        "this is a release of {}, and {} is {} ({})",
        m.os,
        target.place,
        host.os_id.as_deref().unwrap_or("of no known system"),
        host.os_found
    );
    let previous = release::installed(&root)?;
    if let Some(previous) = &previous {
        anyhow::ensure!(
            m.serial >= previous.serial,
            "{} runs {} {} (serial {}), which is newer than this release, {} (serial {}); releases are not applied backwards",
            target.place,
            previous.os,
            previous.version,
            previous.serial,
            m.version,
            m.serial
        );
    }
    let receipt = system::read_receipt(&root)?;
    let Some(base) = receipt.packages.get(BASE_PACKAGE) else {
        anyhow::bail!(
            "{} has no entry `{BASE_PACKAGE}` in /{}: it was installed from an image made before releases could be applied, so nothing records which of its files are the release's and which were added since. Updating it is not possible; install {} {} from its ISO instead (the data in /home and /var can be kept with a backup).",
            target.place,
            system::RECEIPT,
            m.os,
            m.version
        );
    };
    let base_current = base.recipe_digest == release.digest;
    let progress = progress_path()?;
    let mut record = release::Progress {
        schema: 1,
        root: root.clone(),
        folder: folder.clone(),
        os: m.os.clone(),
        version: m.version.clone(),
        serial: m.serial,
        step: "base".into(),
        started_at: now(),
    };
    if target.route == "running"
        && let Some(earlier) = release::Progress::load(&progress)?
    {
        if earlier.folder == folder {
            eprintln!(
                "Finishing the update to {} {} that was interrupted (step: {}).",
                earlier.os, earlier.version, earlier.step
            );
            record.started_at = earlier.started_at;
        } else {
            eprintln!(
                "An update to {} {} from {} was interrupted (step: {}) and is not finished. Applying {} {} instead: files the interrupted update wrote that are in neither release are reported as changed, and the apply stops there.",
                earlier.os,
                earlier.version,
                earlier.folder.display(),
                earlier.step,
                m.os,
                m.version
            );
        }
    }
    step_event(cli, 1, "Checking the release's archives");
    eprintln!("Checking the release's archives...");
    timed("verify release", || release::verify(&folder, &release))?;
    let running = target.route == "running";
    let step = |record: &mut release::Progress, step: &str| -> Result<()> {
        record.step = step.into();
        if running {
            record.save(&progress)?;
        }
        Ok(())
    };
    // 1. The base.
    step(&mut record, "base")?;
    step_event(cli, 2, "Replacing the base");
    let base = if base_current {
        eprintln!(
            "The base of {} is {} {} already.",
            target.place, m.os, m.version
        );
        BaseDone::default()
    } else {
        apply_base(&folder, &release, &target)?
    };
    // 2. The groups, from the release's recipes.
    step(&mut record, "groups")?;
    step_event(cli, 3, "Installing the release's groups");
    let unpacked = super::metadata::default_package_root()?
        .join("release")
        .join(m.serial.to_string());
    if std::fs::symlink_metadata(&unpacked).is_ok() {
        levitate_recipe::fsx::make_removable(&unpacked);
        std::fs::remove_dir_all(&unpacked)?;
    }
    let in_base = root.join(levitate_recipe::sources::RELEASE_RECIPES);
    let recipes = if std::fs::symlink_metadata(&in_base).is_ok_and(|m| m.is_dir()) {
        std::fs::canonicalize(&in_base)?
    } else {
        eprintln!(
            "The release's base has no /{}; using the recipes of {} for this apply.",
            levitate_recipe::sources::RELEASE_RECIPES,
            release::RECIPES_ARCHIVE
        );
        std::fs::create_dir_all(&unpacked)?;
        release::unpack_recipes(&folder, &unpacked)?;
        unpacked.clone()
    };
    let saved_path = cli.recipes_path.replace(recipes.clone());
    // Read once after the base: each write hands on the receipt it wrote.
    let mut receipt = system::read_receipt(&root)?;
    let context = if target.ab.is_some() {
        "other-root"
    } else {
        "root"
    };
    let mut written = Vec::new();
    let mut not_in_release = Vec::new();
    let mut failed: Vec<(String, String)> = Vec::new();
    let args = producer_args(cli)?;
    let mut seen = std::collections::BTreeSet::new();
    for name in release::groups(&receipt) {
        if seen.contains(&name) {
            continue;
        }
        let path = recipes.join(format!("{name}.rhai"));
        if !std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_file()) {
            not_in_release.push(name);
            continue;
        }
        eprintln!("Installing {name} from the release's recipes...");
        match write_into(
            cli,
            &root,
            &receipt,
            &target.state_root,
            &std::fs::canonicalize(&path)?,
            &args,
            context,
        ) {
            Ok(mut w) => {
                if let Some(wrote) = w.receipt.take() {
                    receipt = wrote;
                }
                seen.extend(w.expects.keys().cloned());
                seen.extend(w.present.iter().cloned());
                written.push(w);
            }
            Err(e) => {
                failed.push((name, format!("{e:#}")));
                // A group can fail after its receipt was written (its seeds,
                // discarding the producer's outputs, the flush that follows
                // the rename): the next group writes on the receipt on the
                // disk, not on the one from before.
                receipt = system::read_receipt(&root)?;
            }
        }
    }
    cli.recipes_path = saved_path;
    if recipes == unpacked {
        levitate_recipe::fsx::make_removable(&unpacked);
        let _ = std::fs::remove_dir_all(&unpacked);
        // The folder above it holds nothing else.
        if let Some(parent) = unpacked.parent() {
            let _ = std::fs::remove_dir(parent);
        }
    }
    for name in &not_in_release {
        eprintln!("{name} is not in the release's recipes; it stays as it is.");
    }
    // 3. The effects, once.
    step(&mut record, "effects")?;
    step_event(cli, 4, "Rebuilding the root's caches");
    let mut effects = effects(cli, &root, &receipt);
    // A change waiting in Recipe's A/B record for a package the release has
    // just replaced is done with: written again after a copy, it would bring
    // back the older recipe.
    if target.ab.is_some() {
        let after = effects.receipt.take().unwrap_or(receipt);
        let mut changes = load_record()?;
        let before = changes.changes.len();
        changes.changes.retain(|c| {
            c.action == "remove"
                || c.expects
                    .iter()
                    .all(|(n, d)| after.packages.get(n).is_none_or(|p| &p.recipe_digest == d))
        });
        if changes.changes.len() != before {
            save_record(&changes)?;
        }
    }
    let merged = super::system_route::Written::merge(written);
    let updated: Vec<String> = merged.expects.keys().cloned().collect();
    let done = failed.is_empty() && !effects.failed();
    if running && done {
        let _ = std::fs::remove_file(&progress);
    }
    let mut value = serde_json::json!({
        "action": "apply-release",
        "route": target.route,
        "root": root,
        "os": m.os,
        "version": m.version,
        "serial": m.serial,
        "previous": previous.as_ref().map(|p| serde_json::json!({"version": p.version, "serial": p.serial})),
        "base": base.json(),
        "groups": {
            "updated": updated,
            "present": merged.present,
            "not_in_release": not_in_release,
            "failed": failed.iter().map(|(p, e)| serde_json::json!({"package": p, "error": e})).collect::<Vec<_>>(),
            "kept_config": merged.kept,
            "new_config_beside": merged.beside.iter().map(|p| format!("{p}.recipe-new")).collect::<Vec<_>>(),
            "var_seeded": merged.seeded,
        },
        "effects": effects.json(),
        "receipt": root.join(system::RECEIPT),
        "freed_bytes": merged.freed,
        "restart_needed": running,
        "active_now": false,
    });
    if let Some(roots) = &target.ab {
        value["other_root"] = roots.other_name.clone().into();
        value["other_mount"] = serde_json::json!(roots.other);
        value["running_root"] = roots.booted_name.clone().into();
        value["not_done"] = pending_json(&not_done).into();
    }
    if running {
        value["progress_record"] = serde_json::json!(progress);
    }
    let mut text = format!(
        "Applied {} {} (serial {}) to {}.",
        m.os, m.version, m.serial, target.place
    );
    if base.applied {
        text.push_str(&format!(
            " Base: {} paths, {} removed.",
            base.files, base.removed
        ));
    }
    if !updated.is_empty() {
        text.push_str(&format!(
            " Installed from the release's recipes: {}.",
            updated.join(", ")
        ));
    }
    if !merged.present.is_empty() {
        text.push_str(&format!(
            " Already at the release's recipes: {}.",
            merged.present.join(", ")
        ));
    }
    if !not_in_release.is_empty() {
        text.push_str(&format!(
            " Not in the release, left as they are: {}.",
            not_in_release.join(", ")
        ));
    }
    match &target.ab {
        Some(roots) => text.push_str(&format!(
            " It becomes active when root {} is booted.",
            roots.other_name
        )),
        None if running => text.push_str(
            " Running programs keep their old files until they start again: restart the computer.",
        ),
        None => {}
    }
    finish(cli, value, text)?;
    if !failed.is_empty() {
        let list = failed
            .iter()
            .map(|(p, e)| format!("{p}: {e}"))
            .collect::<Vec<_>>()
            .join("\n");
        anyhow::bail!(
            "the release's base is written, and installing these from its recipes failed:\n{list}\nRunning the same command again finishes the update."
        );
    }
    effects.result()
}

pub(crate) fn record_base(cli: &Cli, manifest: &Path) -> Result<()> {
    let dir = cli.root.as_ref().context(
        "record-base writes into an image tree: recipe --system --root TREE record-base --release FILE",
    )?;
    let tree = std::fs::canonicalize(dir)
        .with_context(|| format!("the tree {} does not exist", dir.display()))?;
    if immutable() {
        anyhow::ensure!(
            std::fs::canonicalize(booted_root()).ok().as_deref() != Some(tree.as_path()),
            "{} is the booted root of this A/B system, which is locked",
            tree.display()
        );
    }
    let recorded = release::record_base(&tree, manifest)?;
    finish(
        cli,
        serde_json::json!({"action": "record-base", "root": tree, "package": BASE_PACKAGE,
            "version": recorded.version, "serial": recorded.serial, "files": recorded.files,
            "receipt": tree.join(system::RECEIPT), "release": tree.join(release::INSTALLED)}),
        format!(
            "Recorded the base of {}: {} paths, release {} (serial {}).",
            tree.display(),
            recorded.files,
            recorded.version,
            recorded.serial
        ),
    )
}
