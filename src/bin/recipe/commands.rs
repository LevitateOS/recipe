use anyhow::{Context, Result};
use levitate_recipe::packages::{InstalledPackage, PackageStore};
use levitate_recipe::{RecipeEngine, bin_support};
use std::io::{IsTerminal, Write};
use std::path::Path;
use std::path::PathBuf;

use levitate_recipe::sources::{self, Sources};
use levitate_recipe::state::State;

use super::{
    cli::{Cli, Commands, SourceCommands},
    metadata::enumerate_recipes,
    runtime,
};

/// Where a recipe named on the command line is looked for.
pub(super) struct Lookup<'a> {
    pub(super) folders: &'a [PathBuf],
    /// None with an explicit collection, which stays isolated.
    pub(super) sources: Option<&'a Sources>,
    /// What a recipe of a remote source is fetched for.
    pub(super) purpose: sources::Purpose,
    /// A system change (`--system install|update|remove`): a bare name the
    /// release source carries is its recipe before the recipe folder's, and a
    /// system group of a remote source is refused on a root with a release
    /// source.
    pub(super) system: bool,
}

/// What was looked at is what runs: the recipe file must be the one named.
pub(super) fn require_sha256(recipe: &Path, expected: Option<&str>) -> Result<()> {
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
        | Commands::Update { recipe, .. }
        | Commands::Download { recipe }
        | Commands::Build { recipe, .. }
        | Commands::Remove { recipe, .. }
        | Commands::Cleanup {
            recipe: Some(recipe),
            ..
        } => recipe.file_stem()?.to_str()?.to_owned(),
        _ => return None,
    };
    Some(match recipe.split_once(':') {
        Some((source, name)) if levitate_recipe::catalog::is_source_name(source) => name.to_owned(),
        _ => recipe,
    })
}

pub(crate) fn execute(mut cli: Cli) -> Result<()> {
    // The runtime commands need no package root, recipes or Ginux.
    if let Commands::Runtime { command } = &cli.command {
        return runtime::execute(command, &cli);
    }
    // A system change is reported by the run that makes it.
    if super::system_route::route(&mut cli)? {
        return Ok(());
    }
    match &cli.command {
        Commands::ApplyRelease { folder } => anyhow::bail!(
            "apply-release changes a system root: recipe --system apply-release {}",
            folder.display()
        ),
        Commands::RecordBase { release } => anyhow::bail!(
            "record-base writes into an image tree: recipe --system --root TREE record-base --release {}",
            release.display()
        ),
        _ => {}
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
        !cli.no_persist_state
            || !matches!(
                &cli.command,
                Commands::Download { .. } | Commands::Build { .. }
            ),
        "download and build keep their result in Recipe's saved state, and --no-persist-state keeps nothing; drop --no-persist-state, or install"
    );
    let machine_events = cli.machine_events;
    let io_start = levitate_recipe::system::process_io();
    let mut execution = levitate_recipe::ExecutionOptions::default();
    execution.target_os = cli.target_os;
    execution.target_arch = cli.target_arch;
    execution.jobs = cli.jobs;
    execution.machine_events = cli.machine_events;
    execution.progress_file = cli.progress_file.clone();
    execution.exact_identity = cli.exact_identity || levitate_recipe::identity::exact_from_env()?;
    execution.execution = cli.execution;
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
    // The release source belongs to the root a system change targets.
    let all_sources = Sources::new(&package_root)
        .with_release(super::system_route::target_root(cli.root.clone()));
    let package_store = PackageStore::new(package_root);
    let lookup = Lookup {
        folders: &recipe_paths,
        sources: (!explicit_collection).then_some(&all_sources),
        purpose: if matches!(
            cli.command,
            Commands::Install { .. }
                | Commands::Update { .. }
                | Commands::Download { .. }
                | Commands::Build { .. }
        ) {
            sources::Purpose::Install
        } else {
            sources::Purpose::Look
        },
        system: false,
    };
    let no_activate = cli.no_activate;
    if let Commands::Install { more, .. } = &cli.command {
        anyhow::ensure!(
            more.is_empty(),
            "several recipes in one install apply to --system; install them one at a time"
        );
    }
    let any_libc = cli.any_libc;
    let any_system = cli.any_system;
    anyhow::ensure!(
        !(any_libc || any_system)
            || matches!(
                &cli.command,
                Commands::Install { .. } | Commands::Update { .. }
            ),
        "--any-libc and --any-system apply to install and update"
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
        effects_root: cli.effects_root.as_deref(),
        no_persist_state: cli.no_persist_state,
        activate_into: (!no_activate).then_some(&package_store),
    };

    match cli.command {
        Commands::Validate { recipe } => {
            let recipe = resolve_recipe_path(&recipe, &lookup)?;
            let supplied = parse_inputs(engine_options.inputs)?;
            let defines = parse_defines(engine_options.defines)?;
            let graph = levitate_recipe::needs::declarations(&recipe, Some(recipes_path))?;
            levitate_recipe::needs::check_defines(&graph, &defines)?;
            let declared =
                levitate_recipe::catalog::entry_of(graph.last().context("empty recipe graph")?);
            let authority = levitate_recipe::needs::report::validate(
                &recipe,
                Some(recipes_path),
                &supplied,
                &engine_options.execution.narrowing,
            )?;
            emit_result(
                &serde_json::json!({"valid":true,"recipe":declared,"authority":authority}),
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
            keep,
            ..
        } => {
            let recipe_path = resolve_recipe_path(&recipe, &lookup)?;
            require_sha256(&recipe_path, sha256.as_deref())?;
            check_libc(
                &recipe_path,
                recipes_path,
                "install",
                &recipe,
                (any_libc, any_system),
                (json_output.as_deref(), json),
            )?;
            // The keep option, remembered per program (section 6.4).
            let name = levitate_recipe::needs::declaration(&recipe_path, Some(recipes_path))?
                .name()
                .to_owned();
            let keep = remember_keep(engine_options.state_dir, &name, keep.choice())?;
            if !no_activate
                && let Some(result) = installed_already(
                    &package_store,
                    &recipe_path,
                    recipes_path,
                    machine_events,
                    keep,
                    &engine_options,
                )?
            {
                emit_result(&result, json_output.as_deref(), json, "Already installed")?;
                return Ok(());
            }
            let engine = create_engine(&recipe_path, &engine_options)?.with_keep(keep);
            // Activation runs inside the install, after the outputs are
            // published and before the material is deleted.
            let key = state_key(&recipe_path, &engine_options)?;
            let activate = |result: &mut serde_json::Value| {
                activate_package(
                    &package_store,
                    &recipe_path,
                    recipes_path,
                    &key,
                    result,
                    None,
                )
            };
            let mut result = engine.install_activating(
                &recipe_path,
                (!no_activate).then_some(&activate as &levitate_recipe::Activate<'_>),
            )?;
            result["producer_io"] = levitate_recipe::system::io_delta(&io_start);
            // Installed: the package is active, or a producer's outputs are
            // published (decision 23).
            let freed = result["freed_bytes"].as_u64().unwrap_or(0);
            finish_run(&mut result, State::Installed, freed, keep);
            emit_result(
                &result,
                json_output.as_deref(),
                json,
                "Installation completed",
            )?;
        }

        Commands::Update { recipe, keep } => {
            anyhow::ensure!(
                !no_activate,
                "update manages active packages; use install --no-activate for producers"
            );
            let installed = installed_package(&package_store, &recipe)?
                .with_context(|| format!("package {} is not installed", recipe.display()))?;
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
            // The keep option, as for install.
            let keep = remember_keep(engine_options.state_dir, &installed.name, keep.choice())?;
            if installed.source.is_some() && recipe_path == installed.recipe {
                let freed = if keep {
                    0
                } else {
                    clean_installed(&recipe_path, recipes_path, &engine_options).0
                };
                let mut result = serde_json::json!({"name": installed.name, "version": installed.version,
                    "operation": "update", "current": true, "profile": package_store.profile_path(),
                    "steps": [], "outputs": {}, "dependencies": []});
                finish_run(&mut result, State::Installed, freed, keep);
                emit_result(&result, json_output.as_deref(), json, "Package is current")?;
                return Ok(());
            }
            check_libc(
                &recipe_path,
                recipes_path,
                "update",
                &PathBuf::from(&installed.name),
                (any_libc, any_system),
                (json_output.as_deref(), json),
            )?;
            let engine = create_engine(&recipe_path, &engine_options)?.with_keep(keep);
            let key = state_key(&recipe_path, &engine_options)?;
            let activate = |result: &mut serde_json::Value| {
                activate_package(
                    &package_store,
                    &recipe_path,
                    recipes_path,
                    &key,
                    result,
                    Some(&installed.name),
                )
            };
            let mut result = engine.install_activating(&recipe_path, Some(&activate))?;
            result["operation"] = "update".into();
            if let Some(source) = &installed.source {
                all_sources.forget(source, &installed.name, Some(&recipe_path));
            }
            let freed = result["freed_bytes"].as_u64().unwrap_or(0);
            finish_run(&mut result, State::Installed, freed, keep);
            emit_result(&result, json_output.as_deref(), json, "Package updated")?;
        }

        Commands::Download { recipe } => {
            let recipe_path = resolve_recipe_path(&recipe, &lookup)?;
            let engine = create_engine(&recipe_path, &engine_options)?;
            let mut result = engine.download(&recipe_path)?;
            let receipt = (!no_activate)
                .then(|| installed_package(&package_store, &recipe))
                .transpose()?
                .flatten();
            let state = if receipt.is_some() {
                State::Installed
            } else {
                let recorded = recorded_state(&recipe_path, &engine, &engine_options)?;
                recorded.state
            };
            // The downloads stay until the program is built, installed or
            // cleaned up.
            finish_run(&mut result, state, 0, true);
            emit_result(&result, json_output.as_deref(), json, "Downloaded")?;
        }

        Commands::Build { recipe, keep } => {
            let recipe_path = resolve_recipe_path(&recipe, &lookup)?;
            let receipt = (!no_activate)
                .then(|| installed_package(&package_store, &recipe))
                .transpose()?
                .flatten();
            // The keep option, as for install. --no-keep deletes the kept
            // material of an installed program at once, before this build
            // keeps its new result (decision 22).
            let name = levitate_recipe::needs::declaration(&recipe_path, Some(recipes_path))?
                .name()
                .to_owned();
            let choice = keep.choice();
            remember_keep(engine_options.state_dir, &name, choice)?;
            let freed = if choice == Some(false) && receipt.is_some() {
                delete_material(&recipe_path, recipes_path, &engine_options)?
            } else {
                0
            };
            let engine = create_engine(&recipe_path, &engine_options)?;
            let mut result = engine.build(&recipe_path)?;
            let state = if receipt.is_some() {
                State::Installed
            } else {
                state_after(&recipe_path, recipes_path, State::Built)?
            };
            // A build keeps its result until it is installed or cleaned up.
            finish_run(&mut result, state, freed, true);
            emit_result(&result, json_output.as_deref(), json, "Built")?;
        }

        Commands::Installed => {
            let result = installed_json(&package_store, &lookup, &engine_options)?;
            emit_result(&result, json_output.as_deref(), json, "Installed packages")?;
        }

        Commands::Updates { upstream, names } => {
            let mut result = updates(&package_store, &lookup)?;
            if upstream {
                result["upstream_checked"] = true.into();
                result["upstream"] = upstream_check(
                    &package_store,
                    &lookup,
                    &engine_options,
                    &names,
                    explicit_collection,
                )?;
            }
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

        Commands::Storage => {
            let storage =
                levitate_recipe::state::storage(engine_options.state_dir, &package_store)?;
            if json_output.is_some() || json {
                emit_result(
                    &serde_json::to_value(&storage)?,
                    json_output.as_deref(),
                    json,
                    "Storage",
                )?;
            } else {
                print_storage(&storage);
            }
        }

        Commands::Remove { recipe, keep } => {
            // The installed files go by the receipt; no recipe code runs.
            let removed = match (!no_activate)
                .then(|| installed_package(&package_store, &recipe))
                .transpose()?
                .flatten()
            {
                Some(installed) => {
                    let removed = package_store.remove(&installed.name)?;
                    if let Some(source) = &removed.source {
                        all_sources.forget(source, &removed.name, None);
                    }
                    Some(removed)
                }
                None => None,
            };
            // The recipe the program was installed from names its material;
            // a program that is not installed is looked up by name.
            let recipe_path = match &removed {
                Some(removed) if removed.recipe.is_file() => Some(removed.recipe.clone()),
                Some(_) => resolve_recipe_path(&recipe, &lookup).ok(),
                None => Some(resolve_recipe_path(&recipe, &lookup)?),
            };
            let mut freed = removed
                .as_ref()
                .and_then(|removed| removed.size_bytes)
                .unwrap_or(0);
            let mut held = false;
            if let Some(recipe_path) = &recipe_path {
                let engine = create_engine(recipe_path, &engine_options)?;
                let graph = levitate_recipe::needs::declarations(recipe_path, Some(recipes_path))?;
                if keep {
                    let on_disk = levitate_recipe::state::on_disk(
                        engine_options.state_dir,
                        engine.destination(),
                        &graph,
                    )?;
                    held = on_disk.downloads + on_disk.build + on_disk.unfinished > 0;
                } else {
                    let material = levitate_recipe::state::cleanup(
                        engine_options.state_dir,
                        engine.destination(),
                        &graph,
                        levitate_recipe::state::Material::All,
                    )?;
                    held = material > 0;
                    freed += material;
                }
            }
            // `recipe remove NAME` forgets the keep option (decision 22).
            let forget = removed
                .as_ref()
                .map(|removed| removed.name.clone())
                .or_else(|| {
                    recipe_path.as_ref().and_then(|path| {
                        levitate_recipe::needs::declaration(path, Some(recipes_path))
                            .ok()
                            .map(|declaration| declaration.name().to_owned())
                    })
                });
            if let Some(name) = forget {
                levitate_recipe::state::set_keep(engine_options.state_dir, &name, false)?;
            }
            anyhow::ensure!(
                removed.is_some() || held,
                "{} is not installed in this profile, and Recipe keeps no downloads or build material of it",
                recipe.display()
            );
            let declared = recipe_path.as_ref().and_then(|path| {
                levitate_recipe::needs::declaration(path, Some(recipes_path)).ok()
            });
            let name = removed
                .as_ref()
                .map(|removed| removed.name.clone())
                .or_else(|| declared.as_ref().map(|d| d.name().to_owned()));
            let version = removed
                .as_ref()
                .map(|removed| removed.version.clone())
                .or_else(|| {
                    declared
                        .as_ref()
                        .and_then(|d| d.version().map(str::to_owned))
                });
            let state = match (&recipe_path, keep) {
                (Some(recipe_path), true) => {
                    let engine = create_engine(recipe_path, &engine_options)?;
                    recorded_state(recipe_path, &engine, &engine_options)?.state
                }
                _ => State::None,
            };
            let mut result = serde_json::json!({"operation": "remove", "name": name,
                "version": version, "steps": [], "outputs": {}, "dependencies": [],
                "profile": package_store.profile_path()});
            finish_run(&mut result, state, freed, keep);
            emit_result(&result, json_output.as_deref(), json, "Removed")?;
        }

        Commands::Cleanup {
            recipe,
            downloads,
            build,
            logs,
        } => {
            use levitate_recipe::state::Material;
            let what = match (downloads, build) {
                (true, _) => Material::Downloads,
                (_, true) => Material::Build,
                _ => Material::All,
            };
            let steps = serde_json::json!([{"step": "cleanup", "status": "done"}]);
            let result = match recipe {
                // The diagnostic logs (decision 26).
                None if logs => {
                    let freed = levitate_recipe::state::cleanup_logs(engine_options.state_dir)?;
                    let mut result = serde_json::json!({"operation": "cleanup", "name": null,
                        "version": null, "steps": steps, "outputs": {}, "dependencies": []});
                    finish_run(&mut result, State::None, freed, false);
                    result["state"] = serde_json::Value::Null;
                    result
                }
                None => {
                    let freed =
                        levitate_recipe::state::cleanup_all(engine_options.state_dir, what)?;
                    let mut result = serde_json::json!({"operation": "cleanup", "name": null,
                        "version": null, "steps": steps, "outputs": {}, "dependencies": []});
                    finish_run(&mut result, State::None, freed, false);
                    result["state"] = serde_json::Value::Null;
                    result
                }
                Some(recipe) => {
                    let recipe_path = resolve_recipe_path(&recipe, &lookup)?;
                    let engine = create_engine(&recipe_path, &engine_options)?;
                    let graph =
                        levitate_recipe::needs::declarations(&recipe_path, Some(recipes_path))?;
                    let main = graph.last().context("empty recipe graph")?;
                    let freed = levitate_recipe::state::cleanup(
                        engine_options.state_dir,
                        engine.destination(),
                        &graph,
                        what,
                    )?;
                    // Then the recipe's own cleanup("manual"), after Recipe's
                    // own deletions (docs/recipe-format.md, section 3.7).
                    let mut result = if main.declared.cleanup {
                        engine.cleanup(&recipe_path)?
                    } else {
                        serde_json::json!({"name": main.name(), "version": main.version()})
                    };
                    result["operation"] = "cleanup".into();
                    result["steps"] = steps;
                    // The builder reads `outputs` from every result.
                    result["outputs"] = serde_json::json!({});
                    result["dependencies"] = serde_json::json!([]);
                    let receipt = (!no_activate)
                        .then(|| installed_package(&package_store, &recipe))
                        .transpose()?
                        .flatten();
                    let state = if receipt.is_some() {
                        State::Installed
                    } else {
                        recorded_state(&recipe_path, &engine, &engine_options)?.state
                    };
                    finish_run(&mut result, state, freed, false);
                    result
                }
            };
            emit_result(&result, json_output.as_deref(), json, "Cleanup completed")?;
        }

        Commands::State { recipe } => {
            let recipe_path = resolve_recipe_path(&recipe, &lookup)?;
            let graph = levitate_recipe::needs::declarations(&recipe_path, Some(recipes_path))?;
            let main = graph.last().context("empty recipe graph")?;
            let receipt = (!no_activate)
                .then(|| installed_package(&package_store, &recipe))
                .transpose()?
                .flatten();
            let engine = create_engine(&recipe_path, &engine_options)?;
            let recorded = recorded_state(&recipe_path, &engine, &engine_options)?;
            let state = if receipt.is_some() {
                State::Installed
            } else {
                recorded.state
            };
            let result = serde_json::json!({
                "schema_version": levitate_recipe::catalog::SCHEMA_VERSION,
                "name": main.name(),
                "version": main.version(),
                "state": state.as_str(),
                "installed_version": receipt.as_ref().map(|receipt| receipt.version.clone()),
                "keep": levitate_recipe::state::keep(engine_options.state_dir, main.name())?,
                "steps": main.declared.steps(),
                "on_disk": recorded.on_disk,
            });
            if json_output.is_some() || json {
                emit_result(&result, json_output.as_deref(), json, "State")?;
            } else {
                println!(
                    "{} {}: {}",
                    main.name(),
                    main.version().unwrap_or("?"),
                    state.as_str()
                );
            }
        }

        Commands::List => list_recipes(&lookup, &package_store, &engine_options, list_json)?,

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
            show_info(&recipe_path, Some(recipes_path), list_json)?;
        }

        Commands::Runtime { .. } => unreachable!("handled before the engine is set up"),
        Commands::ApplyRelease { .. } | Commands::RecordBase { .. } => {
            unreachable!("system commands are handled by the system route")
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
                "Copy the SHA-256 into the recipe's SOURCES entry:".dimmed()
            );
            println!("  {}", format!("sha256: \"{}\",", hashes.sha256).green());
        }
    }

    Ok(())
}

/// Complete a finished run's result (docs/recipe-format.md, section 8.6):
/// `schema_version`, the program's `state` after the run, `activation`
/// (null when nothing was activated), `kept` (whether the downloads and build
/// material stay) and `freed_bytes`.
fn finish_run(result: &mut serde_json::Value, state: State, freed: u64, kept: bool) {
    result["schema_version"] = levitate_recipe::catalog::SCHEMA_VERSION.into();
    result["state"] = state.as_str().into();
    if result.get("activation").is_none() {
        result["activation"] = serde_json::Value::Null;
    }
    result["kept"] = kept.into();
    result["freed_bytes"] = freed.into();
}

/// The keep option of a program for this run: `choice` (`--keep`,
/// `--no-keep`), remembered from now on, or the remembered one.
fn remember_keep(state_dir: Option<&Path>, name: &str, choice: Option<bool>) -> Result<bool> {
    match choice {
        Some(on) => {
            levitate_recipe::state::set_keep(state_dir, name, on)?;
            Ok(on)
        }
        None => levitate_recipe::state::keep(state_dir, name),
    }
}

/// The cleanup step of an install that found the program installed: delete
/// its material, and report a run that holds it instead of failing (the
/// program is installed; `recipe cleanup` deletes the rest later). Returns
/// the bytes freed and the step's status.
fn clean_installed(
    recipe: &Path,
    search: &Path,
    options: &EngineOptions<'_>,
) -> (u64, &'static str) {
    match delete_material(recipe, search, options) {
        Ok(freed) => (freed, "done"),
        Err(error) => {
            eprintln!("recipe: warning: its material is kept for now: {error:#}");
            (0, "failed")
        }
    }
}

/// The state key a run of `recipe` with these options records
/// (`levitate_recipe::state::key`), kept in the receipt of what it installs.
fn state_key(recipe: &Path, options: &EngineOptions<'_>) -> Result<String> {
    let graph = levitate_recipe::needs::declarations(recipe, options.recipes_path)?;
    let defines = parse_defines(options.defines)?;
    let prefix = package_prefix(options.activate_into, recipe, options.recipes_path)?;
    levitate_recipe::state::key(
        &graph,
        &defines,
        prefix.as_deref(),
        &options.execution.host_libc,
    )
}

/// Delete the downloads and build material of `recipe` (and of the
/// dependency runs made for it) at the destination its runs use. Returns the
/// bytes freed.
fn delete_material(recipe: &Path, search: &Path, options: &EngineOptions<'_>) -> Result<u64> {
    let engine = create_engine(recipe, options)?;
    let graph = levitate_recipe::needs::declarations(recipe, Some(search))?;
    levitate_recipe::state::cleanup(
        options.state_dir,
        engine.destination(),
        &graph,
        levitate_recipe::state::Material::All,
    )
}

/// `recipe storage` for a terminal: every kind with its size, what holds
/// it, and the command that removes it.
fn print_storage(storage: &levitate_recipe::state::Storage) {
    let size = |bytes: u64| {
        let mib = bytes as f64 / (1024.0 * 1024.0);
        if mib >= 1024.0 {
            format!("{:.1} GiB", mib / 1024.0)
        } else {
            format!("{mib:.1} MiB")
        }
    };
    println!("Recipe holds {} in all.", size(storage.total_bytes));
    for kind in &storage.kinds {
        let label = match kind.kind {
            "installed" => "Installed programs",
            "downloaded" => "Downloads of programs not installed",
            "built" => "Build material and results not installed",
            "kept" => "Material kept by choice (--keep)",
            "unfinished" => "Unfinished runs",
            "build_image" => "Build image",
            _ => "Diagnostic logs",
        };
        println!("\n{label}: {}", size(kind.bytes));
        for program in &kind.programs {
            println!("  {}: {}", program.name, size(program.bytes));
        }
        if let Some(path) = &kind.path {
            println!("  in {}", path.display());
        }
        match kind.remove {
            Some(command) if kind.bytes > 0 => println!("  removed by: {command}"),
            None if kind.bytes > 0 => {
                println!("  installed with the system; not removed by Recipe")
            }
            _ => {}
        }
    }
}

/// The state after a build: built (the result is kept), or none for a
/// `RERUN` recipe, whose result is never kept.
fn state_after(recipe: &Path, search: &Path, built: State) -> Result<State> {
    let declaration = levitate_recipe::needs::declaration(recipe, Some(search))?;
    Ok(if declaration.declared.rerun {
        State::None
    } else {
        built
    })
}

/// What Recipe's records say of `recipe` at the engine's destination, with
/// the same defines and `PREFIX` a run with these options has.
fn recorded_state(
    recipe: &Path,
    engine: &RecipeEngine,
    options: &EngineOptions<'_>,
) -> Result<levitate_recipe::state::Recorded> {
    let graph = levitate_recipe::needs::declarations(recipe, options.recipes_path)?;
    let defines = parse_defines(options.defines)?;
    let prefix = package_prefix(options.activate_into, recipe, options.recipes_path)?;
    levitate_recipe::state::recorded(
        options.state_dir,
        engine.destination(),
        &graph,
        &defines,
        prefix.as_deref(),
        &options.execution.host_libc,
    )
}

/// `recipe install` of a program already installed from this very recipe
/// file at this version, with its files intact: every step is `already`,
/// nothing runs. Like every install it ends with the cleanup step unless the
/// program keeps its material: what a `recipe download`, a `recipe build` or
/// an earlier `--keep` left is deleted (with `--no-keep`, decision 22). None
/// when something is missing (another recipe file or version, a damaged
/// package), and the install goes ahead.
fn installed_already(
    store: &PackageStore,
    recipe: &Path,
    search: &Path,
    machine_events: bool,
    keep: bool,
    options: &EngineOptions<'_>,
) -> Result<Option<serde_json::Value>> {
    use sha2::{Digest, Sha256};
    let declaration = levitate_recipe::needs::declaration(recipe, Some(search))?;
    if !declaration.needs.outputs.contains_key("package") {
        return Ok(None);
    }
    let Some(receipt) = store
        .list()?
        .into_iter()
        .find(|package| package.name == declaration.name())
    else {
        return Ok(None);
    };
    let digest = hex::encode(Sha256::digest(
        std::fs::read(recipe).with_context(|| format!("cannot read {}", recipe.display()))?,
    ));
    // The same recipe file and version, built with the same state key (its
    // base, pins, modules, defines, PREFIX, HOST_LIBC, dependencies).
    let key = state_key(recipe, options)?;
    if receipt.sha256.as_deref() != Some(digest.as_str())
        || Some(receipt.version.as_str()) != declaration.version()
        || receipt.state_key.as_deref() != Some(key.as_str())
        || store.verify(&receipt.name).is_err()
    {
        return Ok(None);
    }
    let (freed, cleanup) = if keep {
        (0, "skipped")
    } else {
        clean_installed(recipe, search, options)
    };
    let has = declaration.declared.steps();
    let steps: Vec<serde_json::Value> = ["download", "build", "install", "cleanup"]
        .into_iter()
        .map(|step| {
            let status = match step {
                _ if !has.contains(&step) => "none",
                "cleanup" => cleanup,
                _ => "already",
            };
            serde_json::json!({"step": step, "status": status})
        })
        .collect();
    if machine_events {
        eprintln!(
            "{}",
            serde_json::json!({"event": "plan", "recipe": receipt.name, "steps": []})
        );
        for step in &steps {
            eprintln!(
                "{}",
                serde_json::json!({"event": "step", "recipe": receipt.name, "step": step["step"],
                    "status": step["status"], "msg": "installed from this recipe already"})
            );
        }
    }
    let profile = store.profile_path();
    let mut result = serde_json::json!({
        "operation": "install",
        "name": receipt.name,
        "version": receipt.version,
        "steps": steps,
        "outputs": {},
        "activation": {"name": receipt.name, "version": receipt.version,
            "profile": profile, "bin": profile.join("bin")},
        "dependencies": [],
    });
    finish_run(&mut result, State::Installed, freed, keep);
    Ok(Some(result))
}

/// `recipe installed --json` (docs/recipe-format.md, section 8.2): each
/// receipt with its state, the keep option, what it holds beside its files
/// (`on_disk`, from Recipe's records at its recipe's destination) and the
/// offline "Update" mark (decision 25).
fn installed_json(
    store: &PackageStore,
    lookup: &Lookup,
    options: &EngineOptions<'_>,
) -> Result<serde_json::Value> {
    let marks: std::collections::BTreeMap<String, serde_json::Value> =
        updates(store, lookup)?["updates"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|update| {
                Some((
                    update["name"].as_str()?.to_owned(),
                    serde_json::json!({"to": update["to"], "reason": update["reason"],
                    "source": update["source"]}),
                ))
            })
            .collect();
    let mut packages = Vec::new();
    for package in store.list()? {
        let on_disk = held_beside(&package.recipe, options).unwrap_or_default();
        let mut entry = serde_json::to_value(&package)?;
        entry["state"] = State::Installed.as_str().into();
        entry["keep"] = levitate_recipe::state::keep(options.state_dir, &package.name)?.into();
        entry["on_disk"] = serde_json::to_value(on_disk)?;
        entry["update"] = marks
            .get(&package.name)
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        packages.push(entry);
    }
    Ok(serde_json::json!({
        "schema_version": levitate_recipe::catalog::SCHEMA_VERSION,
        "packages": packages,
        "profile": store.profile_path(),
    }))
}

/// The destination a run of `recipe` with these options uses.
fn destination_of(recipe: &Path, options: &EngineOptions<'_>) -> Result<PathBuf> {
    match options.build_dir {
        Some(dir) => Ok(dir.to_owned()),
        None => levitate_recipe::default_destination(options.state_dir, recipe),
    }
}

/// What the recipe file `recipe` holds in Recipe's records beside an
/// installed program; None when the recipe cannot be read any more.
fn held_beside(
    recipe: &Path,
    options: &EngineOptions<'_>,
) -> Option<levitate_recipe::state::OnDisk> {
    if !recipe.is_file() {
        return None;
    }
    let destination = destination_of(recipe, options).ok()?;
    if !levitate_recipe::state::keeps_anything(options.state_dir, recipe, &destination) {
        return Some(Default::default());
    }
    let graph = levitate_recipe::needs::declarations(recipe, options.recipes_path).ok()?;
    levitate_recipe::state::on_disk(options.state_dir, &destination, &graph).ok()
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

/// Before anything is acquired: compare the C library and the systems the
/// recipe (and its runtime dependencies) declare with this system's. A
/// mismatch ends the command with the facts and the flag that installs anyway,
/// and a JSON failure record when the caller asked for JSON; with that flag
/// (`--any-libc`, `--any-system`) it is one note line.
fn check_libc(
    recipe: &Path,
    search: &Path,
    verb: &str,
    requested: &Path,
    (any_libc, any_system): (bool, bool),
    (json_output, json): (Option<&Path>, bool),
) -> Result<()> {
    use levitate_recipe::clibrary::Kind;
    let host = levitate_recipe::clibrary::detect();
    let requested = requested.to_string_lossy();
    let command = levitate_recipe::clibrary::command_for(
        &std::env::var(SYSTEM_PREFIX_ENV).unwrap_or_default(),
        verb,
        &requested,
    );
    let verdict = levitate_recipe::clibrary::check(recipe, Some(search), &host, &command)?;
    for note in &verdict.notes {
        eprintln!("{note}");
    }
    let (allowed, refused): (Vec<_>, Vec<_>) =
        verdict.mismatches.into_iter().partition(|m| match m.kind {
            Kind::Libc => any_libc,
            Kind::System => any_system,
        });
    for mismatch in &allowed {
        eprintln!("{}", mismatch.note());
    }
    if let Some(mismatch) = refused.into_iter().next() {
        if json_output.is_some() || json {
            emit_result(&mismatch.json(), json_output, json, "Not installed")?;
        }
        return Err(mismatch.into());
    }
    Ok(())
}

/// Set by a system change for the producer it starts: the options that go
/// before the command in the line a refusal suggests (`--system --root /mnt`).
pub(crate) const SYSTEM_PREFIX_ENV: &str = "RECIPE_SYSTEM_COMMAND_PREFIX";

/// Activate the published package of `result` in `store`, recording the
/// state key `key` in its receipt. Whatever happens, the publication does
/// not stay: on success its files are in the store, and on failure the kept
/// result publishes it again on the next install.
fn activate_package(
    store: &PackageStore,
    recipe: &Path,
    search: &Path,
    key: &str,
    result: &mut serde_json::Value,
    updating: Option<&str>,
) -> Result<()> {
    let activated = activate_published(store, recipe, search, key, result, updating);
    if activated.is_err()
        && let Some(output) = result["outputs"]["package"].as_str()
        && let Some(publication) = Path::new(output).parent()
        && publication
            .parent()
            .and_then(Path::file_name)
            .is_some_and(|name| name == "exports")
    {
        levitate_recipe::fsx::make_removable(publication);
        if let Err(error) = std::fs::remove_dir_all(publication) {
            eprintln!(
                "recipe: warning: cannot delete the publication {}: {error}",
                publication.display()
            );
        }
    }
    activated
}

fn activate_published(
    store: &PackageStore,
    recipe: &Path,
    search: &Path,
    key: &str,
    result: &mut serde_json::Value,
    updating: Option<&str>,
) -> Result<()> {
    let output = result
        .get("outputs")
        .and_then(|outputs| outputs.get("package"));
    if output.is_none() && updating.is_none() {
        return Ok(());
    }
    let output = output
        .and_then(|value| value.as_str())
        .context("user-managed recipe must publish a directory output named package")?
        .to_owned();
    let name = result
        .get("name")
        .and_then(|value| value.as_str())
        .context("a package output's result names the recipe")?;
    let version = result
        .get("version")
        .and_then(|value| value.as_str())
        .context("a package output needs PACKAGE.version")?;
    if let Some(expected) = updating {
        anyhow::ensure!(
            name == expected,
            "update recipe names {name}, expected {expected}"
        );
    }
    // What was carried was told this folder; it must be where it lands.
    if let Some(prefix) = result.get("prefix").and_then(|value| value.as_str()) {
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
        Some(key),
        Path::new(&output),
        updating.is_some(),
    )?;
    // The store moved the package's files out of the publication: what is
    // left of it (its directories) goes, and the output is where the package
    // now lives.
    let publication = Path::new(&output);
    levitate_recipe::fsx::make_removable(publication);
    if let Err(error) = std::fs::remove_dir_all(publication) {
        eprintln!(
            "recipe: warning: cannot delete the publication {}: {error}",
            publication.display()
        );
    }
    if let Some(parent) = publication.parent() {
        let _ = std::fs::remove_dir(parent);
    }
    result["outputs"]["package"] = store
        .package_prefix(&installed.name)?
        .to_string_lossy()
        .into_owned()
        .into();
    let profile = store.profile_path();
    result["activation"] = serde_json::json!({"name": installed.name, "version": installed.version,
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
    /// `--effects-root`: the live system root input `root`.
    effects_root: Option<&'a Path>,
    no_persist_state: bool,
    /// The store a package output will be activated in; None with
    /// --no-activate. It decides PREFIX.
    activate_into: Option<&'a PackageStore>,
}

fn create_engine(recipe: &Path, options: &EngineOptions<'_>) -> Result<RecipeEngine> {
    let EngineOptions {
        build_dir,
        state_dir,
        execution,
        recipes_path,
        defines,
        inputs,
        effects_root,
        no_persist_state,
        activate_into,
    } = options;
    let mut execution = execution.clone();
    execution.package_prefix = package_prefix(*activate_into, recipe, *recipes_path)?;
    execution.validate()?;
    let engine = match build_dir {
        Some(dir) => {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("Failed to create build directory: {}", dir.display()))?;
            RecipeEngine::new(dir.to_path_buf())
        }
        None if *no_persist_state => RecipeEngine::with_temporary_destination()?,
        None => RecipeEngine::new(levitate_recipe::default_destination(*state_dir, recipe)?),
    };
    // The Ginux launcher is resolved when a recipe of the run needs Ginux;
    // a run entirely on the host needs none.
    let mut engine = engine
        .with_state_persistence(!*no_persist_state)
        .with_state_dir(state_dir.map(Path::to_owned))
        .with_execution_options(execution);

    if let Some(rp) = recipes_path {
        engine = engine.with_recipes_path(rp.to_path_buf());
    }

    for (key, value) in parse_defines(defines)? {
        engine.add_define(key, value);
    }

    for (name, path) in parse_inputs(inputs)? {
        engine.add_input(name, path);
    }
    if let Some(root) = effects_root {
        engine.add_system_root(std::path::absolute(root)?);
    }

    Ok(engine)
}

/// `PREFIX` for a recipe whose package output will be activated in `store`:
/// the folder the package will live in, named by the recipe's file name
/// (docs/recipe-format.md, decision 3).
fn package_prefix(
    store: Option<&PackageStore>,
    recipe: &Path,
    search: Option<&Path>,
) -> Result<Option<String>> {
    let Some(store) = store else {
        return Ok(None);
    };
    let declaration = levitate_recipe::needs::declaration(recipe, search)?;
    let Ok(prefix) = store.package_prefix(declaration.name()) else {
        return Ok(None); // activation refuses the name with its own message
    };
    let prefix = prefix
        .to_str()
        .with_context(|| format!("package folder {} is not UTF-8", prefix.display()))?;
    Ok(Some(prefix.to_owned()))
}

pub(super) fn resolve_recipe_path(recipe: &Path, lookup: &Lookup) -> Result<PathBuf> {
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
                    sources::Kind::Release(release) => Some(release.path.clone()),
                    sources::Kind::Remote { .. } => {
                        if let Some(path) = release_instead(all, &from, name, lookup)? {
                            return Ok(path);
                        }
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

    // A system change takes the release's recipe of a name it carries,
    // whatever any other source holds.
    let name = text.strip_suffix(".rhai").unwrap_or(&text);
    let release = lookup.sources.and_then(Sources::release);
    if lookup.system
        && let Some(path) = release.and_then(|release| release.carries(name))
    {
        return Ok(path);
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

    // A bare name the folders do not hold means the release's recipe, else
    // the one source that has it.
    if let Some(path) = release.and_then(|release| release.carries(name)) {
        return Ok(path);
    }
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
                None => {
                    let from = all.get(&found.source)?;
                    if let Some(path) = release_instead(all, &from, name, lookup)? {
                        return Ok(path);
                    }
                    all.fetch(&from, name, lookup.purpose)
                }
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

/// For a system change on a root with a release source, before a remote
/// source's recipe is fetched: the release's recipe of the same name instead
/// (said on stderr), or a refusal for a system group the release does not
/// carry. None when the remote recipe is the one to fetch.
fn release_instead(
    all: &Sources,
    from: &sources::Source,
    name: &str,
    lookup: &Lookup,
) -> Result<Option<PathBuf>> {
    let Some(release) = all.release().filter(|_| lookup.system) else {
        return Ok(None);
    };
    if let Some(path) = release.carries(name) {
        eprintln!(
            "{name} comes with this system's release, {}: installing {}:{name}, not {}:{name}.",
            release.title,
            sources::RELEASE,
            from.name
        );
        return Ok(Some(path));
    }
    if let Some(entry) = all.published_entry(from, name)
        && let Some(reason) = sources::release_refusal(&entry, release)
    {
        anyhow::bail!("{reason} Nothing was installed from {}.", from.name);
    }
    Ok(None)
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
    let release = lookup.sources.and_then(Sources::release);
    for installed in store.list()? {
        // A package the release carries is at the release's version by
        // definition: only a newer system release changes it.
        if release.is_some_and(|release| release.carries(&installed.name).is_some()) {
            continue;
        }
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
        }));
    }
    Ok(serde_json::json!({
        "schema_version": levitate_recipe::catalog::SCHEMA_VERSION,
        "upstream_checked": false,
        "updates": found,
        "problems": problems,
    }))
}

/// `recipe updates --upstream` (docs/recipe-format.md, sections 2.9 and
/// 8.3): for each installed program, or each named recipe, ask upstream
/// which version is newest. This is the only place Recipe asks: GitHub itself
/// for `UPSTREAM`, the recipe's `latest()` on the host otherwise. A question
/// that fails is the row's `error`; nothing is changed or installed.
fn upstream_check(
    store: &PackageStore,
    lookup: &Lookup,
    options: &EngineOptions<'_>,
    names: &[String],
    explicit_collection: bool,
) -> Result<serde_json::Value> {
    let mut targets: Vec<(String, Option<InstalledPackage>)> = Vec::new();
    if names.is_empty() {
        for installed in store.list()? {
            targets.push((installed.name.clone(), Some(installed)));
        }
    } else {
        for name in names {
            let installed = installed_package(store, Path::new(name))?;
            targets.push((name.clone(), installed));
        }
    }
    let mut listings = std::collections::BTreeMap::new();
    let mut rows = Vec::new();
    for (name, installed) in targets {
        let row = |recipe_version: Option<&str>,
                   latest: Option<&str>,
                   how: Option<&str>,
                   error: Option<String>| {
            serde_json::json!({
                "name": name,
                "installed": installed.as_ref().map(|installed| installed.version.as_str()),
                "recipe_version": recipe_version,
                "latest": latest,
                "newer": match (latest, recipe_version) {
                    (Some(latest), Some(version)) => levitate_recipe::updates::is_newer(latest, version),
                    _ => false,
                },
                "how": how,
                "error": error,
            })
        };
        // The recipe file, resolved the way `recipe update` resolves it.
        let path_of = || -> Result<PathBuf> {
            match &installed {
                Some(installed) => {
                    let named = match &installed.source {
                        Some(source) if !explicit_collection => {
                            PathBuf::from(format!("{source}:{}", installed.name))
                        }
                        _ => PathBuf::from(&installed.name),
                    };
                    resolve_recipe_path(&named, lookup).or_else(|error| {
                        if explicit_collection {
                            Err(error)
                        } else {
                            resolve_recipe_path(&installed.recipe, lookup)
                        }
                    })
                }
                None => resolve_recipe_path(Path::new(&name), lookup),
            }
        };
        let found: Result<(serde_json::Value, Option<PathBuf>)> = match &installed {
            Some(installed) => {
                update_candidate(installed, lookup, &mut listings).and_then(|entry| {
                    entry.map(|entry| (entry, None)).ok_or_else(|| {
                        anyhow::anyhow!(
                            "no recipe for it is in the collection or the sources any more"
                        )
                    })
                })
            }
            None => path_of().and_then(|path| {
                let entry = levitate_recipe::catalog::entry(&path, path.parent())?;
                Ok((entry, Some(path)))
            }),
        };
        let (entry, path) = match found {
            Ok(found) => found,
            Err(error) => {
                rows.push(row(None, None, None, Some(format!("{error:#}"))));
                continue;
            }
        };
        let version = entry["version"].as_str().unwrap_or_default().to_owned();
        let version = Some(version.as_str()).filter(|version| !version.is_empty());
        let upstream = &entry["upstream"];
        let asked: Result<(String, &str), String> = if let Some(repo) = upstream["github"].as_str()
        {
            levitate_recipe::updates::github_newest(repo, upstream["prefix"].as_str())
                .map(|latest| (latest, "github"))
        } else if upstream["latest"] == true {
            path.map(Ok)
                .unwrap_or_else(path_of)
                .and_then(|path| create_engine(&path, options)?.latest(&path))
                .map(|latest| (latest.trim().to_owned(), "latest()"))
                .map_err(|error| format!("{error:#}"))
        } else {
            Err("the recipe says nowhere where its newest version is".to_owned())
        };
        rows.push(match asked {
            Ok((latest, how)) => row(version, Some(&latest), Some(how), None),
            Err(error) => row(version, None, None, Some(error)),
        });
    }
    Ok(serde_json::Value::Array(rows))
}

fn print_updates(result: &serde_json::Value) {
    let found = result["updates"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default();
    if found.is_empty() {
        bin_support::info("Everything installed is at its recipe's version");
    }
    if result["upstream_checked"] == true {
        for row in result["upstream"].as_array().into_iter().flatten() {
            let name = row["name"].as_str().unwrap_or("?");
            if let Some(error) = row["error"].as_str() {
                println!("  {name}: upstream could not be asked: {error}");
            } else if row["newer"] == true {
                println!(
                    "  {name}: upstream has {}, this recipe installs {}",
                    row["latest"].as_str().unwrap_or("?"),
                    row["recipe_version"].as_str().unwrap_or("?"),
                );
            } else {
                println!(
                    "  {name}: nothing newer upstream ({})",
                    row["latest"].as_str().unwrap_or("?")
                );
            }
        }
    }
    for update in found {
        let (name, from, to) = (
            update["name"].as_str().unwrap_or("?"),
            update["from"].as_str().unwrap_or("?"),
            update["to"].as_str().unwrap_or("?"),
        );
        if update["reason"] == "recipe" {
            println!("  {name} {from}: its source publishes another recipe");
        } else {
            println!("  {name} {from} -> {to}");
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

fn list_recipes(
    lookup: &Lookup,
    store: &PackageStore,
    options: &EngineOptions<'_>,
    json: bool,
) -> Result<()> {
    use owo_colors::OwoColorize;

    // The recipe folder first, as before sources existed.
    let folder_name = |_: usize| sources::LOCAL;
    let mut names = std::collections::BTreeSet::new();
    let mut about = Vec::new();
    let mut recipes = Vec::new();
    let mut problems = Vec::new();
    let (found, unreadable) = match lookup.sources {
        Some(all) => all.all(),
        None => (Vec::new(), Vec::new()),
    };
    // The release source first: a front end that takes the first recipe of a
    // name finds the release's.
    let (release, others): (Vec<_>, Vec<_>) = found
        .into_iter()
        .partition(|source| matches!(source.kind, sources::Kind::Release(_)));
    if let Some(all) = lookup.sources {
        for source in &release {
            let listing = all.listing(source);
            about.push(listing.about);
            recipes.extend(listing.recipes);
            problems.extend(listing.problems.iter().map(problem_json));
        }
    }
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
    let remote: std::collections::BTreeSet<String> = others
        .iter()
        .filter(|source| matches!(source.kind, sources::Kind::Remote { .. }))
        .map(|source| source.name.clone())
        .collect();
    if let Some(all) = lookup.sources {
        problems.extend(unreadable.iter().map(problem_json));
        for source in &others {
            let listing = all.listing(source);
            about.push(listing.about);
            recipes.extend(listing.recipes);
            problems.extend(listing.problems.iter().map(problem_json));
        }
    }

    // The state on this system (docs/recipe-format.md, section 6.2): the
    // receipt, else Recipe's records at the recipe's default destination. A
    // recipe of a source that is not fetched yet has no records.
    let installed = store.list().unwrap_or_default();
    let keeping = levitate_recipe::state::keeping(options.state_dir).unwrap_or_default();
    for recipe in &mut recipes {
        let receipt = installed
            .iter()
            .find(|package| recipe["name"].as_str() == Some(package.name.as_str()));
        let state = match (receipt, recipe["path"].as_str()) {
            (Some(_), _) => State::Installed,
            (None, Some(path)) => recorded_in_list(Path::new(path), store, options),
            (None, None) => State::None,
        };
        recipe["state"] = state.as_str().into();
        recipe["installed_version"] = receipt.map(|package| package.version.clone()).into();
        recipe["keep"] = recipe["name"]
            .as_str()
            .is_some_and(|name| keeping.contains(name))
            .into();
    }
    if json {
        let host = levitate_recipe::clibrary::detect();
        let release = lookup.sources.and_then(Sources::release);
        for recipe in &mut recipes {
            levitate_recipe::clibrary::annotate(recipe, &host);
            // On a root with a release source, a remote source's system
            // group, or a recipe the release carries, is not installed from
            // there; no flag changes that.
            let from_remote = recipe["source"]
                .as_str()
                .is_some_and(|source| remote.contains(source));
            if let Some(release) = release.filter(|_| from_remote)
                && let Some(reason) = sources::release_refusal(recipe, release)
            {
                recipe["runs_here"] = false.into();
                recipe["not_runnable_override"] = serde_json::Value::Null;
                recipe["not_runnable_reason"] = reason.into();
            }
        }
        let result = serde_json::json!({
            "schema_version": levitate_recipe::catalog::SCHEMA_VERSION,
            "host_libc": host.libc,
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

/// A listed recipe's state from Recipe's records, without a run's defines:
/// none when nothing is kept for it or it cannot be read.
fn recorded_in_list(recipe: &Path, store: &PackageStore, options: &EngineOptions<'_>) -> State {
    let Ok(destination) = levitate_recipe::default_destination(options.state_dir, recipe) else {
        return State::None;
    };
    if !levitate_recipe::state::keeps_anything(options.state_dir, recipe, &destination) {
        return State::None;
    }
    let recorded = (|| -> Result<State> {
        let graph = levitate_recipe::needs::declarations(recipe, recipe.parent())?;
        let prefix = package_prefix(Some(store), recipe, recipe.parent())?;
        Ok(levitate_recipe::state::recorded(
            options.state_dir,
            &destination,
            &graph,
            &[],
            prefix.as_deref(),
            &options.execution.host_libc,
        )?
        .state)
    })();
    recorded.unwrap_or(State::None)
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
                let place = match about["kind"].as_str() {
                    Some("remote") => format!("{} ({})", text("files"), text("branch")),
                    Some("release") => format!("{}, {}", text("title"), text("path")),
                    _ => text("path"),
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
                    None if about["kind"] == "folder" || about["kind"] == "release" => count,
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

fn show_info(recipe_path: &Path, search_path: Option<&Path>, json: bool) -> Result<()> {
    use owo_colors::OwoColorize;

    let declaration = levitate_recipe::needs::declaration(recipe_path, search_path)?;
    if json {
        let entry = levitate_recipe::catalog::entry_of(&declaration);
        writeln!(std::io::stdout(), "{}", serde_json::to_string(&entry)?)?;
        return Ok(());
    }
    let declared = &declaration.declared;
    let package = &declared.package;
    let row = |label: &str, value: &str| println!("{:<12} {value}", label.bold());

    println!("{:<12} {}", "Name:".bold(), package.name.bold().cyan());
    println!(
        "{:<12} {}",
        "Version:".bold(),
        package.version.as_deref().unwrap_or("?").green()
    );
    if let Some(desc) = &package.description {
        row("Description:", desc);
    }
    println!(
        "{:<12} {}",
        "Recipe:".bold(),
        recipe_path.display().to_string().dimmed()
    );
    row("Steps:", &declared.steps().join(", "));
    if !declared.functions.is_empty() {
        row("Functions:", &declared.functions.join(", "));
    }
    let needs = &declaration.needs;
    let outputs: Vec<String> = needs
        .outputs
        .iter()
        .map(|(name, kind)| format!("{name} ({})", kind.as_str()))
        .collect();
    row(
        "Outputs:",
        &if outputs.is_empty() {
            "none (a base its children complete)".to_owned()
        } else {
            outputs.join(", ")
        },
    );
    if !needs.inputs.is_empty() {
        let inputs: Vec<String> = needs
            .inputs
            .iter()
            .map(|(name, need)| {
                format!(
                    "{name} ({}{})",
                    need.kind.as_str(),
                    if need.optional { ", optional" } else { "" }
                )
            })
            .collect();
        row("Inputs:", &inputs.join(", "));
    }
    if !needs.defines.is_empty() {
        let defines: Vec<String> = needs
            .defines
            .iter()
            .map(|(name, default)| match default {
                Some(default) => format!("{name}={default:?}"),
                None => format!("{name} (required)"),
            })
            .collect();
        row("Defines:", &defines.join(", "));
    }
    for (name, source) in &declared.sources {
        let origin = match &source.origin {
            levitate_recipe::declarations::Origin::Url { url, .. } => url.clone(),
            levitate_recipe::declarations::Origin::Git { git, commit, .. } => {
                format!("{git} at {commit}")
            }
        };
        let libc = source
            .libc
            .map(|libc| format!(" ({} only)", libc.as_str()))
            .unwrap_or_default();
        row("Source:", &format!("{name}: {origin}{libc}"));
    }
    if !declared.files.is_empty() {
        let count = declared.files.len();
        row(
            "Files:",
            &format!("{count} entr{}", if count == 1 { "y" } else { "ies" }),
        );
    }
    if let Some(pins) = &declared.pins {
        let sections: Vec<String> = pins
            .sections
            .iter()
            .map(|section| {
                format!(
                    "{} packages for {} ({}, {} bytes)",
                    section.packages.len(),
                    section.libc.as_str(),
                    section.format.as_str(),
                    section.bytes()
                )
            })
            .collect();
        row("Packages:", &sections.join("; "));
    }
    if !declared.accounts.is_empty() {
        let accounts: Vec<String> = declared
            .accounts
            .iter()
            .map(|(libc, lines)| format!("{libc}: {}", lines.join(", ")))
            .collect();
        row("Accounts:", &accounts.join("; "));
    }
    let deps: Vec<String> = declaration
        .dependencies
        .iter()
        .map(|dependency| {
            if dependency.build_only {
                format!("{} (build)", dependency.name)
            } else {
                dependency.name.clone()
            }
        })
        .collect();
    if !deps.is_empty() {
        row("Needs:", &deps.join(", "));
    }
    match (&declared.upstream, declared.functions.contains(&"latest")) {
        (Some(upstream), _) => row("Upstream:", &format!("github.com/{}", upstream.github)),
        (None, true) => row("Upstream:", "asked through latest()"),
        (None, false) => {}
    }
    if declared.rerun {
        row("Reruns:", "every time it is asked for (RERUN)");
    }
    if let Some(about) = &declared.about
        && let Some(declared) = levitate_recipe::clibrary::declared(about)?
    {
        if !declared.libc.is_empty() {
            let host = levitate_recipe::clibrary::detect();
            let build = declared.libc.join(" or ");
            let carried = if declared.carried {
                format!(", and its package brings its own {build} and loader")
            } else {
                String::new()
            };
            println!(
                "{:<12} its build is linked against {build}{carried}; this system's C library is {} ({})",
                "C library:".bold(),
                host.libc,
                host.found
            );
        }
        if !declared.systems.is_empty() {
            let host = levitate_recipe::clibrary::detect();
            println!(
                "{:<12} made for {} (os-release ID); this system is {} ({})",
                "Systems:".bold(),
                declared.systems.join(", "),
                host.os_id.as_deref().unwrap_or("unknown"),
                host.os_found
            );
        }
    }
    // Where `--execution auto` runs it, from its source alone.
    if let Ok(eligibility) = levitate_recipe::execution::eligibility(recipe_path, search_path) {
        let minimal = levitate_recipe::needs::declarations(recipe_path, search_path)
            .ok()
            .and_then(|graph| graph.last().map(|d| d.needs.runtime))
            == Some(levitate_recipe::runtime::Image::Minimal);
        let runs = match &eligibility.blocker {
            None => "on the host (it starts no program)".to_owned(),
            Some(blocker) if minimal => {
                format!("in Ginux, in the minimal runtime image (it {blocker})")
            }
            Some(blocker) => format!("in Ginux (it {blocker})"),
        };
        row("Runs:", &runs);
    }

    Ok(())
}

fn parse_defines(values: &[String]) -> Result<Vec<(String, String)>> {
    values
        .iter()
        .map(|define| {
            let (key, value) = define.split_once('=').with_context(|| {
                format!("Invalid --define format: '{define}' (expected KEY=VALUE)")
            })?;
            Ok((key.to_owned(), value.to_owned()))
        })
        .collect()
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
