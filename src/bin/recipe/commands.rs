use anyhow::{Context, Result};
use levitate_recipe::{AutoFixConfig, RecipeEngine, helpers, output};
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;

use super::{
    cli::{Cli, Commands},
    metadata::{RecipeMetadata, default_recipes_path, enumerate_recipes},
};

pub(crate) fn execute(cli: Cli) -> Result<()> {
    let recipes_path = cli.recipes_path.unwrap_or_else(default_recipes_path);
    output::set_machine_events(cli.machine_events);

    if !recipes_path.exists() {
        std::fs::create_dir_all(&recipes_path).with_context(|| {
            format!(
                "Failed to create recipes directory: {}",
                recipes_path.display()
            )
        })?;
    }

    let json_output = cli.json_output;

    match cli.command {
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
                    attempts: autofix_attempts.max(1),
                    cwd: autofix_cwd,
                    prompt_file: autofix_prompt_file,
                    allow_paths: autofix_allow_path,
                })
            } else {
                None
            };
            let engine = create_engine(
                cli.build_dir.as_deref(),
                Some(&recipes_path),
                &cli.defines,
                cli.llm_profile.clone(),
                autofix_cfg,
            )?;
            let ctx = engine.execute(&recipe_path)?;
            emit_json(&ctx, json_output.as_deref())?;
        }

        Commands::Remove { recipe } => {
            let recipe_path = resolve_recipe_path(&recipe, &recipes_path)?;
            let engine = create_engine(
                cli.build_dir.as_deref(),
                Some(&recipes_path),
                &cli.defines,
                cli.llm_profile.clone(),
                None,
            )?;
            let ctx = engine.remove(&recipe_path)?;
            emit_json(&ctx, json_output.as_deref())?;
        }

        Commands::Cleanup { recipe, reason } => {
            let recipe_path = resolve_recipe_path(&recipe, &recipes_path)?;
            let engine = create_engine(
                cli.build_dir.as_deref(),
                Some(&recipes_path),
                &cli.defines,
                cli.llm_profile.clone(),
                None,
            )?;
            let cleanup_reason = reason
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or("manual");
            let ctx = engine.cleanup_with_reason(&recipe_path, cleanup_reason)?;
            emit_json(&ctx, json_output.as_deref())?;
        }

        Commands::IsInstalled { recipe } => {
            let recipe_path = resolve_recipe_path(&recipe, &recipes_path)?;
            let engine = create_engine(
                cli.build_dir.as_deref(),
                Some(&recipes_path),
                &cli.defines,
                cli.llm_profile.clone(),
                None,
            )?;
            let ctx = engine.is_installed(&recipe_path)?;
            emit_json(&ctx, json_output.as_deref())?;
        }

        Commands::IsBuilt { recipe } => {
            let recipe_path = resolve_recipe_path(&recipe, &recipes_path)?;
            let engine = create_engine(
                cli.build_dir.as_deref(),
                Some(&recipes_path),
                &cli.defines,
                cli.llm_profile.clone(),
                None,
            )?;
            let ctx = engine.is_built(&recipe_path)?;
            emit_json(&ctx, json_output.as_deref())?;
        }

        Commands::IsAcquired { recipe } => {
            let recipe_path = resolve_recipe_path(&recipe, &recipes_path)?;
            let engine = create_engine(
                cli.build_dir.as_deref(),
                Some(&recipes_path),
                &cli.defines,
                cli.llm_profile.clone(),
                None,
            )?;
            let ctx = engine.is_acquired(&recipe_path)?;
            emit_json(&ctx, json_output.as_deref())?;
        }

        Commands::List => list_recipes(&recipes_path)?,

        Commands::Info { recipe } => {
            let recipe_path = resolve_recipe_path(&recipe, &recipes_path)?;
            show_info(&recipe_path)?;
        }

        Commands::Hash { file } => {
            use owo_colors::OwoColorize;

            if !file.exists() {
                anyhow::bail!("File not found: {}", file.display());
            }

            output::info(&format!("Computing hashes for {}...", file.display()));

            let hashes = helpers::acquire::compute_hashes(&file)
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
                format!("verify_sha256(\"{}\");", hashes.sha256).green()
            );
        }
    }

    Ok(())
}

fn emit_json(ctx: &rhai::Map, path: Option<&Path>) -> Result<()> {
    let dynamic = rhai::Dynamic::from(ctx.clone());
    let json: serde_json::Value = rhai::serde::from_dynamic(&dynamic)
        .map_err(|e| anyhow::anyhow!("Failed to serialize ctx: {}", e))?;
    let json_str = serde_json::to_string(&json)?;
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

fn create_engine(
    build_dir: Option<&Path>,
    recipes_path: Option<&Path>,
    defines: &[String],
    llm_profile: Option<String>,
    autofix: Option<AutoFixConfig>,
) -> Result<RecipeEngine> {
    let build_dir = match build_dir {
        Some(dir) => {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("Failed to create build directory: {}", dir.display()))?;
            dir.to_path_buf()
        }
        None => {
            let temp = tempfile::tempdir().context("Failed to create temporary build directory")?;
            temp.keep()
        }
    };

    let mut engine = RecipeEngine::new(build_dir)
        .with_llm_profile(llm_profile)
        .with_autofix(autofix);

    if let Some(rp) = recipes_path {
        engine = engine.with_recipes_path(rp.to_path_buf());
    }

    for define in defines {
        if let Some((key, value)) = define.split_once('=') {
            engine.add_define(key.to_string(), value.to_string());
        } else {
            anyhow::bail!("Invalid --define format: '{}' (expected KEY=VALUE)", define);
        }
    }

    Ok(engine)
}

fn resolve_recipe_path(recipe: &Path, recipes_path: &Path) -> Result<PathBuf> {
    if recipe.is_absolute() {
        if recipe.exists() {
            return Ok(recipe.to_path_buf());
        }
        anyhow::bail!("Recipe file not found: {}", recipe.display());
    }

    if recipe.exists() {
        return Ok(recipe.to_path_buf());
    }

    let in_recipes = recipes_path.join(recipe);
    if in_recipes.exists() {
        return Ok(in_recipes);
    }

    let with_ext = recipes_path.join(format!("{}.rhai", recipe.display()));
    if with_ext.exists() {
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
        output::info(&format!("No recipes found in {}", recipes_path.display()));
        return Ok(());
    }

    let mut found = false;
    for path in enumerate_recipes(recipes_path) {
        let meta = RecipeMetadata::load(&path);
        println!(
            "  {} {}",
            meta.name.bold(),
            meta.version.as_deref().unwrap_or("?").cyan()
        );
        found = true;
    }

    if !found {
        output::info(&format!("No recipes found in {}", recipes_path.display()));
    }

    Ok(())
}

fn show_info(recipe_path: &Path) -> Result<()> {
    use owo_colors::OwoColorize;

    let meta = RecipeMetadata::load(recipe_path);

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::extract_string_value;
    use std::path::Path;
    use tempfile::TempDir;

    #[test]
    fn test_extract_string_value() {
        assert_eq!(extract_string_value("name: \"test\","), "test");
        assert_eq!(extract_string_value("version: \"1.0\","), "1.0");
        assert_eq!(
            extract_string_value("  description: \"A test package\","),
            "A test package"
        );
    }

    #[test]
    fn test_resolve_recipe_path_absolute() {
        let (_dir, recipes_path) = create_test_recipes_dir();
        let recipe_path = write_recipe(&recipes_path, "test", "");

        let result = resolve_recipe_path(&recipe_path, &recipes_path);
        assert!(result.is_ok());
    }

    #[test]
    fn test_resolve_recipe_path_with_ext() {
        let (_dir, recipes_path) = create_test_recipes_dir();
        write_recipe(&recipes_path, "test", "");

        let result = resolve_recipe_path(Path::new("test"), &recipes_path);
        assert!(result.is_ok());
        assert!(result.unwrap().ends_with("test.rhai"));
    }

    fn create_test_recipes_dir() -> (TempDir, PathBuf) {
        let dir = TempDir::new().unwrap();
        let recipes_path = dir.path().to_path_buf();
        (dir, recipes_path)
    }

    fn write_recipe(recipes_path: &Path, name: &str, content: &str) -> PathBuf {
        let path = recipes_path.join(format!("{}.rhai", name));
        std::fs::write(&path, content).unwrap();
        path
    }
}
