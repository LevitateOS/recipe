//! Recipe CLI - Local-first package manager
//!
//! Usage:
//!   recipe install <path>           Install a recipe
//!   recipe remove <path>            Remove an installed package
//!   recipe cleanup <path>           Clean up build artifacts
//!   recipe list                     List recipes in directory
//!   recipe info <path>              Show recipe info

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use levitate_recipe::{RecipeEngine, output};
use std::path::{Path, PathBuf};

/// Recipe metadata extracted from ctx block
#[derive(Debug, Default)]
struct RecipeMetadata {
    pub name: String,
    pub version: Option<String>,
    pub description: Option<String>,
}

impl RecipeMetadata {
    /// Load metadata from a recipe file by parsing its ctx block
    fn load(path: &Path) -> Self {
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => return Self::default(),
        };

        let mut meta = Self::default();

        // Simple extraction of ctx values
        for line in content.lines() {
            let line = line.trim();
            if line.starts_with("name:") {
                meta.name = extract_string_value(line);
            } else if line.starts_with("version:") {
                meta.version = Some(extract_string_value(line));
            } else if line.starts_with("description:") {
                meta.description = Some(extract_string_value(line));
            }
        }

        if meta.name.is_empty() {
            meta.name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .map(|s| s.to_string())
                .unwrap_or_default();
        }

        meta
    }
}

/// Extract string value from a line like `name: "value",`
fn extract_string_value(line: &str) -> String {
    let Some(colon_pos) = line.find(':') else {
        return String::new();
    };
    let value_part = line[colon_pos + 1..].trim();
    // Remove surrounding quotes and trailing comma
    value_part
        .trim_start_matches('"')
        .trim_end_matches(',')
        .trim_end_matches('"')
        .to_string()
}

/// Iterator over recipe files in a directory
fn enumerate_recipes(recipes_path: &Path) -> impl Iterator<Item = PathBuf> + '_ {
    std::fs::read_dir(recipes_path)
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|e| e == "rhai"))
}

/// Default recipes directory (XDG compliant)
fn default_recipes_path() -> PathBuf {
    if let Ok(path) = std::env::var("RECIPE_PATH") {
        return PathBuf::from(path);
    }

    let data_home = std::env::var("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".local/share")
        });

    data_home.join("recipe/recipes")
}

#[derive(Parser)]
#[command(name = "recipe")]
#[command(about = "Local-first package manager using Rhai recipes")]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// Path to recipes directory
    #[arg(short = 'r', long, global = true)]
    recipes_path: Option<PathBuf>,

    /// Build directory (uses temp dir if not specified)
    #[arg(short, long, global = true)]
    build_dir: Option<PathBuf>,
}

#[derive(Subcommand)]
enum Commands {
    /// Install a package from a recipe file
    Install {
        /// Path to recipe file
        recipe: PathBuf,
    },

    /// Remove an installed package
    Remove {
        /// Path to recipe file
        recipe: PathBuf,
    },

    /// Clean up build artifacts
    Cleanup {
        /// Path to recipe file
        recipe: PathBuf,
    },

    /// List recipes in directory
    List,

    /// Show recipe information
    Info {
        /// Path to recipe file
        recipe: PathBuf,
    },

    /// Compute hashes for a file
    Hash {
        /// Path to file
        file: PathBuf,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    let recipes_path = cli.recipes_path.unwrap_or_else(default_recipes_path);

    // Ensure recipes directory exists
    if !recipes_path.exists() {
        std::fs::create_dir_all(&recipes_path).with_context(|| {
            format!(
                "Failed to create recipes directory: {}",
                recipes_path.display()
            )
        })?;
    }

    match cli.command {
        Commands::Install { recipe } => {
            let recipe_path = resolve_recipe_path(&recipe, &recipes_path)?;
            let engine = create_engine(cli.build_dir.as_deref())?;
            engine.execute(&recipe_path)?;
        }

        Commands::Remove { recipe } => {
            let recipe_path = resolve_recipe_path(&recipe, &recipes_path)?;
            let engine = create_engine(cli.build_dir.as_deref())?;
            engine.remove(&recipe_path)?;
        }

        Commands::Cleanup { recipe } => {
            let recipe_path = resolve_recipe_path(&recipe, &recipes_path)?;
            let engine = create_engine(cli.build_dir.as_deref())?;
            engine.cleanup(&recipe_path)?;
        }

        Commands::List => {
            list_recipes(&recipes_path)?;
        }

        Commands::Info { recipe } => {
            let recipe_path = resolve_recipe_path(&recipe, &recipes_path)?;
            show_info(&recipe_path)?;
        }

        Commands::Hash { file } => {
            use levitate_recipe::helpers::acquire::compute_hashes;
            use owo_colors::OwoColorize;

            if !file.exists() {
                anyhow::bail!("File not found: {}", file.display());
            }

            output::info(&format!("Computing hashes for {}...", file.display()));

            let hashes = compute_hashes(&file)
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

/// Create a recipe engine with proper configuration
fn create_engine(build_dir: Option<&Path>) -> Result<RecipeEngine> {
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

    Ok(RecipeEngine::new(build_dir))
}

/// Resolve a recipe path (absolute path or relative to recipes_path)
fn resolve_recipe_path(recipe: &Path, recipes_path: &Path) -> Result<PathBuf> {
    if recipe.is_absolute() {
        if recipe.exists() {
            return Ok(recipe.to_path_buf());
        }
        anyhow::bail!("Recipe file not found: {}", recipe.display());
    }

    // Try as-is first
    if recipe.exists() {
        return Ok(recipe.to_path_buf());
    }

    // Try in recipes directory
    let in_recipes = recipes_path.join(recipe);
    if in_recipes.exists() {
        return Ok(in_recipes);
    }

    // Try with .rhai extension
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

/// List all recipes in directory
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

/// Show recipe info
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
    use tempfile::TempDir;

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

    #[test]
    fn test_extract_string_value() {
        assert_eq!(extract_string_value("name: \"test\","), "test");
        assert_eq!(extract_string_value("version: \"1.0\","), "1.0");
        assert_eq!(extract_string_value("  description: \"A test package\","), "A test package");
    }

    #[test]
    fn test_recipe_metadata_load() {
        let (_dir, recipes_path) = create_test_recipes_dir();
        let recipe_path = write_recipe(
            &recipes_path,
            "test",
            r#"let ctx = #{
    name: "mypackage",
    version: "2.0",
    description: "A test package",
};"#,
        );

        let meta = RecipeMetadata::load(&recipe_path);
        assert_eq!(meta.name, "mypackage");
        assert_eq!(meta.version, Some("2.0".to_string()));
        assert_eq!(meta.description, Some("A test package".to_string()));
    }

    #[test]
    fn test_recipe_metadata_fallback_name() {
        let (_dir, recipes_path) = create_test_recipes_dir();
        let recipe_path = write_recipe(&recipes_path, "test", "let ctx = #{};");

        let meta = RecipeMetadata::load(&recipe_path);
        assert_eq!(meta.name, "test"); // Falls back to filename
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
}
