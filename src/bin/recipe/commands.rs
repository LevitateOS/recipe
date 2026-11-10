use anyhow::{Context, Result};
use levitate_recipe::packages::{InstalledPackage, PackageStore};
use levitate_recipe::{AutoFixConfig, GinuxLauncher, RecipeEngine, bin_support};
use std::io::{IsTerminal, Write};
use std::path::Path;
use std::path::PathBuf;

use super::{
    cli::{Cli, Commands},
    metadata::{RecipeMetadata, enumerate_recipes},
};

pub(crate) fn execute(cli: Cli) -> Result<()> {
    let mut execution = levitate_recipe::ExecutionOptions::default();
    execution.target_os = cli.target_os;
    execution.target_arch = cli.target_arch;
    execution.jobs = cli.jobs;
    execution.machine_events = cli.machine_events;
    execution.force_acquire = cli.force_acquire;
    execution.force_build = cli.force_build;
    execution.force_install = cli.force_install;
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
    let package_store = PackageStore::new(match cli.package_root {
        Some(path) => std::path::absolute(path)?,
        None => super::metadata::default_package_root()?,
    });
    let no_activate = cli.no_activate;

    let json_output = cli.json_output;
    let json = cli.json || !std::io::stdout().is_terminal();
    let engine_options = EngineOptions {
        build_dir: cli.build_dir.as_deref(),
        state_dir: cli.state_dir.as_deref(),
        execution,
        recipes_path: Some(&recipes_path),
        defines: &cli.defines,
        inputs: &cli.inputs,
        no_persist_state: cli.no_persist_state,
        llm_profile: cli.llm_profile,
    };

    match cli.command {
        Commands::Validate { recipe } => {
            let recipe = resolve_recipe_path(&recipe, &recipe_paths)?;
            let supplied = parse_inputs(engine_options.inputs)?;
            let authority = levitate_recipe::needs::report::validate(
                &recipe,
                Some(&recipes_path),
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
            let recipe = resolve_recipe_path(&recipe, &recipe_paths)?;
            let authority = levitate_recipe::needs::report::inspect(
                &recipe,
                Some(&recipes_path),
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
                let recipe = resolve_recipe_path(&subject, &recipe_paths)?;
                levitate_recipe::needs::report::explain_recipe(
                    &recipe,
                    Some(&recipes_path),
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
            let old = resolve_recipe_path(&old, &recipe_paths)?;
            let new = resolve_recipe_path(&new, &recipe_paths)?;
            emit_result(
                &levitate_recipe::needs::report::diff(&old, &new, Some(&recipes_path))?,
                json_output.as_deref(),
                json,
                "Recipe permission changes",
            )?;
        }
        Commands::Install {
            recipe,
            autofix,
            autofix_attempts,
            autofix_cwd,
            autofix_prompt_file,
            autofix_allow_path,
        } => {
            let recipe_path = resolve_recipe_path(&recipe, &recipe_paths)?;
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
            let engine = create_engine(&recipe_path, &engine_options, autofix_cfg)?;
            let mut ctx = engine.execute(&recipe_path)?;
            if !no_activate {
                activate_package(&package_store, &recipe_path, &mut ctx, None)?;
            }
            emit_result(&ctx, json_output.as_deref(), json, "Installation completed")?;
        }

        Commands::Update { recipe } => {
            anyhow::ensure!(
                !no_activate,
                "update manages active packages; use install --no-activate for producers"
            );
            let installed = installed_package(&package_store, &recipe)?
                .with_context(|| format!("package {} is not installed", recipe.display()))?;
            let recipe_path = resolve_recipe_path(&recipe, &recipe_paths).or_else(|error| {
                if explicit_collection {
                    Err(error)
                } else {
                    resolve_recipe_path(&installed.recipe, &recipe_paths)
                }
            })?;
            let engine = create_engine(&recipe_path, &engine_options, None)?;
            let mut ctx = engine.execute(&recipe_path)?;
            activate_package(
                &package_store,
                &recipe_path,
                &mut ctx,
                Some(&installed.name),
            )?;
            emit_result(&ctx, json_output.as_deref(), json, "Package updated")?;
        }

        Commands::Installed => {
            let result = serde_json::json!({"packages": package_store.list()?, "profile": package_store.profile_path()});
            emit_result(&result, json_output.as_deref(), json, "Installed packages")?;
        }

        Commands::Remove { recipe } => {
            if !no_activate {
                if let Some(installed) = installed_package(&package_store, &recipe)? {
                    let removed = package_store.remove(&installed.name)?;
                    let result = serde_json::json!({"name": removed.name, "version": removed.version,
                        "operation": "remove", "profile": package_store.profile_path()});
                    emit_result(&result, json_output.as_deref(), json, "Package removed")?;
                    return Ok(());
                }
            }
            let recipe_path = resolve_recipe_path(&recipe, &recipe_paths)?;
            require_output_only_if_inactive(&recipe_path, recipes_path, no_activate)?;
            let engine = create_engine(&recipe_path, &engine_options, None)?;
            let ctx = engine.remove(&recipe_path)?;
            emit_result(&ctx, json_output.as_deref(), json, "Removal completed")?;
        }

        Commands::Cleanup { recipe, reason } => {
            let recipe_path = resolve_recipe_path(&recipe, &recipe_paths)?;
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
            if !no_activate {
                if let Some(installed) = installed_package(&package_store, &recipe)? {
                    let installed = package_store.verify(&installed.name)?;
                    let result = serde_json::json!({"name": installed.name, "version": installed.version,
                        "profile": package_store.profile_path()});
                    emit_result(&result, json_output.as_deref(), json, "Package is active")?;
                    return Ok(());
                }
            }
            let recipe_path = resolve_recipe_path(&recipe, &recipe_paths)?;
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
            let recipe_path = resolve_recipe_path(&recipe, &recipe_paths)?;
            let engine = create_engine(&recipe_path, &engine_options, None)?;
            let ctx = engine.is_built(&recipe_path)?;
            emit_result(&ctx, json_output.as_deref(), json, "Build check passed")?;
        }

        Commands::IsAcquired { recipe } => {
            let recipe_path = resolve_recipe_path(&recipe, &recipe_paths)?;
            let engine = create_engine(&recipe_path, &engine_options, None)?;
            let ctx = engine.is_acquired(&recipe_path)?;
            emit_result(
                &ctx,
                json_output.as_deref(),
                json,
                "Source acquisition check passed",
            )?;
        }

        Commands::List => list_recipes(&recipe_paths)?,

        Commands::Info { recipe } => {
            let recipe_path = resolve_recipe_path(&recipe, &recipe_paths)?;
            show_info(&recipe_path, Some(&recipes_path))?;
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
    let name = requested
        .to_str()
        .filter(|_| requested.components().count() == 1);
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

fn activate_package(
    store: &PackageStore,
    recipe: &Path,
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
    let installed = store.install(name, version, recipe, Path::new(output), updating.is_some())?;
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
    } = options;
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
        .with_execution_options(execution.clone())
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

fn resolve_recipe_path(recipe: &Path, recipes_paths: &[PathBuf]) -> Result<PathBuf> {
    if recipe.is_absolute() {
        if recipe.is_file() {
            return Ok(recipe.to_path_buf());
        }
        anyhow::bail!("Recipe file not found: {}", recipe.display());
    }

    if recipe.is_file() {
        return Ok(recipe.to_path_buf());
    }

    for recipes_path in recipes_paths {
        let in_recipes = recipes_path.join(recipe);
        if in_recipes.is_file() {
            return Ok(in_recipes);
        }
        let with_ext = recipes_path.join(format!("{}.rhai", recipe.display()));
        if with_ext.is_file() {
            return Ok(with_ext);
        }
    }

    anyhow::bail!(
        "Recipe not found: {}\nSearched in the current directory and: {}\nNext: run 'recipe list --recipes-path <DIRECTORY>' to see available recipes, or pass a .rhai file path.",
        recipe.display(),
        recipes_paths
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn list_recipes(recipes_paths: &[PathBuf]) -> Result<()> {
    use owo_colors::OwoColorize;

    let mut names = std::collections::BTreeSet::new();
    let mut found = false;
    for recipes_path in recipes_paths {
        let mut paths: Vec<_> = enumerate_recipes(recipes_path).collect();
        paths.sort();
        for path in paths {
            if !names.insert(path.file_name().unwrap().to_owned()) {
                continue;
            }
            let meta = RecipeMetadata::load(&path, Some(recipes_path))?;
            println!(
                "  {} {}",
                meta.name.bold(),
                meta.version.as_deref().unwrap_or("?").cyan()
            );
            found = true;
        }
    }
    if !found {
        bin_support::info("No recipes found in the selected collections");
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
