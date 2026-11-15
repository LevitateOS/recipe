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
//! the running one, and no recab: system scope is then the ordinary root route.
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
struct Record {
    schema_version: u32,
    changes: Vec<Change>,
}

#[derive(Clone, Serialize, Deserialize)]
struct Change {
    /// The package the change is about.
    package: String,
    /// `install`, `update` or `remove`.
    action: String,
    /// The recipe that produced it (install and update).
    recipe: Option<PathBuf>,
    /// The options the producer ran with, to run it the same way again.
    producer_args: Vec<String>,
    /// Package name -> recipe digest that the other root's receipt holds once
    /// the change is in it (install and update; empty for remove).
    expects: BTreeMap<String, String>,
    written_at: u64,
}

struct Roots {
    booted_name: String,
    other_name: String,
    booted: PathBuf,
    other: PathBuf,
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

fn immutable() -> bool {
    let file = std::env::var_os("RECIPE_KERNEL_CMDLINE").unwrap_or_else(|| "/proc/cmdline".into());
    std::fs::read_to_string(file)
        .unwrap_or_default()
        .split_whitespace()
        .any(|p| p == "recab.immutable")
}

fn record_path() -> Result<PathBuf> {
    Ok(super::metadata::default_package_root()?.join("other-root.json"))
}

fn load_record() -> Result<Record> {
    let path = record_path()?;
    match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text)
            .with_context(|| format!("{} is not a usable record", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Record::default()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

fn save_record(record: &Record) -> Result<()> {
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

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Which root is booted and where the other one is mounted.
fn roots() -> Result<Roots> {
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
fn settle(cli: &Cli, roots: &Roots, skip: Option<&str>) -> Result<Vec<(String, String)>> {
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
fn produce(cli: &Cli, recipe: &Path, args: &[String]) -> Result<serde_json::Value> {
    let base = super::metadata::default_package_root()?.join("system-producer");
    let state = cli.state_dir.clone().unwrap_or_else(|| base.join("state"));
    let build = cli.build_dir.clone().unwrap_or_else(|| base.join("build"));
    std::fs::create_dir_all(&base)?;
    let result = base.join("result.json");
    let _ = std::fs::remove_file(&result);
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
        .arg("RECIPE_SYSTEM_CONTEXT=other-root")
        .stdout(std::process::Stdio::null())
        .status()
        .context("starting the recipe")?;
    anyhow::ensure!(
        status.success(),
        "the recipe failed; nothing was written to the other root"
    );
    let file = std::fs::File::open(&result)?;
    anyhow::ensure!(
        file.metadata()?.len() <= 8 * 1024 * 1024,
        "the recipe result exceeds 8 MiB"
    );
    Ok(serde_json::from_reader(file)?)
}

/// Produce `recipe` and write it, with the runtime dependencies it brings, into
/// the other root. Returns the change to remember.
fn install_package(
    cli: &Cli,
    roots: &Roots,
    recipe: &Path,
    args: &[String],
    action: &str,
) -> Result<Change> {
    let produced = produce(cli, recipe, args)?;
    let payloads = system::payloads(&produced, "other-root")?;
    for p in &payloads {
        anyhow::ensure!(
            p.package.architecture == std::env::consts::ARCH,
            "the recipe builds for {}, this system is {}",
            p.package.architecture,
            std::env::consts::ARCH
        );
    }
    let old = system::read_receipt(&roots.other)?;
    let observed = system::observe(&roots.other, &old, &old.affected(&payloads, None))?;
    let plan = system::reconcile(&old, &payloads, None, &observed)?;
    system::apply(&roots.other, &payloads, &plan)?;
    let name = produced["name"]
        .as_str()
        .or_else(|| payloads.last().map(|p| p.package.name.as_str()))
        .unwrap()
        .to_owned();
    let change = Change {
        package: name,
        action: action.into(),
        recipe: Some(recipe.to_path_buf()),
        producer_args: args.to_vec(),
        expects: payloads
            .iter()
            .map(|p| (p.package.name.clone(), p.package.recipe_digest.clone()))
            .collect(),
        written_at: now(),
    };
    remember(change.clone())?;
    Ok(change)
}

fn remove_package(roots: &Roots, name: &str, args: &[String]) -> Result<Change> {
    let old = system::read_receipt(&roots.other)?;
    let name = old.find(Path::new(name))?.name.clone();
    let observed = system::observe(&roots.other, &old, &old.affected(&[], Some(&name)))?;
    let plan = system::reconcile(&old, &[], Some(&name), &observed)?;
    system::apply(&roots.other, &[], &plan)?;
    let change = Change {
        package: name,
        action: "remove".into(),
        recipe: None,
        producer_args: args.to_vec(),
        expects: BTreeMap::new(),
        written_at: now(),
    };
    remember(change.clone())?;
    Ok(change)
}

/// The options a producer run is given, from this invocation's.
fn producer_args(cli: &Cli) -> Result<Vec<String>> {
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

fn finish(cli: &Cli, value: serde_json::Value, text: String) -> Result<()> {
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

fn summary(r: &Receipt) -> Vec<serde_json::Value> {
    r.packages
        .values()
        .map(|p| {
            serde_json::json!({"name": p.name, "version": p.version, "recipe": p.recipe,
            "recipe_digest": p.recipe_digest, "dependencies": p.dependencies})
        })
        .collect()
}

fn list(r: &Receipt) -> String {
    if r.packages.is_empty() {
        return "  (none)\n".into();
    }
    r.packages
        .values()
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
    if !ab {
        // One root, the running one: system software lives in its own package
        // root, and recab is not involved.
        cli.package_root = Some("/var/lib/recipe".into());
        cli.system = false;
        return Ok(false);
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
    let args = producer_args(cli)?;
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
        remove_package(roots, &name, &args)?
    } else {
        let paths = super::metadata::recipe_paths(cli.recipes_path.clone());
        let sources = Sources::new(&super::metadata::default_package_root()?);
        let explicit = cli.recipes_path.is_some() || std::env::var_os("RECIPE_PATH").is_some();
        let lookup = Lookup {
            folders: &paths,
            sources: (!explicit).then_some(&sources),
            purpose: sources::Purpose::Install,
        };
        let path = match resolve_recipe_path(recipe, &lookup) {
            Ok(path) => {
                if let Commands::Install { sha256, .. } = &cli.command {
                    require_sha256(&path, sha256.as_deref())?;
                }
                std::fs::canonicalize(path)?
            }
            Err(e) if action == "install" || explicit => return Err(e),
            // An update with no catalog entry uses the recipe the package came from.
            Err(_) => other
                .find(recipe)
                .or_else(|_| other.find(Path::new(&stem(recipe))))
                .with_context(|| {
                    format!("{} is not installed in the other root", recipe.display())
                })?
                .recipe
                .clone(),
        };
        install_package(cli, roots, &path, &args, action)?
    };
    let after = system::read_receipt(&roots.other)?;
    let names: Vec<_> = done.expects.keys().cloned().collect();
    finish(
        cli,
        serde_json::json!({"action": action, "package": done.package, "packages": names,
            "other_root": roots.other_name, "other_mount": roots.other,
            "running_root": roots.booted_name, "active_now": false,
            "in_other_root": after.packages.contains_key(&done.package)}),
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
    )
}

fn pending_json(failed: &[(String, String)]) -> Vec<serde_json::Value> {
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
            "booted": {"root": roots.booted_name, "packages": summary(booted)},
            "other": {"root": roots.other_name, "mount": roots.other, "packages": summary(&other)},
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
