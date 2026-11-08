use anyhow::{Context, Result};
use levitate_recipe::{AutoFixConfig, GinuxLauncher, RecipeEngine, bin_support};
use std::io::Write;
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
    let recipes_path = cli.recipes_path;

    let json_output = cli.json_output;
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
            let recipe = resolve_recipe_path(&recipe, &recipes_path)?;
            let supplied = parse_inputs(&engine_options.inputs)?;
            let authority = levitate_recipe::needs::validate(
                &recipe,
                Some(&recipes_path),
                &supplied,
                &engine_options.execution.narrowing,
            )?;
            emit_json(
                &serde_json::json!({"valid":true,"authority":authority}),
                json_output.as_deref(),
            )?;
        }
        Commands::Inspect { recipe } => {
            let recipe = resolve_recipe_path(&recipe, &recipes_path)?;
            let authority = levitate_recipe::needs::inspect(
                &recipe,
                Some(&recipes_path),
                &engine_options.execution.narrowing,
            )?;
            emit_json(&authority, json_output.as_deref())?;
        }
        Commands::Explain { subject, operation } => {
            let path = PathBuf::from(&subject);
            let logs = engine_options.state_dir.map(PathBuf::from).or_else(|| {
                std::env::var_os("XDG_STATE_HOME")
                    .map(PathBuf::from)
                    .or_else(|| dirs::home_dir().map(|home| home.join(".local/state")))
                    .map(|path| path.join("recipe"))
            });
            let recorded = if path.is_dir() {
                Some(path)
            } else {
                logs.map(|root| root.join("logs").join(&subject))
                    .filter(|path| path.is_dir())
            };
            let explanation = if let Some(directory) = recorded {
                levitate_recipe::needs::explain_invocation(&directory, &operation)?
            } else {
                let recipe = resolve_recipe_path(&subject, &recipes_path)?;
                levitate_recipe::needs::explain_recipe(
                    &recipe,
                    Some(&recipes_path),
                    &operation,
                    &engine_options.execution.narrowing,
                )?
            };
            emit_json(&explanation, json_output.as_deref())?;
        }
        Commands::Diff { old, new } => {
            let old = resolve_recipe_path(&old, &recipes_path)?;
            let new = resolve_recipe_path(&new, &recipes_path)?;
            emit_json(
                &levitate_recipe::needs::diff(&old, &new, Some(&recipes_path))?,
                json_output.as_deref(),
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
            let recipe_path = resolve_recipe_path(&recipe, &recipes_path)?;
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
            let ctx = engine.execute(&recipe_path)?;
            emit_json(&ctx, json_output.as_deref())?;
        }

        Commands::Remove { recipe } => {
            let recipe_path = resolve_recipe_path(&recipe, &recipes_path)?;
            let engine = create_engine(&recipe_path, &engine_options, None)?;
            let ctx = engine.remove(&recipe_path)?;
            emit_json(&ctx, json_output.as_deref())?;
        }

        Commands::Cleanup { recipe, reason } => {
            let recipe_path = resolve_recipe_path(&recipe, &recipes_path)?;
            let engine = create_engine(&recipe_path, &engine_options, None)?;
            let cleanup_reason = reason
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or(levitate_recipe::bin_support::DEFAULT_CLEANUP_REASON);
            let ctx = engine.cleanup_with_reason(&recipe_path, cleanup_reason)?;
            emit_json(&ctx, json_output.as_deref())?;
        }

        Commands::IsInstalled { recipe } => {
            let recipe_path = resolve_recipe_path(&recipe, &recipes_path)?;
            let engine = create_engine(&recipe_path, &engine_options, None)?;
            let ctx = engine.is_installed(&recipe_path)?;
            emit_json(&ctx, json_output.as_deref())?;
        }

        Commands::IsBuilt { recipe } => {
            let recipe_path = resolve_recipe_path(&recipe, &recipes_path)?;
            let engine = create_engine(&recipe_path, &engine_options, None)?;
            let ctx = engine.is_built(&recipe_path)?;
            emit_json(&ctx, json_output.as_deref())?;
        }

        Commands::IsAcquired { recipe } => {
            let recipe_path = resolve_recipe_path(&recipe, &recipes_path)?;
            let engine = create_engine(&recipe_path, &engine_options, None)?;
            let ctx = engine.is_acquired(&recipe_path)?;
            emit_json(&ctx, json_output.as_deref())?;
        }

        Commands::List => list_recipes(&recipes_path)?,

        Commands::Info { recipe } => {
            let recipe_path = resolve_recipe_path(&recipe, &recipes_path)?;
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

fn emit_json(ctx: &serde_json::Value, path: Option<&Path>) -> Result<()> {
    let json_str = serde_json::to_string(ctx)?;
    match path {
        Some(p) => {
            std::fs::write(p, &json_str)
                .with_context(|| format!("Failed to write JSON to {}", p.display()))?;
        }
        None => {
            writeln!(std::io::stdout(), "{}", json_str)?;
        }
    }
    Ok(())
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

    for input in *inputs {
        let (name, path) = input
            .split_once('=')
            .context("--input requires NAME=PATH")?;
        if name.is_empty() || path.is_empty() {
            anyhow::bail!("--input requires nonempty NAME=PATH");
        }
        engine.add_input(name.to_owned(), PathBuf::from(path));
    }

    Ok(engine)
}

fn resolve_recipe_path(recipe: &Path, recipes_path: &Path) -> Result<PathBuf> {
    if recipe.is_absolute() {
        if recipe.is_file() {
            return Ok(recipe.to_path_buf());
        }
        anyhow::bail!("Recipe file not found: {}", recipe.display());
    }

    if recipe.is_file() {
        return Ok(recipe.to_path_buf());
    }

    let in_recipes = recipes_path.join(recipe);
    if in_recipes.is_file() {
        return Ok(in_recipes);
    }

    let with_ext = recipes_path.join(format!("{}.rhai", recipe.display()));
    if with_ext.is_file() {
        return Ok(with_ext);
    }

    anyhow::bail!(
        "Recipe not found: {}\nSearched in: {}",
        recipe.display(),
        recipes_path.display()
    )
}

fn list_recipes(recipes_path: &Path) -> Result<()> {
    use owo_colors::OwoColorize;

    if !recipes_path.exists() {
        bin_support::info(&format!("No recipes found in {}", recipes_path.display()));
        return Ok(());
    }

    let mut found = false;
    for path in enumerate_recipes(recipes_path) {
        let meta = RecipeMetadata::load(&path, Some(recipes_path))?;
        println!(
            "  {} {}",
            meta.name.bold(),
            meta.version.as_deref().unwrap_or("?").cyan()
        );
        found = true;
    }

    if !found {
        bin_support::info(&format!("No recipes found in {}", recipes_path.display()));
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
            let (name, path) = value.split_once('=').context("input requires NAME=PATH")?;
            Ok((name.to_owned(), PathBuf::from(path)))
        })
        .collect()
}
