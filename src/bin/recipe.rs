//! Recipe CLI - Execute Rhai package recipes
//!
//! Usage:
//!   recipe install <recipe.rhai> [--prefix <path>]
//!   recipe run <recipe.rhai> [--prefix <path>]

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use levitate_recipe::RecipeEngine;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "recipe")]
#[command(about = "Rhai-based package recipe executor")]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Execute a recipe (alias for install)
    Run {
        /// Path to the recipe file
        recipe: PathBuf,

        /// Installation prefix
        #[arg(short, long, default_value = "/usr/local")]
        prefix: PathBuf,

        /// Build directory (uses temp dir if not specified)
        #[arg(short, long)]
        build_dir: Option<PathBuf>,

        /// Path to recipes directory for module resolution
        #[arg(short = 'r', long)]
        recipes_path: Option<PathBuf>,
    },

    /// Install a package from a recipe
    Install {
        /// Path to the recipe file
        recipe: PathBuf,

        /// Installation prefix
        #[arg(short, long, default_value = "/usr/local")]
        prefix: PathBuf,

        /// Build directory (uses temp dir if not specified)
        #[arg(short, long)]
        build_dir: Option<PathBuf>,

        /// Path to recipes directory for module resolution
        #[arg(short = 'r', long)]
        recipes_path: Option<PathBuf>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Run { recipe, prefix, build_dir, recipes_path }
        | Commands::Install { recipe, prefix, build_dir, recipes_path } => {
            execute_recipe(&recipe, &prefix, build_dir.as_deref(), recipes_path.as_deref())
        }
    }
}

fn execute_recipe(
    recipe: &PathBuf,
    prefix: &PathBuf,
    build_dir: Option<&std::path::Path>,
    recipes_path: Option<&std::path::Path>,
) -> Result<()> {
    // Create or use provided build directory
    let temp_dir;
    let build_dir = match build_dir {
        Some(dir) => {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("Failed to create build directory: {}", dir.display()))?;
            dir.to_path_buf()
        }
        None => {
            temp_dir = tempfile::tempdir()
                .context("Failed to create temporary build directory")?;
            temp_dir.path().to_path_buf()
        }
    };

    // Ensure prefix exists
    std::fs::create_dir_all(prefix)
        .with_context(|| format!("Failed to create prefix directory: {}", prefix.display()))?;

    // Create engine
    let mut engine = RecipeEngine::new(prefix.clone(), build_dir);

    // Set recipes path for module resolution if provided
    if let Some(path) = recipes_path {
        engine = engine.with_recipes_path(path.to_path_buf());
    } else if let Some(parent) = recipe.parent() {
        // Default to recipe's parent directory for module resolution
        engine = engine.with_recipes_path(parent.to_path_buf());
    }

    // Execute
    engine.execute(recipe)?;

    Ok(())
}
