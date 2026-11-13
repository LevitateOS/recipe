use anyhow::{Context, Result};
use levitate_recipe::packages::{InstalledPackage, PackageStore};
use levitate_recipe::{AutoFixConfig, GinuxLauncher, RecipeEngine, bin_support};
use std::io::{IsTerminal, Write};
use std::path::Path;
use std::path::PathBuf;

use levitate_recipe::sources::{self, Sources};

use super::{
    cli::{Cli, Commands, SourceCommands},
    metadata::{RecipeMetadata, enumerate_recipes},
};

/// Where a recipe named on the command line is looked for.
struct Lookup<'a> {
    folders: &'a [PathBuf],
    /// None with an explicit collection, which stays isolated.
    sources: Option<&'a Sources>,
    /// What a recipe of a remote source is fetched for.
    purpose: sources::Purpose,
}

/// What was looked at is what runs: the recipe file must be the one named.
fn require_sha256(recipe: &Path, expected: Option<&str>) -> Result<()> {
    use sha2::{Digest, Sha256};
    let Some(expected) = expected else {
        return Ok(());
    };
    let found = hex::encode(Sha256::digest(
        std::fs::read(recipe).with_context(|| format!("cannot read {}", recipe.display()))?,
    ));
    anyhow::ensure!(
        found.eq_ignore_ascii_case(expected),
        "{} changed since it was looked at; nothing was run\n  expected SHA-256 {expected}\n  it is now       {found}\nNext: look at the recipe again, then install.",
        recipe.display()
    );
    Ok(())
}

/// The package a change is about, as a front end names it: without its source
/// and without `.rhai`.
fn changed_package(command: &Commands) -> Option<String> {
    let recipe = match command {
        Commands::Install { recipe, .. }
        | Commands::Update { recipe }
        | Commands::Remove { recipe } => recipe.file_stem()?.to_str()?.to_owned(),
        Commands::GoBack { name, .. } | Commands::Hold { name } | Commands::Unhold { name } => {
            name.clone()
        }
        _ => return None,
    };
    Some(match recipe.split_once(':') {
        Some((source, name)) if levitate_recipe::catalog::is_source_name(source) => name.to_owned(),
        _ => recipe,
    })
}

pub(crate) fn execute(cli: Cli) -> Result<()> {
    // A change delegated to recab is reported by the run that makes it.
    if system_route(&cli)? {
        return Ok(());
    }
    let changed = cli
        .machine_events
        .then(|| changed_package(&cli.command))
        .flatten();
    let result = run(cli);
    if let Some(recipe) = changed {
        // For a front end: how the change ended, as the last event.
        eprintln!("{}", bin_support::final_event(&recipe, &result));
    }
    result
}

fn run(cli: Cli) -> Result<()> {
    anyhow::ensure!(
        !(cli.force_acquire || cli.force_build || cli.force_install)
            || matches!(
                &cli.command,
                Commands::Install { .. } | Commands::Update { .. }
            ),
        "force options apply only to install operations"
    );
    let io_start = levitate_recipe::system::process_io();
    let mut execution = levitate_recipe::ExecutionOptions::default();
    execution.target_os = cli.target_os;
    execution.target_arch = cli.target_arch;
    execution.jobs = cli.jobs;
    execution.machine_events = cli.machine_events;
    execution.progress_file = cli.progress_file.clone();
    execution.force_acquire = cli.force_acquire;
    execution.force_build = cli.force_build;
    execution.force_install = cli.force_install;
    execution.exact_identity = cli.exact_identity || levitate_recipe::identity::exact_from_env()?;
    execution.narrowing = levitate_recipe::needs::Narrowing {
        timeout_secs: cli.timeout_secs,
        memory_mib: cli.memory_mib,
        storage_max_objects: cli.storage_max_objects,
        storage_max_bytes: cli.storage_max_bytes,
        network: if cli.no_network {
            Some(Vec::new())
        } else if cli.network_hosts.is_empty() {
            None
        } else {
            Some(cli.network_hosts)
        },
        network_max_bytes: cli.network_max_bytes,
    };
    if matches!(
        &cli.command,
        Commands::Validate { .. }
            | Commands::Inspect { .. }
            | Commands::Explain { .. }
            | Commands::Diff { .. }
    ) {
        execution.validate()?;
    }
    let explicit_collection =
        cli.recipes_path.is_some() || std::env::var_os("RECIPE_PATH").is_some();
    let recipe_paths = super::metadata::recipe_paths(cli.recipes_path);
    let recipes_path = &recipe_paths[0];
    let package_root = match cli.package_root {
        Some(path) => std::path::absolute(path)?,
        None => super::metadata::default_package_root()?,
    };
    let all_sources = Sources::new(&package_root);
    let package_store = PackageStore::new(package_root);
    let lookup = Lookup {
        folders: &recipe_paths,
        sources: (!explicit_collection).then_some(&all_sources),
        purpose: if matches!(
            cli.command,
            Commands::Install { .. } | Commands::Update { .. }
        ) {
            sources::Purpose::Install
        } else {
            sources::Purpose::Look
        },
    };
    let no_activate = cli.no_activate;
    let any_libc = cli.any_libc;
    anyhow::ensure!(
        !any_libc || matches!(&cli.command, Commands::Install { .. } | Commands::Update { .. }),
        "--any-libc applies to install and update"
    );

    let json_output = cli.json_output;
    let list_json = cli.json;
    let json = cli.json || !std::io::stdout().is_terminal();
    let engine_options = EngineOptions {
        build_dir: cli.build_dir.as_deref(),
        state_dir: cli.state_dir.as_deref(),
        execution,
        recipes_path: Some(recipes_path),
        defines: &cli.defines,
        inputs: &cli.inputs,
        no_persist_state: cli.no_persist_state,
        llm_profile: cli.llm_profile,
        activate_into: (!no_activate).then_some(&package_store),
    };

    let command_name = match &cli.command {
        Commands::Hold { .. } => "hold",
        Commands::Unhold { .. } => "unhold",
        _ => "",
    };
    match cli.command {
        Commands::Validate { recipe } => {
            let recipe = resolve_recipe_path(&recipe, &lookup)?;
            let supplied = parse_inputs(engine_options.inputs)?;
            let authority = levitate_recipe::needs::report::validate(
                &recipe,
                Some(recipes_path),
                &supplied,
                &engine_options.execution.narrowing,
            )?;
            emit_result(
                &serde_json::json!({"valid":true,"authority":authority}),
                json_output.as_deref(),
                json,
                "Validation passed",
            )?;
        }
        Commands::Inspect { recipe } => {
            let recipe = resolve_recipe_path(&recipe, &lookup)?;
            let authority = levitate_recipe::needs::report::inspect(
                &recipe,
                Some(recipes_path),
                &engine_options.execution.narrowing,
            )?;
            emit_result(
                &authority,
                json_output.as_deref(),
                json,
                "Recipe permissions and limits",
            )?;
        }
        Commands::Explain { subject, operation } => {
            let recorded = if subject.is_dir() {
                Some(subject.clone())
            } else {
                bin_support::retained_invocation(engine_options.state_dir, &subject)
            };
            let explanation = if let Some(directory) = recorded {
                levitate_recipe::needs::report::explain_invocation(&directory, &operation)?
            } else {
                let recipe = resolve_recipe_path(&subject, &lookup)?;
                levitate_recipe::needs::report::explain_recipe(
                    &recipe,
                    Some(recipes_path),
                    &operation,
                    &engine_options.execution.narrowing,
                )?
            };
            emit_result(
                &explanation,
                json_output.as_deref(),
                json,
                "Operation explanation",
            )?;
        }
        Commands::Diff { old, new } => {
            let old = resolve_recipe_path(&old, &lookup)?;
            let new = resolve_recipe_path(&new, &lookup)?;
            emit_result(
                &levitate_recipe::needs::report::diff(&old, &new, Some(recipes_path))?,
                json_output.as_deref(),
                json,
                "Recipe permission changes",
            )?;
        }
        Commands::Install {
            recipe,
            sha256,
            autofix,
            autofix_attempts,
            autofix_cwd,
            autofix_prompt_file,
            autofix_allow_path,
        } => {
            let recipe_path = resolve_recipe_path(&recipe, &lookup)?;
            require_sha256(&recipe_path, sha256.as_deref())?;
            let autofix_cfg = if autofix {
                Some(AutoFixConfig {
                    attempts: autofix_attempts,
                    cwd: autofix_cwd,
                    prompt_file: autofix_prompt_file,
                    allow_paths: autofix_allow_path,
                })
            } else {
                None
            };
            check_libc(
                &recipe_path,
                recipes_path,
                "install",
                &recipe,
                any_libc,
                (json_output.as_deref(), json),
            )?;
            let engine = create_engine(&recipe_path, &engine_options, autofix_cfg)?;
            let mut ctx = engine.execute(&recipe_path)?;
            if !no_activate {
                activate_package(&package_store, &recipe_path, recipes_path, &mut ctx, None)?;
            }
            ctx["producer_io"] = levitate_recipe::system::io_delta(&io_start);
            emit_result(&ctx, json_output.as_deref(), json, "Installation completed")?;
        }

        Commands::Update { recipe } => {
            anyhow::ensure!(
                !no_activate,
                "update manages active packages; use install --no-activate for producers"
            );
            let installed = installed_package(&package_store, &recipe)?
                .with_context(|| format!("package {} is not installed", recipe.display()))?;
            anyhow::ensure!(
                !installed.held,
                "{0} is held, so it is not updated. Release it with: recipe unhold {0}",
                installed.name
            );
            // A package fetched from a remote source is updated from that
            // source, unless the command names another.
            let named = match &installed.source {
                Some(source)
                    if !explicit_collection && recipe.as_os_str() == installed.name.as_str() =>
                {
                    PathBuf::from(format!("{source}:{}", installed.name))
                }
                _ => recipe,
            };
            let recipe_path = resolve_recipe_path(&named, &lookup).or_else(|error| {
                if explicit_collection {
                    Err(error)
                } else {
                    resolve_recipe_path(&installed.recipe, &lookup)
                }
            })?;
            if installed.source.is_some() && recipe_path == installed.recipe {
                let result = serde_json::json!({"name": installed.name, "version": installed.version,
                    "operation": "update", "current": true, "profile": package_store.profile_path()});
                emit_result(&result, json_output.as_deref(), json, "Package is current")?;
                return Ok(());
            }
            check_libc(
                &recipe_path,
                recipes_path,
                "update",
                &PathBuf::from(&installed.name),
                any_libc,
                (json_output.as_deref(), json),
            )?;
            let engine = create_engine(&recipe_path, &engine_options, None)?;
            let mut ctx = engine.execute(&recipe_path)?;
            activate_package(
                &package_store,
                &recipe_path,
                recipes_path,
                &mut ctx,
                Some(&installed.name),
            )?;
            if let Some(source) = &installed.source {
                all_sources.forget(source, &installed.name, Some(&recipe_path));
            }
            emit_result(&ctx, json_output.as_deref(), json, "Package updated")?;
        }

        Commands::Installed => {
            let mut kept = package_store.kept()?;
            let packages = package_store
                .list()?
                .into_iter()
                .map(|package| {
                    let earlier = kept.remove(&package.name).unwrap_or_default();
                    let mut entry = serde_json::to_value(&package)?;
                    entry["kept"] = serde_json::to_value(earlier)?;
                    entry["held"] = package.held.into();
                    Ok(entry)
                })
                .collect::<Result<Vec<_>>>()?;
            let result =
                serde_json::json!({"packages": packages, "profile": package_store.profile_path()});
            emit_result(&result, json_output.as_deref(), json, "Installed packages")?;
        }

        Commands::Updates => {
            let result = updates(&package_store, &lookup)?;
            if json_output.is_some() || json {
                emit_result(&result, json_output.as_deref(), json, "Updates")?;
            } else {
                print_updates(&result);
            }
        }

        Commands::History => {
            let entries = package_store.history()?;
            if json_output.is_some() || json {
                let result = serde_json::json!({"schema_version": 1, "history": entries});
                emit_result(&result, json_output.as_deref(), json, "History")?;
            } else if entries.is_empty() {
                bin_support::info("Nothing has been done to the installed software yet");
            } else {
                for entry in &entries {
                    println!("{}", history_line(entry));
                }
            }
        }

        Commands::Hold { name } | Commands::Unhold { name } => {
            let held = matches!(command_name, "hold");
            let installed = installed_package(&package_store, Path::new(&name))?
                .with_context(|| format!("package {name} is not installed"))?;
            let changed = package_store.set_held(&installed.name, held)?;
            let result = serde_json::json!({"name": changed.name, "version": changed.version,
                "operation": command_name, "held": changed.held, "profile": package_store.profile_path()});
            emit_result(
                &result,
                json_output.as_deref(),
                json,
                if held {
                    "Package held"
                } else {
                    "Package released"
                },
            )?;
        }

        Commands::GoBack { name, to } => {
            let back = package_store.go_back(&name, to.as_deref())?;
            let result = serde_json::json!({"name": back.name, "version": back.version,
                "operation": "go-back", "profile": package_store.profile_path()});
            emit_result(
                &result,
                json_output.as_deref(),
                json,
                "Earlier version activated",
            )?;
        }

        Commands::Storage { clear_earlier } => {
            let result = if clear_earlier {
                let cleared = package_store.clear_earlier()?;
                serde_json::json!({"schema_version": levitate_recipe::catalog::SCHEMA_VERSION,
                    "operation": "clear-earlier", "cleared_generations": cleared.earlier_generations,
                    "cleared_bytes": cleared.earlier_bytes, "active_bytes": cleared.active_bytes,
                    "earlier_generations": 0, "earlier_bytes": 0, "profile": package_store.profile_path()})
            } else {
                let storage = package_store.storage()?;
                serde_json::json!({"schema_version": levitate_recipe::catalog::SCHEMA_VERSION,
                    "active_bytes": storage.active_bytes, "earlier_generations": storage.earlier_generations,
                    "earlier_bytes": storage.earlier_bytes, "profile": package_store.profile_path()})
            };
            if json_output.is_some() || json {
                emit_result(&result, json_output.as_deref(), json, "Storage")?;
            } else {
                if clear_earlier {
                    println!(
                        "Deleted {} earlier generation(s), {} bytes.",
                        result["cleared_generations"], result["cleared_bytes"]
                    );
                }
                println!("Installed software: {} bytes", result["active_bytes"]);
                println!(
                    "Earlier versions:   {} bytes in {} generation(s)",
                    result["earlier_bytes"], result["earlier_generations"]
                );
            }
        }

        Commands::Remove { recipe } => {
            if !no_activate && let Some(installed) = installed_package(&package_store, &recipe)? {
                let removed = package_store.remove(&installed.name)?;
                if let Some(source) = &removed.source {
                    all_sources.forget(source, &removed.name, None);
                }
                let result = serde_json::json!({"name": removed.name, "version": removed.version,
                        "operation": "remove", "profile": package_store.profile_path()});
                emit_result(&result, json_output.as_deref(), json, "Package removed")?;
                return Ok(());
            }
            let recipe_path = resolve_recipe_path(&recipe, &lookup)?;
            require_output_only_if_inactive(&recipe_path, recipes_path, no_activate)?;
            let engine = create_engine(&recipe_path, &engine_options, None)?;
            let ctx = engine.remove(&recipe_path)?;
            emit_result(&ctx, json_output.as_deref(), json, "Removal completed")?;
        }

        Commands::Cleanup { recipe, reason } => {
            let recipe_path = resolve_recipe_path(&recipe, &lookup)?;
            let engine = create_engine(&recipe_path, &engine_options, None)?;
            let cleanup_reason = reason
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or(levitate_recipe::bin_support::DEFAULT_CLEANUP_REASON);
            let ctx = engine.cleanup_with_reason(&recipe_path, cleanup_reason)?;
            emit_result(&ctx, json_output.as_deref(), json, "Cleanup completed")?;
        }

        Commands::IsInstalled { recipe } => {
            if !no_activate && let Some(installed) = installed_package(&package_store, &recipe)? {
                let installed = package_store.verify(&installed.name)?;
                let result = serde_json::json!({"name": installed.name, "version": installed.version,
                        "profile": package_store.profile_path()});
                emit_result(&result, json_output.as_deref(), json, "Package is active")?;
                return Ok(());
            }
            let recipe_path = resolve_recipe_path(&recipe, &lookup)?;
            require_output_only_if_inactive(&recipe_path, recipes_path, no_activate)?;
            let engine = create_engine(&recipe_path, &engine_options, None)?;
            let ctx = engine.is_installed(&recipe_path)?;
            emit_result(
                &ctx,
                json_output.as_deref(),
                json,
                "Installation check passed",
            )?;
        }

        Commands::IsBuilt { recipe } => {
            let recipe_path = resolve_recipe_path(&recipe, &lookup)?;
            let engine = create_engine(&recipe_path, &engine_options, None)?;
            let ctx = engine.is_built(&recipe_path)?;
            emit_result(&ctx, json_output.as_deref(), json, "Build check passed")?;
        }

        Commands::IsAcquired { recipe } => {
            let recipe_path = resolve_recipe_path(&recipe, &lookup)?;
            let engine = create_engine(&recipe_path, &engine_options, None)?;
            let ctx = engine.is_acquired(&recipe_path)?;
            emit_result(
                &ctx,
                json_output.as_deref(),
                json,
                "Source acquisition check passed",
            )?;
        }

        Commands::List => list_recipes(&lookup, list_json)?,

        Commands::Source { command } => {
            anyhow::ensure!(
                !explicit_collection,
                "an explicit --recipes-path or $RECIPE_PATH uses no sources"
            );
            source_command(&all_sources, command, list_json)?;
        }

        Commands::Refresh { name } => {
            anyhow::ensure!(
                !explicit_collection,
                "an explicit --recipes-path or $RECIPE_PATH uses no sources"
            );
            let remote: Vec<_> = match name {
                Some(name) => vec![all_sources.get(&name)?],
                None => all_sources
                    .all()
                    .0
                    .into_iter()
                    .filter(|source| matches!(source.kind, sources::Kind::Remote { .. }))
                    .collect(),
            };
            if remote.is_empty() {
                bin_support::info("No remote sources to refresh");
            }
            let mut failed = 0;
            for source in &remote {
                match all_sources.refresh(source) {
                    Ok(index) => bin_support::info(&format!(
                        "{}: {} recipe{} at commit {}",
                        source.name,
                        index.recipes.len(),
                        if index.recipes.len() == 1 { "" } else { "s" },
                        &index.commit[..12]
                    )),
                    Err(error) => {
                        failed += 1;
                        bin_support::warning(&format!("{}: {error:#}", source.name));
                    }
                }
            }
            anyhow::ensure!(
                failed == 0,
                "{failed} of {} sources could not be refreshed; the index kept from before stays in use",
                remote.len()
            );
        }

        Commands::Index { folder, check } => {
            let index = levitate_recipe::catalog::index(&folder)?;
            let path = folder.join(levitate_recipe::catalog::INDEX_FILE);
            let summary = format!(
                "{} recipe{} at commit {}",
                index.recipes,
                if index.recipes == 1 { "" } else { "s" },
                &index.commit[..index.commit.len().min(12)]
            );
            if std::fs::read_to_string(&path).is_ok_and(|current| current == index.text) {
                bin_support::info(&format!("{} is current: {summary}", path.display()));
            } else if check {
                anyhow::bail!(
                    "{} is missing or out of date\nNext: run 'recipe index {}' and commit the result.",
                    path.display(),
                    folder.display()
                );
            } else {
                std::fs::write(&path, &index.text)
                    .with_context(|| format!("cannot write {}", path.display()))?;
                bin_support::info(&format!("Wrote {}: {summary}", path.display()));
            }
        }

        Commands::Info { recipe } => {
            let recipe_path = resolve_recipe_path(&recipe, &lookup)?;
            show_info(&recipe_path, Some(recipes_path))?;
        }

        Commands::Hash { file } => {
            use owo_colors::OwoColorize;

            if !file.exists() {
                anyhow::bail!("File not found: {}", file.display());
            }

            bin_support::info(&format!("Computing hashes for {}...", file.display()));

            let hashes = bin_support::compute_all_hashes(&file)
                .with_context(|| format!("Failed to compute hashes for {}", file.display()))?;

            println!();
            println!("{:<10} {}", "sha256:".bold(), hashes.sha256.cyan());
            println!("{:<10} {}", "sha512:".bold(), hashes.sha512.cyan());
            println!("{:<10} {}", "blake3:".bold(), hashes.blake3.cyan());
            println!();
            println!(
                "{}",
                "Copy one of these into your recipe's acquire() function:".dimmed()
            );
            println!(
                "  {}",
                format!("verify_sha256(path, \"{}\");", hashes.sha256).green()
            );
        }
    }

    Ok(())
}

/// An inactive package must not look installed merely because build outputs remain.
fn require_output_only_if_inactive(recipe: &Path, search: &Path, no_activate: bool) -> Result<()> {
    if !no_activate {
        let declaration = levitate_recipe::needs::declaration(recipe, Some(search))?;
        anyhow::ensure!(
            !declaration.needs.outputs.contains_key("package"),
            "package is not active; use install first, or --no-activate to inspect/remove producer outputs"
        );
    }
    Ok(())
}

/// Active package identity is recorded independently of mutable recipe sources.
fn installed_package(store: &PackageStore, requested: &Path) -> Result<Option<InstalledPackage>> {
    // A profile holds one package of a name, whichever source it came from.
    let name = requested
        .to_str()
        .filter(|_| requested.components().count() == 1)
        .map(|name| match name.split_once(':') {
            Some((source, name)) if levitate_recipe::catalog::is_source_name(source) => name,
            _ => name,
        });
    let canonical = requested.canonicalize().ok().or_else(|| {
        (requested.components().count() > 1)
            .then(|| std::path::absolute(requested).ok())
            .flatten()
    });
    Ok(store.list()?.into_iter().find(|installed| {
        name.is_some_and(|name| {
            name == installed.name || name == format!("{}.rhai", installed.name)
        }) || canonical
            .as_ref()
            .is_some_and(|path| path == &installed.recipe)
    }))
}

/// Before anything is acquired: compare the C library the recipe (and its
/// runtime dependencies) declare with this system's. A mismatch ends the
/// command with the facts and the flag that installs anyway, and a JSON
/// failure record when the caller asked for JSON; with `--any-libc` it is one
/// note line.
fn check_libc(
    recipe: &Path,
    search: &Path,
    verb: &str,
    requested: &Path,
    any_libc: bool,
    (json_output, json): (Option<&Path>, bool),
) -> Result<()> {
    let host = levitate_recipe::clibrary::detect();
    let requested = requested.to_string_lossy();
    let verdict =
        levitate_recipe::clibrary::check(recipe, Some(search), &host, verb, &requested)?;
    for note in &verdict.notes {
        eprintln!("{note}");
    }
    if any_libc {
        for mismatch in &verdict.mismatches {
            eprintln!("{}", mismatch.note());
        }
        return Ok(());
    }
    if let Some(mismatch) = verdict.mismatches.into_iter().next() {
        if json_output.is_some() || json {
            emit_result(&mismatch.json(), json_output, json, "Not installed")?;
        }
        return Err(mismatch.into());
    }
    Ok(())
}

fn activate_package(
    store: &PackageStore,
    recipe: &Path,
    search: &Path,
    ctx: &mut serde_json::Value,
    updating: Option<&str>,
) -> Result<()> {
    let output = ctx
        .get("outputs")
        .and_then(|outputs| outputs.get("package"));
    if output.is_none() && updating.is_none() {
        return Ok(());
    }
    let output = output
        .and_then(|value| value.as_str())
        .context("user-managed recipe must publish a directory output named package")?;
    let name = ctx
        .get("name")
        .and_then(|value| value.as_str())
        .context("package output requires ctx.name")?;
    let version = ctx
        .get("version")
        .and_then(|value| value.as_str())
        .context("package output requires ctx.version")?;
    if let Some(expected) = updating {
        anyhow::ensure!(
            name == expected,
            "update recipe names {name}, expected {expected}"
        );
    }
    // What was carried was told this folder; it must be where it lands.
    if let Some(prefix) = ctx
        .pointer("/runtime/paths/prefix")
        .and_then(|value| value.as_str())
    {
        let expected = store.package_prefix(name)?;
        anyhow::ensure!(
            Path::new(prefix) == expected,
            "the package was built for {prefix} but would be activated at {}",
            expected.display()
        );
    }
    // What the recipe may use is kept with the package, so that a later
    // recipe can be compared with it ('recipe updates').
    let needs = levitate_recipe::needs::declaration(recipe, Some(search))
        .ok()
        .map(|declaration| declaration.needs);
    let installed = store.install(
        name,
        version,
        recipe,
        needs.as_ref(),
        Path::new(output),
        updating.is_some(),
    )?;
    let profile = store.profile_path();
    ctx["activation"] = serde_json::json!({"name": installed.name, "version": installed.version,
        "profile": profile, "bin": profile.join("bin")});
    let bin = profile.join("bin");
    if !std::env::var_os("PATH").is_some_and(|path| std::env::split_paths(&path).any(|p| p == bin))
    {
        eprintln!(
            "Recipe packages are active in {}. Add this directory to PATH to use them by name.",
            bin.display()
        );
    }
    Ok(())
}

fn emit_result(
    value: &serde_json::Value,
    path: Option<&Path>,
    json: bool,
    summary: &str,
) -> Result<()> {
    if let Some(path) = path {
        std::fs::write(path, serde_json::to_string(value)?)
            .with_context(|| format!("Failed to write JSON to {}", path.display()))?;
        if !json {
            println!("{summary}. Full result saved to {}.", path.display());
        }
    } else if json {
        writeln!(std::io::stdout(), "{}", serde_json::to_string(value)?)?;
    } else {
        println!("{summary}");
        print_summary(value);
        println!("\nUse --json for the full result, or --json-output <FILE> to save it.");
    }
    Ok(())
}

/// Terminal summaries expose the useful result; the full context and contract
/// remain available as JSON, including the historical output to pipes.
fn print_summary(value: &serde_json::Value) {
    if let Some(authority) = value.get("authority") {
        print_summary(authority);
        return;
    }
    for (field, label) in [
        ("name", "Package"),
        ("version", "Version"),
        ("recipe", "Recipe"),
        ("operation", "Operation"),
        ("phase", "Observation"),
        ("limitation", "Limitation"),
    ] {
        if let Some(text) = value.get(field).and_then(|v| v.as_str()) {
            println!("  {label}: {text}");
        }
    }
    for (field, label) in [
        ("activation", "Active package"),
        ("packages", "Installed packages"),
        ("profile", "Profile"),
        ("outputs", "Outputs"),
        ("effective", "Resource limits"),
        ("changes", "Changes"),
        ("decisions", "Recorded decisions"),
    ] {
        if let Some(details) = value.get(field) {
            println!("\n{label}:");
            print_details(details, 2);
        }
    }
    if let Some(needs) = value.get("needs") {
        for (field, label) in [
            ("inputs", "Inputs"),
            ("outputs", "Declared outputs"),
            ("network", "Declared network endpoints"),
        ] {
            if let Some(details) = needs.get(field) {
                println!("\n{label}:");
                print_details(details, 2);
            }
        }
    }
    if let Some(dependencies) = value.get("dependencies").and_then(|v| v.as_array()) {
        println!("\nDependencies: {}", dependencies.len());
        for dependency in dependencies {
            if let Some(recipe) = dependency
                .pointer("/declaration/recipe")
                .and_then(|v| v.as_str())
            {
                println!("  {recipe}");
            }
        }
    }
    if let Some(changed) = value.get("authority_changed").and_then(|v| v.as_bool()) {
        println!(
            "\nPermissions or limits changed: {}",
            if changed { "yes" } else { "no" }
        );
    }
    if let Some(text) = value.get("ginux").and_then(|v| v.as_str()) {
        println!("\n{text}");
    }
    if let Some(text) = value.get("recipe_rule").and_then(|v| v.as_str()) {
        println!("Recipe rule: {text}");
    }
}

fn print_details(value: &serde_json::Value, indent: usize) {
    match value {
        serde_json::Value::Object(fields) if !fields.is_empty() => {
            for (key, value) in fields {
                let label = match key.as_str() {
                    "memory_mib" => "Memory limit (MiB)".to_owned(),
                    "network" => "Network endpoints".to_owned(),
                    "network_max_bytes" => "Network transfer budget".to_owned(),
                    "storage_max_bytes" => "Storage byte limit".to_owned(),
                    "storage_max_objects" => "Storage object limit".to_owned(),
                    "timeout_secs" => "Time limit (seconds)".to_owned(),
                    _ => key.replace('_', " "),
                };
                if value.is_object() || value.is_array() {
                    println!("{:indent$}{label}:", "");
                    print_details(value, indent + 2);
                } else {
                    print!("{:indent$}{label}: ", "");
                    if key.ends_with("_bytes") && value.is_u64() {
                        let bytes = value.as_u64().unwrap();
                        let gib = bytes as f64 / (1024.0 * 1024.0 * 1024.0);
                        if gib >= 1.0 {
                            println!("{gib:.1} GiB ({bytes} bytes)");
                        } else {
                            println!("{bytes} bytes");
                        }
                    } else {
                        print_details(value, 0);
                    }
                }
            }
        }
        serde_json::Value::Array(items) if !items.is_empty() => {
            for item in items {
                print_details(item, indent);
            }
        }
        serde_json::Value::String(text) => println!("{:indent$}{text}", ""),
        serde_json::Value::Null => println!("{:indent$}not set", ""),
        serde_json::Value::Array(_) | serde_json::Value::Object(_) => {
            println!("{:indent$}none", "")
        }
        other => println!("{:indent$}{other}", ""),
    }
}

/// Configure one executable operation through mandatory Ginux. Metadata and
/// hashing commands do not evaluate recipe code and need no execution domain.
struct EngineOptions<'a> {
    build_dir: Option<&'a Path>,
    state_dir: Option<&'a Path>,
    execution: levitate_recipe::ExecutionOptions,
    recipes_path: Option<&'a Path>,
    defines: &'a [String],
    inputs: &'a [String],
    no_persist_state: bool,
    llm_profile: Option<String>,
    /// The store a package output will be activated in; None with
    /// --no-activate. It decides RUNTIME.paths.prefix.
    activate_into: Option<&'a PackageStore>,
}

fn create_engine(
    recipe: &Path,
    options: &EngineOptions<'_>,
    autofix: Option<AutoFixConfig>,
) -> Result<RecipeEngine> {
    let EngineOptions {
        build_dir,
        state_dir,
        execution,
        recipes_path,
        defines,
        inputs,
        no_persist_state,
        llm_profile,
        activate_into,
    } = options;
    let mut execution = execution.clone();
    execution.package_prefix = package_prefix(*activate_into, recipe, *recipes_path)?;
    execution.validate()?;
    let launcher = GinuxLauncher::from_env()?;
    let engine = match build_dir {
        Some(dir) => {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("Failed to create build directory: {}", dir.display()))?;
            RecipeEngine::new(dir.to_path_buf())
        }
        None if *no_persist_state => RecipeEngine::with_temporary_destination()?,
        None => RecipeEngine::new(levitate_recipe::default_destination(*state_dir, recipe)?),
    };
    let mut engine = engine
        .with_launcher(launcher)
        .with_state_persistence(!*no_persist_state)
        .with_state_dir(state_dir.map(Path::to_owned))
        .with_execution_options(execution)
        .with_llm_profile(llm_profile.clone())
        .with_autofix(autofix);

    if let Some(rp) = recipes_path {
        engine = engine.with_recipes_path(rp.to_path_buf());
    }

    for define in *defines {
        if let Some((key, value)) = define.split_once('=') {
            engine.add_define(key.to_string(), value.to_string());
        } else {
            anyhow::bail!("Invalid --define format: '{}' (expected KEY=VALUE)", define);
        }
    }

    for (name, path) in parse_inputs(inputs)? {
        engine.add_input(name, path);
    }

    Ok(engine)
}

/// RUNTIME.paths.prefix for a recipe whose package output will be activated
/// in `store`: the folder the package will live in. It needs the package's
/// name before anything runs, so the recipe's `name` must be a literal;
/// otherwise there is no prefix (and carrying refuses to start).
fn package_prefix(
    store: Option<&PackageStore>,
    recipe: &Path,
    search: Option<&Path>,
) -> Result<Option<String>> {
    let Some(store) = store else {
        return Ok(None);
    };
    let declared = levitate_recipe::declared_metadata(recipe, search)?;
    let Some(name) = declared
        .fields
        .get("name")
        .filter(|_| !declared.dynamic_fields.contains("name"))
    else {
        return Ok(None);
    };
    let Ok(prefix) = store.package_prefix(name) else {
        return Ok(None); // activation refuses the name with its own message
    };
    let prefix = prefix
        .to_str()
        .with_context(|| format!("package folder {} is not UTF-8", prefix.display()))?;
    Ok(Some(prefix.to_owned()))
}

fn resolve_recipe_path(recipe: &Path, lookup: &Lookup) -> Result<PathBuf> {
    if recipe.is_absolute() {
        if recipe.is_file() {
            return Ok(recipe.to_path_buf());
        }
        anyhow::bail!("Recipe file not found: {}", recipe.display());
    }

    if recipe.is_file() {
        return Ok(recipe.to_path_buf());
    }

    let text = recipe.to_string_lossy();

    // SOURCE:NAME names one source's recipe, whatever the others hold.
    if let Some((source, name)) = text.split_once(':')
        && levitate_recipe::catalog::is_source_name(source)
        && levitate_recipe::catalog::is_file_name(name)
    {
        let folder = match source {
            sources::LOCAL => lookup.folders.first().cloned(),
            _ => {
                let all = lookup.sources.with_context(|| {
                    format!("an explicit --recipes-path or $RECIPE_PATH uses no sources, so {text} names nothing")
                })?;
                let from = all.get(source)?;
                match &from.kind {
                    sources::Kind::Folder { path } => Some(path.clone()),
                    sources::Kind::Remote { .. } => {
                        return all.fetch(&from, name, lookup.purpose);
                    }
                }
            }
        };
        let file = folder
            .with_context(|| format!("there is no {source} folder in this run"))?
            .join(format!("{name}.rhai"));
        anyhow::ensure!(
            file.is_file(),
            "{source} has no recipe named {name} ({} is missing)",
            file.display()
        );
        return Ok(file);
    }

    for recipes_path in lookup.folders {
        let in_recipes = recipes_path.join(recipe);
        if in_recipes.is_file() {
            return Ok(in_recipes);
        }
        let with_ext = recipes_path.join(format!("{}.rhai", recipe.display()));
        if with_ext.is_file() {
            return Ok(with_ext);
        }
    }

    // A bare name the folders do not hold means the one source that has it.
    let name = text.strip_suffix(".rhai").unwrap_or(&text);
    if let Some(all) = lookup.sources
        && levitate_recipe::catalog::is_file_name(name)
    {
        let mut found = all.find(name);
        if found.len() > 1 {
            anyhow::bail!(
                "{name} is in more than one source\nNext: name the one you mean: {}",
                found
                    .iter()
                    .map(|found| format!("{}:{name}", found.source))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        if let Some(found) = found.pop() {
            return match found.path {
                Some(path) => Ok(path),
                None => all.fetch(&all.get(&found.source)?, name, lookup.purpose),
            };
        }
    }

    anyhow::bail!(
        "Recipe not found: {}\nSearched in the current directory, the sources and: {}\nNext: run 'recipe list' to see available recipes, or pass a .rhai file path.",
        recipe.display(),
        lookup
            .folders
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    )
}

/// The catalog entry `recipe update NAME` would run for an installed package,
/// read without fetching or running anything: its own source's, else the
/// recipe folders', else the one source that has it, else the recipe file it
/// was installed from.
fn update_candidate(
    installed: &InstalledPackage,
    lookup: &Lookup,
    listings: &mut std::collections::BTreeMap<String, Vec<serde_json::Value>>,
) -> Result<Option<serde_json::Value>> {
    let mut listed = |all: &Sources, source: &str| -> Option<serde_json::Value> {
        if !listings.contains_key(source) {
            let recipes = all.get(source).map(|source| all.listing(&source).recipes);
            listings.insert(source.to_owned(), recipes.unwrap_or_default());
        }
        listings[source]
            .iter()
            .find(|recipe| recipe["name"] == installed.name.as_str())
            .cloned()
    };
    if let (Some(source), Some(all)) = (&installed.source, lookup.sources)
        && let Some(entry) = listed(all, source)
    {
        return Ok(Some(entry));
    }
    for folder in lookup.folders {
        let path = folder.join(format!("{}.rhai", installed.name));
        if path.is_file() {
            let mut entry = levitate_recipe::catalog::entry(&path, Some(folder))?;
            entry["source"] = sources::LOCAL.into();
            return Ok(Some(entry));
        }
    }
    if let Some(all) = lookup.sources {
        let found = all.find(&installed.name);
        if let [only] = found.as_slice()
            && let Some(entry) = listed(all, &only.source)
        {
            return Ok(Some(entry));
        }
    }
    if installed.recipe.is_file() {
        return levitate_recipe::catalog::entry(&installed.recipe, installed.recipe.parent())
            .map(Some);
    }
    Ok(None)
}

/// Which installed packages `recipe update` would change. See `recipe updates --help`.
fn updates(store: &PackageStore, lookup: &Lookup) -> Result<serde_json::Value> {
    let mut listings = std::collections::BTreeMap::new();
    let mut found = Vec::new();
    let mut problems = Vec::new();
    for installed in store.list()? {
        let entry = match update_candidate(&installed, lookup, &mut listings) {
            Ok(Some(entry)) => entry,
            Ok(None) => {
                problems.push(serde_json::json!({"name": installed.name,
                    "problem": "no recipe for it is in the collection or the sources any more"}));
                continue;
            }
            Err(error) => {
                problems.push(
                    serde_json::json!({"name": installed.name, "problem": format!("{error:#}")}),
                );
                continue;
            }
        };
        let to = entry["version"].as_str().unwrap_or_default();
        // A source publishes the checksum of each recipe file; a folder does not.
        let published = entry["files"][0]["sha256"].as_str();
        let other_file = published.is_some() && published != installed.sha256.as_deref();
        let reason = if !to.is_empty() && to != installed.version {
            "version"
        } else if other_file {
            "recipe"
        } else {
            continue;
        };
        let newer: Option<levitate_recipe::needs::Needs> =
            serde_json::from_value(entry["needs"].clone()).ok();
        let changes = match (&installed.needs, &newer) {
            (Some(before), Some(after)) => {
                serde_json::to_value(levitate_recipe::updates::changes(before, after))?
            }
            _ => serde_json::Value::Null,
        };
        found.push(serde_json::json!({
            "name": installed.name,
            "source": entry["source"],
            "from": installed.version,
            "to": if to.is_empty() { installed.version.as_str() } else { to },
            "reason": reason,
            "changes": changes,
            "held": installed.held,
        }));
    }
    Ok(serde_json::json!({
        "schema_version": levitate_recipe::catalog::SCHEMA_VERSION,
        "updates": found,
        "problems": problems,
    }))
}

fn print_updates(result: &serde_json::Value) {
    let found = result["updates"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default();
    if found.is_empty() {
        bin_support::info("Everything installed is at its recipe's version");
    }
    for update in found {
        let (name, from, to) = (
            update["name"].as_str().unwrap_or("?"),
            update["from"].as_str().unwrap_or("?"),
            update["to"].as_str().unwrap_or("?"),
        );
        let held = if update["held"] == true {
            " (held: 'recipe update' leaves it; 'recipe unhold' releases it)"
        } else {
            ""
        };
        if update["reason"] == "recipe" {
            println!("  {name} {from}: its source publishes another recipe{held}");
        } else {
            println!("  {name} {from} -> {to}{held}");
        }
        match update["changes"].as_array() {
            None => println!(
                "      what the newer recipe may use was not compared: nothing was recorded at install"
            ),
            Some(changes) if changes.is_empty() => println!("      it may use the same as before"),
            Some(changes) => {
                for change in changes {
                    println!(
                        "      {}: {} -> {} ({})",
                        change["field"].as_str().unwrap_or("?"),
                        change["before"].as_str().unwrap_or("?"),
                        change["after"].as_str().unwrap_or("?"),
                        change["kind"].as_str().unwrap_or("?"),
                    );
                }
            }
        }
    }
    for problem in result["problems"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
    {
        bin_support::warning(&format!(
            "{}: {}",
            problem["name"].as_str().unwrap_or("?"),
            problem["problem"].as_str().unwrap_or_default()
        ));
    }
}

/// One history entry as a line for a terminal: when (UTC), what, which package.
fn history_line(entry: &levitate_recipe::packages::HistoryEntry) -> String {
    let days = (entry.at / 86_400) as i64;
    let seconds = entry.at % 86_400;
    // Civil date from days since 1970 (proleptic Gregorian).
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    let mut line = format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}  {}",
        seconds / 3600,
        seconds % 3600 / 60,
        seconds % 60,
        entry.what
    );
    if let Some(package) = &entry.package {
        line.push(' ');
        line.push_str(package);
    }
    match (entry.from.as_deref(), entry.to.as_deref()) {
        (Some(from), Some(to)) if from != to => line.push_str(&format!(" {from} -> {to}")),
        (Some(version), Some(_)) | (Some(version), None) | (None, Some(version)) => {
            line.push(' ');
            line.push_str(version);
        }
        (None, None) => {}
    }
    if let Some(source) = &entry.source {
        line.push_str(&format!(" (from {source})"));
    }
    line
}

fn problem_json(problem: &sources::Problem) -> serde_json::Value {
    serde_json::json!({ "path": problem.path, "problem": problem.problem })
}

fn list_recipes(lookup: &Lookup, json: bool) -> Result<()> {
    use owo_colors::OwoColorize;

    // The recipe folder first, as before sources existed.
    let folder_name = |_: usize| sources::LOCAL;
    let mut names = std::collections::BTreeSet::new();
    let mut about = Vec::new();
    let mut recipes = Vec::new();
    let mut problems = Vec::new();
    for (index, recipes_path) in lookup.folders.iter().enumerate() {
        about.push(serde_json::json!({
            "name": folder_name(index),
            "kind": "folder",
            "title": null,
            "path": recipes_path,
            "checked": null,
            "problem": null,
        }));
        let mut paths: Vec<_> = enumerate_recipes(recipes_path).collect();
        paths.sort();
        for path in paths {
            if !names.insert(path.file_name().unwrap().to_owned()) {
                continue;
            }
            if !json {
                let meta = RecipeMetadata::load(&path, Some(recipes_path))?;
                recipes.push(serde_json::json!({
                    "name": meta.name,
                    "version": meta.version,
                    "source": folder_name(index),
                }));
                continue;
            }
            // For a front end: every recipe it could offer, read without
            // running anything. One unreadable recipe must not hide the others.
            match levitate_recipe::catalog::entry(&path, Some(recipes_path)) {
                Ok(mut entry) => {
                    entry["source"] = folder_name(index).into();
                    recipes.push(entry);
                }
                Err(error) => problems.push(serde_json::json!({
                    "path": path,
                    "problem": format!("{error:#}"),
                })),
            }
        }
    }
    if let Some(all) = lookup.sources {
        let (found, unreadable) = all.all();
        problems.extend(unreadable.iter().map(problem_json));
        for source in found {
            let listing = all.listing(&source);
            about.push(listing.about);
            recipes.extend(listing.recipes);
            problems.extend(listing.problems.iter().map(problem_json));
        }
    }

    if json {
        let result = serde_json::json!({
            "schema_version": levitate_recipe::catalog::SCHEMA_VERSION,
            "collections": lookup.folders,
            "sources": about,
            "recipes": recipes,
            "problems": problems,
        });
        writeln!(std::io::stdout(), "{}", serde_json::to_string(&result)?)?;
        return Ok(());
    }
    for recipe in &recipes {
        let source = recipe["source"].as_str().unwrap_or_default();
        println!(
            "  {} {}{}",
            recipe["name"].as_str().unwrap_or("?").bold(),
            recipe["version"].as_str().unwrap_or("?").cyan(),
            if source == sources::LOCAL {
                String::new()
            } else {
                format!("  {}", source.dimmed())
            }
        );
    }
    for problem in &problems {
        bin_support::warning(problem["problem"].as_str().unwrap_or_default());
    }
    if recipes.is_empty() {
        bin_support::info("No recipes found in the selected collections");
    }

    Ok(())
}

fn source_command(all: &Sources, command: SourceCommands, json: bool) -> Result<()> {
    use owo_colors::OwoColorize;

    match command {
        SourceCommands::List => {
            let (found, unreadable) = all.all();
            let listings: Vec<_> = found.iter().map(|source| all.listing(source)).collect();
            if json {
                let result = serde_json::json!({
                    "schema_version": levitate_recipe::catalog::SCHEMA_VERSION,
                    "sources": listings.iter().map(|listing| &listing.about).collect::<Vec<_>>(),
                    "problems": unreadable.iter().map(problem_json).collect::<Vec<_>>(),
                });
                writeln!(std::io::stdout(), "{}", serde_json::to_string(&result)?)?;
                return Ok(());
            }
            for listing in &listings {
                let about = &listing.about;
                let text = |field: &str| about[field].as_str().unwrap_or_default().to_owned();
                let place = if about["kind"] == "remote" {
                    format!("{} ({})", text("files"), text("branch"))
                } else {
                    text("path")
                };
                println!(
                    "  {}  {}  {}",
                    text("name").bold(),
                    text("kind").cyan(),
                    place
                );
                let count = format!(
                    "{} recipe{}",
                    listing.recipes.len(),
                    if listing.recipes.len() == 1 { "" } else { "s" }
                );
                let state = match about["checked"].as_u64() {
                    Some(checked) => format!("{count}, fetched {}", sources::ago(checked)),
                    None if about["kind"] == "folder" => count,
                    None if !listing.recipes.is_empty() => {
                        format!("{count}, from the copy that came with the system; not fetched yet")
                    }
                    None => String::new(),
                };
                if !state.is_empty() {
                    println!("      {}", state.dimmed());
                }
                if let Some(problem) = about["problem"].as_str() {
                    bin_support::warning(&format!("{}: {problem}", text("name")));
                }
            }
            for problem in &unreadable {
                bin_support::warning(&problem.problem);
            }
            if listings.is_empty() {
                bin_support::info("No sources; recipes come from the recipe folders only");
            }
        }
        SourceCommands::Add {
            address,
            branch,
            path,
            name,
        } => match (address, path) {
            (_, Some(folder)) => {
                let source = all.add_folder(&folder, name.as_deref())?;
                let count = all.listing(&source).recipes.len();
                bin_support::info(&format!(
                    "Added the folder {} as {}: {count} recipe{}",
                    folder.display(),
                    source.name,
                    if count == 1 { "" } else { "s" }
                ));
            }
            (Some(address), None) => {
                let (source, index) = all.add_remote(&address, &branch)?;
                bin_support::info(&format!(
                    "Added {}: {}, kept by {} ({} recipe{})",
                    source.name,
                    index.source.title,
                    index.source.keeper,
                    index.recipes.len(),
                    if index.recipes.len() == 1 { "" } else { "s" }
                ));
            }
            (None, None) => unreachable!("clap requires an address or --path"),
        },
        SourceCommands::Remove { name } => match all.remove(&name)? {
            sources::Removed::Gone => bin_support::info(&format!("Removed the source {name}")),
            sources::Removed::SwitchedOff(file) => bin_support::info(&format!(
                "{name} comes with the system, so it is switched off; delete {} to bring it back",
                file.display()
            )),
        },
    }
    Ok(())
}

fn show_info(recipe_path: &Path, search_path: Option<&Path>) -> Result<()> {
    use owo_colors::OwoColorize;

    let meta = RecipeMetadata::load(recipe_path, search_path)?;

    println!("{:<12} {}", "Name:".bold(), meta.name.bold().cyan());
    println!(
        "{:<12} {}",
        "Version:".bold(),
        meta.version.as_deref().unwrap_or("?").green()
    );
    if let Some(desc) = &meta.description {
        println!("{:<12} {}", "Description:".bold(), desc);
    }
    println!(
        "{:<12} {}",
        "Recipe:".bold(),
        recipe_path.display().to_string().dimmed()
    );
    if let Some(declared) = levitate_recipe::catalog::declared_libc(recipe_path, search_path)? {
        let host = levitate_recipe::clibrary::detect();
        let build = declared.libc.join(" or ");
        let carried = if declared.carried {
            format!(", and its package brings its own {build} and loader")
        } else {
            String::new()
        };
        println!(
            "{:<12} {}",
            "C library:".bold(),
            format!(
                "its build is linked against {build}{carried}; this system's C library is {} ({})",
                host.libc, host.found
            )
        );
    }

    Ok(())
}

fn parse_inputs(values: &[String]) -> Result<Vec<(String, PathBuf)>> {
    values
        .iter()
        .map(|value| {
            let (name, path) = value
                .split_once('=')
                .context("--input requires NAME=PATH")?;
            if name.is_empty() || path.is_empty() {
                anyhow::bail!("--input requires nonempty NAME=PATH");
            }
            Ok((name.to_owned(), PathBuf::from(path)))
        })
        .collect()
}

/// Root's installed-system software never activates a shared /var profile.
/// Explicit independent roots and builder output-only invocations retain their
/// existing behavior, including on an immutable host.
fn system_route(cli: &Cli) -> Result<bool> {
    let command = match &cli.command {
        Commands::Install { recipe, .. } => Some(("install", Some(recipe))),
        Commands::Update { recipe } => Some(("update", Some(recipe))),
        Commands::Remove { recipe } => Some(("remove", Some(recipe))),
        Commands::Installed => Some(("list", None)),
        Commands::IsInstalled { recipe } => Some(("check", Some(recipe))),
        _ => None,
    };
    let immutable = std::fs::read_to_string("/proc/cmdline")
        .unwrap_or_default()
        .split_whitespace()
        .any(|p| p == "recab.immutable");
    let route = cli.system
        || (!cli.no_activate
            && command.is_some()
            && cli.package_root.is_none()
            && unsafe { libc::geteuid() } == 0
            && immutable);
    if !route {
        anyhow::ensure!(
            !cli.prepared && cli.preparation.is_none(),
            "prepared selection options require the system route"
        );
        return Ok(false);
    }
    let (action, recipe) =
        command.context("--system supports install, update, remove, installed and isinstalled")?;
    anyhow::ensure!(
        cli.build_dir.is_none() && cli.state_dir.is_none(),
        "recab scopes system producer state/build work to the preparation; use --package-root for an independent profile"
    );
    let mut cmd = std::process::Command::new(
        std::env::var_os("RECIPE_RECAB_BIN").unwrap_or_else(|| "recab".into()),
    );
    cmd.arg("change").arg(action);
    if let Some(recipe) = recipe {
        cmd.arg(if recipe.exists() {
            std::fs::canonicalize(recipe)?
        } else {
            recipe.clone()
        });
        if matches!(action, "install" | "update") {
            let paths = super::metadata::recipe_paths(cli.recipes_path.clone());
            let sources = Sources::new(&super::metadata::default_package_root()?);
            let explicit = cli.recipes_path.is_some() || std::env::var_os("RECIPE_PATH").is_some();
            let lookup = Lookup {
                folders: &paths,
                sources: (!explicit).then_some(&sources),
                purpose: sources::Purpose::Install,
            };
            match resolve_recipe_path(recipe, &lookup) {
                Ok(path) => {
                    if let Commands::Install { sha256, .. } = &cli.command {
                        require_sha256(&path, sha256.as_deref())?;
                    }
                    cmd.arg("--producer-recipe")
                        .arg(std::fs::canonicalize(path)?);
                }
                Err(e) if action == "install" || explicit => return Err(e),
                Err(_) => {} // recab falls back to this prepared package's recorded source.
            }
        }
    }
    if let Some(id) = &cli.preparation {
        cmd.arg("--preparation")
            .arg(id)
            .arg("--revision")
            .arg(cli.revision.unwrap().to_string());
    }
    if cli.prepared {
        cmd.arg("--prepared");
    }
    let json = cli.json || !std::io::stdout().is_terminal() || cli.json_output.is_some();
    if json {
        cmd.arg("--json");
    }
    if matches!(action, "install" | "update") {
        cmd.arg("--");
        if let Some(path) = &cli.recipes_path {
            cmd.arg("--recipes-path").arg(std::path::absolute(path)?);
        }
        cmd.args([
            "--target-os",
            &cli.target_os,
            "--target-arch",
            &cli.target_arch,
            "--jobs",
            &cli.jobs.to_string(),
        ]);
        for (flag, value) in [
            ("--timeout-secs", cli.timeout_secs),
            ("--memory-mib", cli.memory_mib),
            ("--storage-max-objects", cli.storage_max_objects),
            ("--storage-max-bytes", cli.storage_max_bytes),
            ("--network-max-bytes", cli.network_max_bytes),
        ] {
            if let Some(value) = value {
                cmd.arg(flag).arg(value.to_string());
            }
        }
        for (flag, value) in [
            ("--no-network", cli.no_network),
            ("--force-acquire", cli.force_acquire),
            ("--force-build", cli.force_build),
            ("--force-install", cli.force_install),
            ("--no-persist-state", cli.no_persist_state),
            ("--exact-identity", cli.exact_identity),
        ] {
            if value {
                cmd.arg(flag);
            }
        }
        for host in &cli.network_hosts {
            cmd.arg("--network-host").arg(host);
        }
        for define in &cli.defines {
            cmd.arg("--define").arg(define);
        }
        for input in &cli.inputs {
            let (name, path) = input.split_once('=').context("input must be NAME=PATH")?;
            cmd.arg("--input")
                .arg(format!("{name}={}", std::path::absolute(path)?.display()));
        }
        if let Some(profile) = &cli.llm_profile {
            cmd.arg("--llm-profile").arg(profile);
        }
        if cli.machine_events {
            cmd.arg("--machine-events");
        }
        if let Some(path) = &cli.progress_file {
            cmd.arg("--progress-file").arg(std::path::absolute(path)?);
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
            cmd.arg("--autofix")
                .arg("--autofix-attempts")
                .arg(autofix_attempts.to_string());
            for (flag, path) in [
                ("--autofix-cwd", autofix_cwd),
                ("--autofix-prompt-file", autofix_prompt_file),
            ] {
                if let Some(path) = path {
                    cmd.arg(flag).arg(std::path::absolute(path)?);
                }
            }
            for path in autofix_allow_path {
                cmd.arg("--autofix-allow-path")
                    .arg(std::path::absolute(path)?);
            }
        }
    }
    cmd.env("RECAB_RECIPE_BIN", std::env::current_exe()?);
    if let Some(path) = std::env::var_os("RECIPE_PATH") {
        cmd.env("RECIPE_PATH", std::path::absolute(path)?);
    }
    if let Some(output) = &cli.json_output {
        let result = cmd.output()?;
        std::io::stderr().write_all(&result.stderr)?;
        anyhow::ensure!(result.status.success(), "system change refused by recab");
        std::fs::write(output, result.stdout)?;
    } else {
        anyhow::ensure!(cmd.status()?.success(), "system change refused by recab");
    }
    Ok(true)
}
