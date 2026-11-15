//! System scope: software that belongs to the computer's root, not to one user.
//!
//! On an A/B system the booted root is locked and the other root is unlocked,
//! mounted read-write where `recab status --json` says (`other_mount`). A system
//! change is written there and nowhere else; it becomes active when that root
//! is booted. Recipe keeps a record of what it wrote (`other-root.json` in the
//! package root, on the shared `/var`) so that, when `recab copy` overwrites the
//! other root, only that system part is applied again.
//!
//! A system without `recab.immutable` on its kernel command line has one root,
//! the running one, and no recab: a system change is written into it with its
//! receipt (`usr/lib/recipe/system.json`), as into any root. `--root DIR` writes
//! into a root that is not running (an installer's target, an image tree) the
//! same way. Neither calls recab or keeps the A/B record.
use anyhow::{Context, Result};
use levitate_recipe::system::{self, Receipt};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use super::cli::{Cli, Commands};
use super::commands::{Lookup, require_sha256, resolve_recipe_path};
use levitate_recipe::sources::{self, Sources};

/// What Recipe wrote into the other root, so it can write it again if a copy
/// of the running system wipes it.
#[derive(Default, Serialize, Deserialize)]
pub(super) struct Record {
    pub(super) schema_version: u32,
    pub(super) changes: Vec<Change>,
}

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Change {
    /// The package the change is about.
    pub(super) package: String,
    /// `install`, `update` or `remove`.
    pub(super) action: String,
    /// The recipe that produced it (install and update).
    recipe: Option<PathBuf>,
    /// The options the producer ran with, to run it the same way again.
    producer_args: Vec<String>,
    /// Package name -> recipe digest that the other root's receipt holds once
    /// the change is in it (install and update; empty for remove).
    pub(super) expects: BTreeMap<String, String>,
    pub(super) written_at: u64,
}

pub(super) struct Roots {
    pub(super) booted_name: String,
    pub(super) other_name: String,
    pub(super) booted: PathBuf,
    pub(super) other: PathBuf,
}

#[derive(PartialEq)]
enum State {
    /// The booted root holds it: that root was booted into. Nothing to do.
    Booted,
    /// The other root holds it.
    InOther,
    /// Neither root holds it: it was in the other root and a copy wiped it.
    Wiped,
}

pub(super) fn immutable() -> bool {
    let file = std::env::var_os("RECIPE_KERNEL_CMDLINE").unwrap_or_else(|| "/proc/cmdline".into());
    std::fs::read_to_string(file)
        .unwrap_or_default()
        .split_whitespace()
        .any(|p| p == "recab.immutable")
}

fn record_path() -> Result<PathBuf> {
    Ok(super::metadata::default_package_root()?.join("other-root.json"))
}

pub(super) fn load_record() -> Result<Record> {
    let path = record_path()?;
    match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text)
            .with_context(|| format!("{} is not a usable record", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Record::default()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

pub(super) fn save_record(record: &Record) -> Result<()> {
    let path = record_path()?;
    std::fs::create_dir_all(path.parent().unwrap())?;
    let temp = path.with_extension("json.new");
    let mut out = std::fs::File::create(&temp)?;
    out.write_all(&serde_json::to_vec_pretty(&Record {
        schema_version: 1,
        changes: record.changes.clone(),
    })?)?;
    out.sync_all()?;
    std::fs::rename(temp, path)?;
    Ok(())
}

pub(super) fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The root a system change targets, found when it is first needed: DIR of
/// `--root`, the other root of an A/B system (the booted root when recab
/// cannot say where the other one is), or the running root. Its release
/// source is the one recipe uses.
pub(super) fn target_root(dir: Option<PathBuf>) -> impl Fn() -> Option<PathBuf> + 'static {
    move || {
        if let Some(dir) = &dir {
            return std::fs::canonicalize(dir).ok();
        }
        if immutable()
            && let Ok(roots) = roots()
        {
            return Some(roots.other);
        }
        Some(std::env::var_os("RECIPE_BOOTED_ROOT").map_or_else(|| "/".into(), PathBuf::from))
    }
}

/// Which root is booted and where the other one is mounted.
pub(super) fn roots() -> Result<Roots> {
    let bin = std::env::var_os("RECIPE_RECAB_BIN").unwrap_or_else(|| "recab".into());
    let out = Command::new(&bin)
        .args(["status", "--json"])
        .output()
        .with_context(|| {
            format!(
                "cannot run {} to find the other root",
                Path::new(&bin).display()
            )
        })?;
    anyhow::ensure!(
        out.status.success(),
        "recab status failed: {}",
        String::from_utf8_lossy(&out.stderr).trim()
    );
    let status: serde_json::Value =
        serde_json::from_slice(&out.stdout).context("recab status did not print JSON")?;
    let name = |key: &str| {
        status[key]
            .as_str()
            .map(str::to_owned)
            .with_context(|| format!("recab status names no {key} root"))
    };
    let (booted_name, other_name) = (name("booted")?, name("other")?);
    let Some(mount) = status["other_mount"].as_str() else {
        anyhow::bail!(
            "The other root ({other_name}) is not mounted, so there is nowhere to write system changes. `recab copy` recreates it."
        );
    };
    let other = PathBuf::from(mount);
    anyhow::ensure!(
        other.is_dir(),
        "The other root ({other_name}) is not mounted at {mount}, so there is nowhere to write system changes. `recab copy` recreates it."
    );
    Ok(Roots {
        booted_name,
        other_name,
        booted: std::env::var_os("RECIPE_BOOTED_ROOT").map_or_else(|| "/".into(), PathBuf::from),
        other,
    })
}

fn state(change: &Change, booted: &Receipt, other: &Receipt) -> State {
    if change.action == "remove" {
        return if !booted.packages.contains_key(&change.package) {
            State::Booted
        } else if !other.packages.contains_key(&change.package) {
            State::InOther
        } else {
            State::Wiped
        };
    }
    let holds = |r: &Receipt| {
        change
            .expects
            .iter()
            .all(|(n, d)| r.packages.get(n).is_some_and(|p| &p.recipe_digest == d))
    };
    if holds(booted) {
        State::Booted
    } else if holds(other) {
        State::InOther
    } else {
        State::Wiped
    }
}

/// Compare Recipe's record with both roots' receipts. A change the booted root
/// now holds is done and forgotten. A change neither root holds was wiped from
/// the other root: write that part again (only that part). Returns the changes
/// that are still not in the other root, with why.
pub(super) fn settle(
    cli: &Cli,
    roots: &Roots,
    skip: Option<&str>,
) -> Result<Vec<(String, String)>> {
    let mut record = load_record()?;
    if record.changes.is_empty() {
        return Ok(vec![]);
    }
    let booted = system::read_receipt(&roots.booted)?;
    let mut failed = Vec::new();
    let mut kept = Vec::new();
    for change in std::mem::take(&mut record.changes) {
        let other = system::read_receipt(&roots.other)?;
        if Some(change.package.as_str()) == skip {
            kept.push(change);
            continue;
        }
        match state(&change, &booted, &other) {
            State::Booted => {}
            State::InOther => kept.push(change),
            State::Wiped => {
                eprintln!(
                    "{} is no longer in the other root ({}): a copy of the running system replaced it. Applying that system part again.",
                    change.package, roots.other_name
                );
                let again = if change.action == "remove" {
                    remove_package(roots, &change.package, &change.producer_args)
                        .map(|_| change.clone())
                } else {
                    let recipe = change.recipe.clone().context("the record has no recipe")?;
                    install_package(cli, roots, &recipe, &change.producer_args, &change.action)
                        .map(|(done, _)| done)
                };
                match again {
                    Ok(done) => kept.push(done),
                    Err(e) => {
                        failed.push((change.package.clone(), format!("{e:#}")));
                        kept.push(change);
                    }
                }
            }
        }
    }
    record.changes = kept;
    save_record(&record)?;
    Ok(failed)
}

fn remember(change: Change) -> Result<()> {
    let mut record = load_record()?;
    record.changes.retain(|c| c.package != change.package);
    record.changes.push(change);
    save_record(&record)
}

/// Run the recipe as a producer: its outputs are made and published, nothing is
/// activated anywhere. The result lists what it produced.
fn produce(
    cli: &Cli,
    root: &Path,
    recipe: &Path,
    args: &[String],
    context: &str,
) -> Result<serde_json::Value> {
    produce_with(cli, root, recipe, args, context, true)
}

/// `produce`; with `skip_present`, a recipe the root holds at the same digest
/// and version is not produced again (the effects recipe always runs).
fn produce_with(
    cli: &Cli,
    root: &Path,
    recipe: &Path,
    args: &[String],
    context: &str,
    skip_present: bool,
) -> Result<serde_json::Value> {
    let (base, state, build) = producer_dirs(cli)?;
    std::fs::create_dir_all(&base)?;
    let result = base.join("result.json");
    let _ = std::fs::remove_file(&result);
    // What the root holds, so the producer skips a recipe (or dependency) it
    // already holds at the same recipe digest and version.
    let present = base.join("present.json");
    let mut held = system::read_receipt(root)?;
    if !skip_present {
        held.packages.clear();
    }
    std::fs::write(
        &present,
        serde_json::to_vec(
            &held
                .packages
                .values()
                .map(|p| {
                    serde_json::json!({"name": p.name, "version": p.version,
                        "recipe_digest": p.recipe_digest})
                })
                .collect::<Vec<_>>(),
        )?,
    )?;
    let status = Command::new(std::env::current_exe()?)
        .arg("install")
        .arg(recipe)
        .args(args)
        .args(["--no-activate", "--json", "--state-dir"])
        .arg(std::path::absolute(state)?)
        .arg("--build-dir")
        .arg(std::path::absolute(build)?)
        .arg("--json-output")
        .arg(&result)
        .arg("--define")
        .arg(format!("RECIPE_SYSTEM_CONTEXT={context}"))
        // The root written into decides what the recipe sees of "this
        // system" (RUNTIME.host.libc, the libc and systems checks), and a
        // refusal names the command the user gave.
        .env(levitate_recipe::clibrary::ROOT_ENV, root)
        // Read by the producer's graph (`backend::SYSTEM_PRESENT_ENV`).
        .env("RECIPE_SYSTEM_PRESENT", &present)
        // Its published outputs are written into the root and deleted here.
        .env("RECIPE_EPHEMERAL_OUTPUTS", "1")
        .env(
            super::commands::SYSTEM_PREFIX_ENV,
            match &cli.root {
                Some(dir) => format!("--system --root {}", dir.display()),
                None => "--system".into(),
            },
        )
        .stdout(std::process::Stdio::null())
        .status()
        .context("starting the recipe")?;
    anyhow::ensure!(
        status.success(),
        "{}",
        match context {
            "effects" => "the effects recipe failed (its error is above)",
            "other-root" => "the recipe failed; nothing was written to the other root",
            _ => "the recipe failed; nothing was written to the target root",
        }
    );
    let file = std::fs::File::open(&result)?;
    anyhow::ensure!(
        file.metadata()?.len() <= 8 * 1024 * 1024,
        "the recipe result exceeds 8 MiB"
    );
    Ok(serde_json::from_reader(file)?)
}

/// The producer's directory, state directory and build directory.
pub(super) fn producer_dirs(cli: &Cli) -> Result<(PathBuf, PathBuf, PathBuf)> {
    let base = super::metadata::default_package_root()?.join("system-producer");
    let state = cli.state_dir.clone().unwrap_or_else(|| base.join("state"));
    let build = cli.build_dir.clone().unwrap_or_else(|| base.join("build"));
    Ok((base, state, build))
}

/// Run `f`; with `RECIPE_TIMING` set, say how long it took.
pub(super) fn timed<T>(what: &str, f: impl FnOnce() -> T) -> T {
    let start = std::time::Instant::now();
    let result = f();
    if std::env::var_os("RECIPE_TIMING").is_some() {
        eprintln!(
            "recipe: timing: {what} took {:.1} s",
            start.elapsed().as_secs_f64()
        );
    }
    result
}

/// Delete the producer's published outputs once they are written into a root:
/// they would otherwise keep a second copy of everything installed. Only
/// publications in the producer's own `<state>/exports` are deleted; its saved
/// state (unless `--no-persist-state`) stays for reuse.
fn discard_outputs(cli: &Cli, produced: &serde_json::Value) -> Result<()> {
    let (_, state, _) = producer_dirs(cli)?;
    let Ok(exports) = std::path::absolute(state)?.join("exports").canonicalize() else {
        return Ok(());
    };
    let mut done = std::collections::BTreeSet::new();
    let producers = produced["system_producers"]
        .as_array()
        .into_iter()
        .flatten();
    for outputs in producers
        .map(|p| &p["outputs"])
        .chain(std::iter::once(&produced["outputs"]))
    {
        for path in outputs
            .as_object()
            .into_iter()
            .flat_map(|o| o.values())
            .filter_map(|v| v.as_str())
        {
            let Some(publication) = Path::new(path).parent() else {
                continue;
            };
            if publication.parent() != Some(exports.as_path()) || !done.insert(publication) {
                continue;
            }
            levitate_recipe::fsx::make_removable(publication);
            std::fs::remove_dir_all(publication).with_context(|| {
                format!("removing the producer output {}", publication.display())
            })?;
        }
    }
    Ok(())
}

/// Tell what a change left alone: configuration files the administrator
/// changed, and the accounts it created.
pub(super) fn tell(plan: &system::Plan, created: &[(String, system::Account)]) {
    for path in &plan.kept {
        if plan.beside.contains(path) {
            eprintln!(
                "Kept /{path}: it was changed since it was installed. The new version is /{path}.recipe-new."
            );
        } else if plan.untouched.contains(path) {
            eprintln!(
                "Kept /{path}: it was changed since it was installed; the new version brings nothing new for it."
            );
        } else {
            eprintln!(
                "Kept /{path}: it was changed since it was installed, so it was not removed."
            );
        }
    }
    for (package, account) in created {
        match &account.group {
            Some(group) => eprintln!(
                "Added user {} to group {group} ({}) for {package}.",
                account.name, account.id
            ),
            None => eprintln!(
                "Created {} {} ({}) for {package}.",
                account.kind, account.name, account.id
            ),
        }
    }
}

/// What writing a change into a root did.
pub(super) struct Written {
    /// The package the change is about.
    pub(super) name: String,
    /// Package name -> recipe digest, for every package written.
    pub(super) expects: BTreeMap<String, String>,
    pub(super) kept: Vec<String>,
    pub(super) beside: Vec<String>,
    pub(super) created: Vec<(String, system::Account)>,
    /// State seeds made under `/var`.
    pub(super) seeded: Vec<String>,
    /// Packages the root already held at the same recipe digest and version:
    /// not produced or written again.
    pub(super) present: Vec<String>,
}

impl Written {
    /// One record of several changes.
    pub(super) fn merge(all: Vec<Written>) -> Written {
        let mut merged = Written {
            name: all.first().map(|w| w.name.clone()).unwrap_or_default(),
            expects: BTreeMap::new(),
            kept: Vec::new(),
            beside: Vec::new(),
            created: Vec::new(),
            seeded: Vec::new(),
            present: Vec::new(),
        };
        for w in all {
            merged.expects.extend(w.expects);
            merged.kept.extend(w.kept);
            merged.beside.extend(w.beside);
            merged.created.extend(w.created);
            merged.seeded.extend(w.seeded);
            for p in w.present {
                if !merged.present.contains(&p) && !merged.expects.contains_key(&p) {
                    merged.present.push(p);
                }
            }
        }
        merged
    }

    pub(super) fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "kept_config": self.kept,
            "new_config_beside": self.beside.iter().map(|p| format!("{p}.recipe-new")).collect::<Vec<_>>(),
            "accounts_created": self.created.iter().map(|(p, a)| serde_json::json!({"package": p, "kind": a.kind, "name": a.name, "id": a.id, "group": a.group})).collect::<Vec<_>>(),
            "var_seeded": self.seeded,
            "present": self.present,
        })
    }
}

/// Produce `recipe` and write it, with the runtime dependencies it brings, into
/// the root at `root`.
pub(super) fn write_into(
    cli: &Cli,
    root: &Path,
    state_root: &Path,
    recipe: &Path,
    args: &[String],
    context: &str,
) -> Result<Written> {
    let produced = timed("produce", || produce(cli, root, recipe, args, context))?;
    let present: Vec<String> = produced["system_present"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|n| n["name"].as_str().map(str::to_owned))
        .collect();
    if produced["system_producers"]
        .as_array()
        .is_some_and(|p| p.is_empty())
    {
        // Everything it would write is in the root already.
        return Ok(Written {
            name: produced["name"]
                .as_str()
                .or(present.last().map(String::as_str))
                .unwrap_or_default()
                .to_owned(),
            expects: BTreeMap::new(),
            kept: Vec::new(),
            beside: Vec::new(),
            created: Vec::new(),
            seeded: Vec::new(),
            present,
        });
    }
    let mut payloads = system::payloads(&produced, context)?;
    for p in &payloads {
        anyhow::ensure!(
            p.package.architecture == std::env::consts::ARCH,
            "the recipe builds for {}, this system is {}",
            p.package.architecture,
            std::env::consts::ARCH
        );
    }
    let old = timed("read receipt", || system::read_receipt(root))?;
    let plan = timed("accounts, observe, reconcile", || {
        system::plan(root, &old, &mut payloads, None)
    })?;
    timed("apply", || system::apply(root, &payloads, &plan))?;
    let seeded = timed("seeds", || system::seed(state_root, &payloads))?;
    if !seeded.is_empty() {
        eprintln!(
            "Made {} missing state path(s) under {}/var (seeds; never overwritten or removed).",
            seeded.len(),
            state_root.display()
        );
    }
    let created = payloads
        .iter()
        .flat_map(|p| {
            p.package
                .accounts
                .iter()
                .map(|a| (p.package.name.clone(), a.clone()))
        })
        .collect::<Vec<_>>();
    tell(&plan, &created);
    timed("discard producer output", || {
        discard_outputs(cli, &produced)
    })?;
    let name = produced["name"]
        .as_str()
        .or_else(|| payloads.last().map(|p| p.package.name.as_str()))
        .unwrap()
        .to_owned();
    Ok(Written {
        name,
        expects: payloads
            .iter()
            .map(|p| (p.package.name.clone(), p.package.recipe_digest.clone()))
            .collect(),
        kept: plan.kept.clone(),
        beside: plan.beside.iter().cloned().collect(),
        created,
        seeded,
        present,
    })
}

/// Remove the package `name` from the root at `root`.
fn remove_from(root: &Path, name: &str) -> Result<Written> {
    let old = system::read_receipt(root)?;
    let name = old.find(Path::new(name))?.name.clone();
    anyhow::ensure!(
        name != EFFECTS_PACKAGE,
        "{EFFECTS_PACKAGE} holds the root's caches, made by /{EFFECTS_RECIPE} after each system change; it is not removed by itself"
    );
    anyhow::ensure!(name != BASE_PACKAGE, "{}", base_refusal("removed"));
    anyhow::ensure!(
        name != system::PREVIOUS_KERNEL_PACKAGE,
        "{} keeps the modules of the kernel this system ran before its last update, which stays as the second boot entry; the next update gives them back to the base and removes them",
        system::PREVIOUS_KERNEL_PACKAGE
    );
    let plan = system::plan(root, &old, &mut [], Some(&name))?;
    let had_session = has_session(root);
    system::apply(root, &[], &plan)?;
    tell(&plan, &[]);
    if had_session && !has_session(root) {
        eprintln!(
            "{name} was the last desktop in this root: it now starts to a text login. Install a desktop with Recipe to get a graphical session back."
        );
    }
    Ok(Written {
        name,
        expects: BTreeMap::new(),
        kept: plan.kept.clone(),
        beside: Vec::new(),
        created: Vec::new(),
        seeded: Vec::new(),
        present: Vec::new(),
    })
}

/// Whether `root` holds a desktop session a login screen can start.
fn has_session(root: &Path) -> bool {
    ["usr/share/wayland-sessions", "usr/share/xsessions"]
        .iter()
        .any(|dir| {
            std::fs::read_dir(root.join(dir)).is_ok_and(|entries| {
                entries
                    .flatten()
                    .any(|entry| entry.path().extension().is_some_and(|x| x == "desktop"))
            })
        })
}

/// Produce `recipe` and write it, with the runtime dependencies it brings, into
/// the other root. Returns the change to remember.
fn install_package(
    cli: &Cli,
    roots: &Roots,
    recipe: &Path,
    args: &[String],
    action: &str,
) -> Result<(Change, Written)> {
    // The shared /var is the running system's: seeds go there, once.
    let written = write_into(cli, &roots.other, &roots.booted, recipe, args, "other-root")?;
    if written.expects.is_empty() {
        eprintln!(
            "{} is in the other root ({}) already, from the same recipe; nothing was written.",
            written.name, roots.other_name
        );
    }
    let change = Change {
        package: written.name.clone(),
        action: action.into(),
        recipe: Some(recipe.to_path_buf()),
        producer_args: args.to_vec(),
        expects: written.expects.clone(),
        written_at: now(),
    };
    remember(change.clone())?;
    Ok((change, written))
}

fn remove_package(roots: &Roots, name: &str, args: &[String]) -> Result<(Change, Written)> {
    let written = remove_from(&roots.other, name)?;
    let change = Change {
        package: written.name.clone(),
        action: "remove".into(),
        recipe: None,
        producer_args: args.to_vec(),
        expects: BTreeMap::new(),
        written_at: now(),
    };
    remember(change.clone())?;
    Ok((change, written))
}

/// The options a producer run is given, from this invocation's.
pub(super) fn producer_args(cli: &Cli) -> Result<Vec<String>> {
    let mut args: Vec<String> = Vec::new();
    let mut push = |a: &str, v: Option<String>| {
        args.push(a.into());
        args.extend(v);
    };
    if let Some(path) = &cli.recipes_path {
        push(
            "--recipes-path",
            Some(std::path::absolute(path)?.to_string_lossy().into_owned()),
        );
    }
    push("--target-os", Some(cli.target_os.clone()));
    push("--target-arch", Some(cli.target_arch.clone()));
    push("--jobs", Some(cli.jobs.to_string()));
    if cli.execution != levitate_recipe::ExecutionMode::Auto {
        push("--execution", Some(cli.execution.as_str().into()));
    }
    for (flag, value) in [
        ("--timeout-secs", cli.timeout_secs),
        ("--memory-mib", cli.memory_mib),
        ("--storage-max-objects", cli.storage_max_objects),
        ("--storage-max-bytes", cli.storage_max_bytes),
        ("--network-max-bytes", cli.network_max_bytes),
    ] {
        if let Some(value) = value {
            push(flag, Some(value.to_string()));
        }
    }
    for (flag, on) in [
        ("--no-network", cli.no_network),
        ("--force-acquire", cli.force_acquire),
        ("--force-build", cli.force_build),
        ("--force-install", cli.force_install),
        ("--any-libc", cli.any_libc),
        ("--any-system", cli.any_system),
        ("--no-persist-state", cli.no_persist_state),
        ("--exact-identity", cli.exact_identity),
        ("--machine-events", cli.machine_events),
    ] {
        if on {
            push(flag, None);
        }
    }
    for host in &cli.network_hosts {
        push("--network-host", Some(host.clone()));
    }
    for define in &cli.defines {
        push("--define", Some(define.clone()));
    }
    for input in &cli.inputs {
        let (name, path) = input.split_once('=').context("input must be NAME=PATH")?;
        push(
            "--input",
            Some(format!("{name}={}", std::path::absolute(path)?.display())),
        );
    }
    if let Some(profile) = &cli.llm_profile {
        push("--llm-profile", Some(profile.clone()));
    }
    if let Some(path) = &cli.progress_file {
        push(
            "--progress-file",
            Some(std::path::absolute(path)?.to_string_lossy().into_owned()),
        );
    }
    if let Commands::Install {
        autofix: true,
        autofix_attempts,
        autofix_cwd,
        autofix_prompt_file,
        autofix_allow_path,
        ..
    } = &cli.command
    {
        push("--autofix", None);
        push("--autofix-attempts", Some(autofix_attempts.to_string()));
        for (flag, path) in [
            ("--autofix-cwd", autofix_cwd),
            ("--autofix-prompt-file", autofix_prompt_file),
        ] {
            if let Some(path) = path {
                push(
                    flag,
                    Some(std::path::absolute(path)?.to_string_lossy().into_owned()),
                );
            }
        }
        for path in autofix_allow_path {
            push(
                "--autofix-allow-path",
                Some(std::path::absolute(path)?.to_string_lossy().into_owned()),
            );
        }
    }
    Ok(args)
}

pub(super) fn finish(cli: &Cli, value: serde_json::Value, text: String) -> Result<()> {
    let json = cli.json || !std::io::stdout().is_terminal() || cli.json_output.is_some();
    if let Some(path) = &cli.json_output {
        std::fs::write(path, serde_json::to_string(&value)?)?;
        if !json {
            println!("{text}");
        }
    } else if json {
        writeln!(std::io::stdout(), "{}", serde_json::to_string(&value)?)?;
    } else {
        println!("{text}");
    }
    Ok(())
}

/// The receipt's packages for `--json`: all but the effects package. The
/// base is there with the serial of the release the root at `root` records.
///
/// `from_release` says a package was installed from a release's recipes;
/// `not_in_release` that the root's release source no longer carries it, so
/// it stays as it is until it is removed (no other source updates it).
fn summary(r: &Receipt, root: &Path) -> Vec<serde_json::Value> {
    let release = sources::Release::of(root);
    r.packages
        .values()
        .filter(|p| p.name != EFFECTS_PACKAGE && p.name != system::PREVIOUS_KERNEL_PACKAGE)
        .map(|p| {
            if p.name == BASE_PACKAGE {
                let serial = levitate_recipe::release::installed(root)
                    .ok()
                    .flatten()
                    .map(|i| i.serial);
                return serde_json::json!({"name": p.name, "version": p.version, "serial": serial,
                    "release": true, "recipe": p.recipe, "recipe_digest": p.recipe_digest,
                    "dependencies": p.dependencies});
            }
            // Desktop sessions this package provides: a front end warns
            // before the last package with one is removed.
            let sessions = p
                .files
                .keys()
                .filter(|path| {
                    (path.starts_with("usr/share/wayland-sessions/")
                        || path.starts_with("usr/share/xsessions/"))
                        && path.ends_with(".desktop")
                })
                .count();
            let from_release = sources::from_release(&p.recipe);
            let not_in_release = from_release
                && release
                    .as_ref()
                    .is_some_and(|release| release.carries(&p.name).is_none());
            serde_json::json!({"name": p.name, "version": p.version, "recipe": p.recipe,
            "recipe_digest": p.recipe_digest, "dependencies": p.dependencies,
            "sessions": sessions, "from_release": from_release,
            "not_in_release": not_in_release})
        })
        .collect()
}

/// Packages the human list leaves out: the effects caches, the base and the
/// kept modules of the previous kernel.
fn hidden(name: &str) -> bool {
    name == EFFECTS_PACKAGE || name == BASE_PACKAGE || name == system::PREVIOUS_KERNEL_PACKAGE
}

fn list(r: &Receipt) -> String {
    if r.packages.keys().all(|n| hidden(n)) {
        return "  (none)\n".into();
    }
    r.packages
        .values()
        .filter(|p| !hidden(&p.name))
        .map(|p| format!("  {} {}\n", p.name, p.version))
        .collect()
}

/// Software the running system holds that the other root's receipt lacks.
fn missing_in_other(booted: &Receipt, other: &Receipt) -> Vec<String> {
    booted
        .packages
        .keys()
        .filter(|n| !other.packages.contains_key(*n))
        .cloned()
        .collect()
}

/// Run a system-scope command. Returns true when it was handled here, false
/// when the ordinary route applies.
pub(crate) fn route(cli: &mut Cli) -> Result<bool> {
    if cli.system {
        match &cli.command {
            // The catalog as a system change on the target root sees it.
            Commands::List => return Ok(false),
            Commands::ApplyRelease { folder } => {
                let folder = folder.clone();
                super::release_route::apply_release(cli, &folder)?;
                return Ok(true);
            }
            Commands::RecordBase { release } => {
                let release = release.clone();
                super::release_route::record_base(cli, &release)?;
                return Ok(true);
            }
            Commands::Install { recipe, more, .. } => {
                for recipe in std::iter::once(recipe).chain(more) {
                    anyhow::ensure!(
                        stem(recipe) != BASE_PACKAGE,
                        "{}",
                        base_refusal("installed")
                    );
                }
            }
            Commands::Update { recipe } => {
                anyhow::ensure!(stem(recipe) != BASE_PACKAGE, "{}", base_refusal("updated"));
            }
            _ => {}
        }
    }
    let command = match &cli.command {
        Commands::Install { recipe, .. } => Some(Some(recipe.clone())),
        Commands::Update { recipe } | Commands::Remove { recipe } => Some(Some(recipe.clone())),
        Commands::Installed => Some(None),
        Commands::IsInstalled { recipe } => Some(Some(recipe.clone())),
        _ => None,
    };
    let ab = immutable();
    let wanted = cli.system
        || (!cli.no_activate
            && command.is_some()
            && cli.package_root.is_none()
            && unsafe { libc::geteuid() } == 0
            && ab);
    if !wanted {
        return Ok(false);
    }
    let recipe =
        command.context("--system supports install, update, remove, installed and isinstalled")?;
    if let Some(dir) = cli.root.clone() {
        // A root that is not running (an installer's target, an image tree).
        root_route(cli, &dir, "target", recipe)?;
        return Ok(true);
    }
    if !ab {
        // One root, the running one, and no recab: the change is written into
        // it with its receipt, like any root.
        let booted =
            std::env::var_os("RECIPE_BOOTED_ROOT").map_or_else(|| "/".into(), PathBuf::from);
        root_route(cli, &booted, "running", recipe)?;
        return Ok(true);
    }
    let roots = roots()?;
    let failed = settle(cli, &roots, skipped(&cli.command).as_deref())?;
    let booted = system::read_receipt(&roots.booted)?;
    match &cli.command {
        Commands::Installed => report(cli, &roots, &booted, &failed)?,
        Commands::IsInstalled { .. } => {
            is_installed(cli, &roots, &booted, &recipe.unwrap(), &failed)?
        }
        _ => {
            if let Some((package, why)) = failed.first() {
                anyhow::bail!(
                    "{package} was in the other root and is gone, and writing it again failed: {why}\nThe system change you asked for was not made."
                );
            }
            change(cli, &roots, &booted, &recipe.unwrap())?
        }
    }
    Ok(true)
}

/// A removal does not first re-apply the package it removes.
fn skipped(command: &Commands) -> Option<String> {
    match command {
        Commands::Remove { recipe } => Some(stem(recipe)),
        _ => None,
    }
}

fn stem(recipe: &Path) -> String {
    let text = recipe
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    match text.split_once(':') {
        Some((source, name)) if levitate_recipe::catalog::is_source_name(source) => name.to_owned(),
        _ => text,
    }
}

fn change(cli: &Cli, roots: &Roots, booted: &Receipt, recipe: &Path) -> Result<()> {
    let other = system::read_receipt(&roots.other)?;
    let lacking = missing_in_other(booted, &other);
    if !lacking.is_empty() {
        eprintln!(
            "The running system has {}, which the other root ({}) does not; `recab copy` makes the other root a copy of the running system.",
            lacking.join(", "),
            roots.other_name
        );
    }
    let action = match &cli.command {
        Commands::Install { .. } => "install",
        Commands::Update { .. } => "update",
        _ => "remove",
    };
    if action == "update"
        && let Some((name, why)) = not_in_release(cli, recipe, &other, &roots.other)
    {
        return finish(
            cli,
            serde_json::json!({"action": "update", "package": name, "changed": false,
                "not_in_release": true, "reason": why, "other_root": roots.other_name,
                "other_mount": roots.other, "running_root": roots.booted_name,
                "effects": Effects::Unchanged.json(), "present": [], "kept_config": [],
                "new_config_beside": [], "accounts_created": [], "var_seeded": []}),
            why,
        );
    }
    let args = producer_args(cli)?;
    let mut all_names = std::collections::BTreeSet::new();
    let mut all_written = Vec::new();
    let done = if action == "remove" {
        if other.find(recipe).is_err() && other.find(Path::new(&stem(recipe))).is_err() {
            // Recorded but never in place: forget it.
            let mut record = load_record()?;
            let before = record.changes.len();
            record.changes.retain(|c| c.package != stem(recipe));
            if record.changes.len() != before {
                save_record(&record)?;
                return finish(
                    cli,
                    serde_json::json!({"action":"remove","package":stem(recipe),"other_root":roots.other_name,"changed":false}),
                    format!(
                        "{} was not in the other root; it is no longer waiting to be written.",
                        stem(recipe)
                    ),
                );
            }
        }
        let name = other
            .find(recipe)
            .or_else(|_| other.find(Path::new(&stem(recipe))))
            .map(|p| p.name.clone())
            .unwrap_or_else(|_| stem(recipe));
        let (done, written) = remove_package(roots, &name, &args)?;
        all_written.push(written);
        done
    } else {
        let mut first = None;
        for recipe in std::iter::once(recipe.to_path_buf()).chain(more_recipes(cli)) {
            let other = system::read_receipt(&roots.other)?;
            let path = resolve_for(cli, &recipe, action, &other, "the other root", &roots.other)?;
            let (change, written) = install_package(cli, roots, &path, &args, action)?;
            all_names.extend(change.expects.keys().cloned());
            all_written.push(written);
            first.get_or_insert(change);
        }
        first.unwrap()
    };
    if action == "remove" {
        all_names.extend(done.expects.keys().cloned());
    }
    let effects = if action == "remove" || !all_names.is_empty() {
        effects(cli, &roots.other)
    } else {
        Effects::Unchanged
    };
    let after = system::read_receipt(&roots.other)?;
    let mut value = serde_json::json!({"action": action, "package": done.package, "packages": all_names,
        "other_root": roots.other_name, "other_mount": roots.other,
        "running_root": roots.booted_name, "active_now": false,
        "in_other_root": after.packages.contains_key(&done.package)});
    // The same report of what the change left alone as a change to one root:
    // present, kept_config, new_config_beside, accounts_created, var_seeded.
    if let (Some(map), serde_json::Value::Object(more)) =
        (value.as_object_mut(), Written::merge(all_written).json())
    {
        map.extend(more);
    }
    value["effects"] = effects.json();
    finish(
        cli,
        value,
        if action == "remove" {
            format!(
                "Removed {} from the other root ({}, {}). It stays in the running system until root {} is booted.",
                done.package,
                roots.other_name,
                roots.other.display(),
                roots.other_name
            )
        } else {
            format!(
                "Installed {} into the other root ({}, {}), not into the running system. It becomes active when root {} is booted.",
                done.package,
                roots.other_name,
                roots.other.display(),
                roots.other_name
            )
        },
    )?;
    effects.result()
}

/// Whether the collection is explicit (`--recipes-path`, `$RECIPE_PATH`):
/// it stays isolated, with no sources and no release source.
fn explicit(cli: &Cli) -> bool {
    cli.recipes_path.is_some() || std::env::var_os("RECIPE_PATH").is_some()
}

/// For an update by name of a package the root holds from its release's
/// recipes that the root's release source no longer carries: the package and
/// why it is not updated. Such a package changes only with a system release,
/// so no other source updates it. None for anything else, and with an
/// explicit collection or a recipe given as a file.
fn not_in_release(
    cli: &Cli,
    recipe: &Path,
    installed: &Receipt,
    root: &Path,
) -> Option<(String, String)> {
    if explicit(cli) || recipe.is_file() || recipe.components().count() != 1 {
        return None;
    }
    let release = sources::Release::of(root)?;
    let package = installed
        .find(recipe)
        .or_else(|_| installed.find(Path::new(&stem(recipe))))
        .ok()?;
    if !sources::from_release(&package.recipe) || release.carries(&package.name).is_some() {
        return None;
    }
    Some((
        package.name.clone(),
        format!(
            "{} {} came with an earlier system release, and this root's release, {}, no longer carries it; it changes only with a system release, so it is not updated from any other source and stays as it is (`recipe --system remove {}` removes it).",
            package.name, package.version, release.title, package.name
        ),
    ))
}

/// The recipe an install or update runs: the catalog's, or for an update with
/// no catalog entry, the recipe the package was installed from (`installed`).
/// A name the release source of `root` carries is its recipe (see `Lookup`).
fn resolve_for(
    cli: &Cli,
    recipe: &Path,
    action: &str,
    installed: &Receipt,
    place: &str,
    root: &Path,
) -> Result<PathBuf> {
    let paths = super::metadata::recipe_paths(cli.recipes_path.clone());
    let root = root.to_path_buf();
    let sources = Sources::new(&super::metadata::default_package_root()?)
        .with_release(move || Some(root.clone()))
        .for_system_change();
    let explicit = explicit(cli);
    let lookup = Lookup {
        folders: &paths,
        sources: (!explicit).then_some(&sources),
        purpose: sources::Purpose::Install,
        system: true,
    };
    Ok(match resolve_recipe_path(recipe, &lookup) {
        Ok(path) => {
            if let Commands::Install { sha256, .. } = &cli.command {
                require_sha256(&path, sha256.as_deref())?;
            }
            std::fs::canonicalize(path)?
        }
        Err(e) if action == "install" || explicit => return Err(e),
        // An update with no catalog entry uses the recipe the package came from.
        Err(_) => installed
            .find(recipe)
            .or_else(|_| installed.find(Path::new(&stem(recipe))))
            .with_context(|| format!("{} is not installed in {place}", recipe.display()))?
            .recipe
            .clone(),
    })
}

/// System scope on one root that is written directly: a target given with
/// `--root` (`kind` "target"), or the running root of a system without A/B
/// (`kind` "running"). The change goes into that root with its receipt; recab
/// is not called and no A/B record is kept.
fn root_route(cli: &Cli, dir: &Path, kind: &str, recipe: Option<PathBuf>) -> Result<()> {
    let root = std::fs::canonicalize(dir)
        .with_context(|| format!("the root {} does not exist", dir.display()))?;
    anyhow::ensure!(
        root.is_dir(),
        "the root {} is not a directory",
        root.display()
    );
    if kind == "target" && immutable() {
        let booted =
            std::env::var_os("RECIPE_BOOTED_ROOT").map_or_else(|| "/".into(), PathBuf::from);
        anyhow::ensure!(
            std::fs::canonicalize(&booted).ok().as_deref() != Some(root.as_path()),
            "{} is the booted root of this A/B system, which is locked; `recipe --system` without --root writes into the other root",
            root.display()
        );
    }
    let place = if kind == "target" {
        format!("the target root {}", root.display())
    } else {
        "this system's root".to_owned()
    };
    let receipt = system::read_receipt(&root)?;
    let action = match &cli.command {
        Commands::Installed => {
            let mut text = format!("System software in {place}:\n{}", list(&receipt));
            if text.ends_with('\n') {
                text.pop();
            }
            return finish(
                cli,
                serde_json::json!({"root": root, "packages": summary(&receipt, &root)}),
                text,
            );
        }
        Commands::IsInstalled { .. } => {
            let recipe = recipe.unwrap();
            let found = receipt
                .find(&recipe)
                .or_else(|_| receipt.find(Path::new(&stem(&recipe))))
                .ok()
                .map(|p| p.version.clone());
            let name = stem(&recipe);
            let text = match &found {
                Some(v) => format!("{name} {v} is installed in {place}."),
                None => format!("{name} is not installed in {place}."),
            };
            finish(
                cli,
                serde_json::json!({"package": name, "root": root, "installed": found.is_some(), "version": found}),
                text.clone(),
            )?;
            anyhow::ensure!(found.is_some(), "{text}");
            return Ok(());
        }
        Commands::Install { .. } => "install",
        Commands::Update { .. } => "update",
        _ => "remove",
    };
    let recipe = recipe.unwrap();
    if action == "update"
        && let Some((name, why)) = not_in_release(cli, &recipe, &receipt, &root)
    {
        return finish(
            cli,
            serde_json::json!({"action": "update", "package": name, "changed": false,
                "not_in_release": true, "reason": why, "packages": [], "root": root,
                "receipt": root.join(system::RECEIPT), "effects": Effects::Unchanged.json(), "present": [], "kept_config": [],
                "new_config_beside": [], "accounts_created": [], "var_seeded": []}),
            why,
        );
    }
    let mut all = Vec::new();
    for recipe in std::iter::once(recipe).chain(more_recipes(cli)) {
        let receipt = system::read_receipt(&root)?;
        all.push(if action == "remove" {
            let name = receipt
                .find(&recipe)
                .or_else(|_| receipt.find(Path::new(&stem(&recipe))))
                .map(|p| p.name.clone())
                .with_context(|| format!("{} is not installed in {place}", stem(&recipe)))?;
            remove_from(&root, &name)?
        } else {
            let path = resolve_for(cli, &recipe, action, &receipt, &place, &root)?;
            write_into(cli, &root, &root, &path, &producer_args(cli)?, "root")?
        });
    }
    let changed = action == "remove" || all.iter().any(|w| !w.expects.is_empty());
    let effects = if changed {
        effects(cli, &root)
    } else {
        Effects::Unchanged
    };
    let first = all[0].name.clone();
    let mut value = serde_json::json!({"action": action, "package": first,
        "packages": all.iter().flat_map(|w| w.expects.keys()).collect::<Vec<_>>(),
        "root": root, "receipt": root.join(system::RECEIPT)});
    let merged = Written::merge(all);
    if let (Some(map), serde_json::Value::Object(more)) = (value.as_object_mut(), merged.json()) {
        map.extend(more);
    }
    value["effects"] = effects.json();
    let verb = match action {
        "install" => "Installed",
        "update" => "Updated",
        _ => "Removed",
    };
    let preposition = if action == "remove" { "from" } else { "in" };
    let names = merged.expects.keys().cloned().collect::<Vec<_>>();
    let mut text = if action == "remove" {
        format!("{verb} {first} {preposition} {place}.")
    } else if names.is_empty() {
        format!(
            "Nothing to write: {place} already holds {}.",
            merged.present.join(", ")
        )
    } else {
        format!("{verb} {} {preposition} {place}.", names.join(", "))
    };
    if action != "remove" && !names.is_empty() && !merged.present.is_empty() {
        text.push_str(&format!(" Already there: {}.", merged.present.join(", ")));
    }
    finish(cli, value, text)?;
    effects.result()
}

/// More recipes named on an install command line.
fn more_recipes(cli: &Cli) -> Vec<PathBuf> {
    match &cli.command {
        Commands::Install { more, .. } => more.clone(),
        _ => Vec::new(),
    }
}

/// Where a root keeps the recipe that rebuilds its whole-root caches.
const EFFECTS_RECIPE: &str = "usr/lib/recipe/effects.rhai";
/// The reserved package its output is recorded as.
const EFFECTS_PACKAGE: &str = system::EFFECTS_PACKAGE;
/// The reserved package that is the root's base tree.
const BASE_PACKAGE: &str = system::BASE_PACKAGE;

/// Why the base is not installed, updated or removed like a package.
fn base_refusal(verb: &str) -> String {
    format!(
        "{BASE_PACKAGE} is the root's base tree, the files of the system release it runs; it is not {verb} like a package. A newer release replaces it: `recab update` on an A/B system, `recupdate apply` on one without A/B (both end in `recipe --system apply-release FOLDER`)."
    )
}

/// What running the root's effects recipe did.
pub(super) enum Effects {
    /// Nothing changed, so nothing ran.
    Unchanged,
    /// `--no-effects`.
    Skipped,
    /// The root has no effects recipe.
    None,
    /// It ran; the number of files `system-effects` holds now.
    Done(usize),
    /// It failed; the change itself stays written.
    Failed(String),
}

impl Effects {
    pub(super) fn json(&self) -> serde_json::Value {
        match self {
            Effects::Unchanged => serde_json::json!({"status": "unchanged"}),
            Effects::Skipped => serde_json::json!({"status": "skipped"}),
            Effects::None => serde_json::json!({"status": "none", "recipe": EFFECTS_RECIPE}),
            Effects::Done(files) => serde_json::json!({"status": "done", "recipe": EFFECTS_RECIPE,
                "package": EFFECTS_PACKAGE, "files": files}),
            Effects::Failed(error) => {
                serde_json::json!({"status": "failed", "recipe": EFFECTS_RECIPE,
                "error": error})
            }
        }
    }

    pub(super) fn result(self) -> Result<()> {
        match self {
            Effects::Failed(error) => anyhow::bail!(
                "the system change is written, but rebuilding the root's caches with /{EFFECTS_RECIPE} failed: {error}\nThe next system change runs it again."
            ),
            _ => Ok(()),
        }
    }
}

/// After a system change to `root`: run the root's effects recipe
/// (`usr/lib/recipe/effects.rhai`), if it has one, with the root as its
/// read-only input `root`, and write its `system` output into the root as the
/// reserved package `system-effects`, replacing that package's previous files.
pub(super) fn effects(cli: &Cli, root: &Path) -> Effects {
    if cli.no_effects {
        return Effects::Skipped;
    }
    let recipe = root.join(EFFECTS_RECIPE);
    if !std::fs::symlink_metadata(&recipe).is_ok_and(|m| m.is_file()) {
        return Effects::None;
    }
    eprintln!("Rebuilding the root's caches with /{EFFECTS_RECIPE}...");
    let run = || -> Result<usize> {
        let mut args = producer_args(cli)?;
        args.push("--input".into());
        args.push(format!("root={}", root.display()));
        let produced = produce_with(cli, root, &recipe, &args, "effects", false)?;
        let mut payloads = system::payloads(&produced, "effects")?;
        anyhow::ensure!(
            payloads.len() == 1,
            "the effects recipe has runtime dependencies; it must produce one system output"
        );
        payloads[0].package.name = EFFECTS_PACKAGE.into();
        payloads[0].package.dependencies.clear();
        let old = system::read_receipt(root)?;
        let plan = system::plan(root, &old, &mut payloads, None)?;
        system::apply(root, &payloads, &plan)?;
        discard_outputs(cli, &produced)?;
        Ok(plan
            .receipt
            .packages
            .get(EFFECTS_PACKAGE)
            .map_or(0, |p| p.files.len()))
    };
    match run() {
        Ok(files) => Effects::Done(files),
        Err(error) => Effects::Failed(format!("{error:#}")),
    }
}

pub(super) fn pending_json(failed: &[(String, String)]) -> Vec<serde_json::Value> {
    failed
        .iter()
        .map(|(p, why)| serde_json::json!({"package": p, "error": why}))
        .collect()
}

fn report(cli: &Cli, roots: &Roots, booted: &Receipt, failed: &[(String, String)]) -> Result<()> {
    let other = system::read_receipt(&roots.other)?;
    let mut text = format!(
        "System software in the running root ({}, locked):\n{}\nSystem software in the other root ({}, unlocked; it becomes active when root {} is booted):\n{}",
        roots.booted_name,
        list(booted),
        roots.other_name,
        roots.other_name,
        list(&other)
    );
    for (package, why) in failed {
        text.push_str(&format!(
            "\nNot done yet: {package} was in the other root and is gone; writing it again failed: {why}\n"
        ));
    }
    finish(
        cli,
        serde_json::json!({
            "booted": {"root": roots.booted_name, "packages": summary(booted, &roots.booted)},
            "other": {"root": roots.other_name, "mount": roots.other, "packages": summary(&other, &roots.other)},
            "not_done": pending_json(failed),
        }),
        text,
    )
}

fn is_installed(
    cli: &Cli,
    roots: &Roots,
    booted: &Receipt,
    recipe: &Path,
    failed: &[(String, String)],
) -> Result<()> {
    let other = system::read_receipt(&roots.other)?;
    let find = |r: &Receipt| {
        r.find(recipe)
            .or_else(|_| r.find(Path::new(&stem(recipe))))
            .ok()
            .map(|p| p.version.clone())
    };
    let (in_booted, in_other) = (find(booted), find(&other));
    let name = stem(recipe);
    let not_done = failed.iter().any(|(p, _)| *p == name);
    let text = match (&in_booted, &in_other) {
        (Some(v), Some(_)) => format!(
            "{name} {v} is in the running root ({}) and in the other root ({}).",
            roots.booted_name, roots.other_name
        ),
        (Some(v), None) => format!(
            "{name} {v} is in the running root ({}), not in the other root ({}).",
            roots.booted_name, roots.other_name
        ),
        (None, Some(v)) => format!(
            "{name} {v} is in the other root ({}) only; it becomes active when root {} is booted. It is not in the running root ({}).",
            roots.other_name, roots.other_name, roots.booted_name
        ),
        (None, None) if not_done => format!(
            "{name} is not in either root: it was in the other root ({}) and is gone, and writing it again has not succeeded.",
            roots.other_name
        ),
        (None, None) => format!("{name} is not in the running root or the other root."),
    };
    finish(
        cli,
        serde_json::json!({"package": name,
            "booted": {"root": roots.booted_name, "installed": in_booted.is_some(), "version": in_booted},
            "other": {"root": roots.other_name, "installed": in_other.is_some(), "version": in_other},
            "not_done": not_done}),
        text.clone(),
    )?;
    anyhow::ensure!(in_booted.is_some() || in_other.is_some(), "{text}");
    Ok(())
}
