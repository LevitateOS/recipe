//! Recipe CLI - Local-first package manager
//!
//! Usage:
//!   recipe install <name>          Install a package
//!   recipe remove <name>           Remove a package
//!   recipe update [name]           Check for updates
//!   recipe upgrade [name]          Apply updates
//!   recipe list                    List installed packages
//!   recipe search <pattern>        Search available recipes
//!   recipe info <name>             Show package info

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use levitate_recipe::{recipe_state, RecipeEngine};
use std::path::PathBuf;

/// Default recipes directory (XDG compliant)
fn default_recipes_path() -> PathBuf {
    if let Ok(path) = std::env::var("RECIPE_PATH") {
        return PathBuf::from(path);
    }

    // XDG_DATA_HOME or ~/.local/share
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

    /// Installation prefix
    #[arg(short, long, global = true, default_value = "/usr/local")]
    prefix: PathBuf,

    /// Build directory (uses temp dir if not specified)
    #[arg(short, long, global = true)]
    build_dir: Option<PathBuf>,
}

#[derive(Subcommand)]
enum Commands {
    /// Install a package
    Install {
        /// Package name or path to recipe file
        package: String,
    },

    /// Remove an installed package
    Remove {
        /// Package name
        package: String,
    },

    /// Check for available updates
    Update {
        /// Specific package to check (all if not specified)
        package: Option<String>,
    },

    /// Apply pending updates
    Upgrade {
        /// Specific package to upgrade (all if not specified)
        package: Option<String>,
    },

    /// List installed packages
    List,

    /// Search available recipes
    Search {
        /// Pattern to search for
        pattern: String,
    },

    /// Show package information
    Info {
        /// Package name
        package: String,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    let recipes_path = cli.recipes_path.unwrap_or_else(default_recipes_path);

    // Ensure recipes directory exists
    if !recipes_path.exists() {
        std::fs::create_dir_all(&recipes_path)
            .with_context(|| format!("Failed to create recipes directory: {}", recipes_path.display()))?;
    }

    match cli.command {
        Commands::Install { package } => {
            let recipe_path = resolve_recipe(&package, &recipes_path)?;
            let engine = create_engine(&cli.prefix, cli.build_dir.as_deref(), &recipes_path)?;
            engine.execute(&recipe_path)?;
        }

        Commands::Remove { package } => {
            let recipe_path = resolve_recipe(&package, &recipes_path)?;
            let engine = create_engine(&cli.prefix, cli.build_dir.as_deref(), &recipes_path)?;
            engine.remove(&recipe_path)?;
        }

        Commands::Update { package } => {
            let engine = create_engine(&cli.prefix, cli.build_dir.as_deref(), &recipes_path)?;

            if let Some(pkg) = package {
                let recipe_path = resolve_recipe(&pkg, &recipes_path)?;
                engine.update(&recipe_path)?;
            } else {
                // Update all installed packages
                println!("==> Checking for updates...");
                let recipes = find_installed_recipes(&recipes_path)?;
                for recipe_path in recipes {
                    engine.update(&recipe_path)?;
                }
            }
        }

        Commands::Upgrade { package } => {
            let engine = create_engine(&cli.prefix, cli.build_dir.as_deref(), &recipes_path)?;

            if let Some(pkg) = package {
                let recipe_path = resolve_recipe(&pkg, &recipes_path)?;
                engine.upgrade(&recipe_path)?;
            } else {
                // Upgrade all packages with pending updates
                println!("==> Upgrading packages...");
                let recipes = find_upgradable_recipes(&recipes_path)?;
                for recipe_path in recipes {
                    engine.upgrade(&recipe_path)?;
                }
            }
        }

        Commands::List => {
            list_packages(&recipes_path)?;
        }

        Commands::Search { pattern } => {
            search_packages(&pattern, &recipes_path)?;
        }

        Commands::Info { package } => {
            let recipe_path = resolve_recipe(&package, &recipes_path)?;
            show_info(&recipe_path)?;
        }
    }

    Ok(())
}

/// Create a recipe engine with proper configuration
fn create_engine(prefix: &PathBuf, build_dir: Option<&std::path::Path>, recipes_path: &PathBuf) -> Result<RecipeEngine> {
    // Create or use provided build directory
    let build_dir = match build_dir {
        Some(dir) => {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("Failed to create build directory: {}", dir.display()))?;
            dir.to_path_buf()
        }
        None => {
            let temp = tempfile::tempdir()
                .context("Failed to create temporary build directory")?;
            temp.keep()
        }
    };

    // Ensure prefix exists
    std::fs::create_dir_all(prefix)
        .with_context(|| format!("Failed to create prefix directory: {}", prefix.display()))?;

    let engine = RecipeEngine::new(prefix.clone(), build_dir)
        .with_recipes_path(recipes_path.clone());

    Ok(engine)
}

/// Validate a package name to prevent path traversal attacks
fn validate_package_name(package: &str) -> Result<()> {
    if package.is_empty() {
        anyhow::bail!("Package name cannot be empty");
    }

    // Package names must be simple identifiers (alphanumeric, underscore, hyphen)
    // This prevents path traversal attacks like "../../../etc/passwd"
    if !package.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-') {
        anyhow::bail!(
            "Invalid package name '{}': only alphanumeric characters, underscores, and hyphens are allowed",
            package
        );
    }

    Ok(())
}

/// Resolve a package name to a recipe path
fn resolve_recipe(package: &str, recipes_path: &PathBuf) -> Result<PathBuf> {
    // If it's already a path (contains path separators or ends with .rhai), handle specially
    let is_explicit_path = package.contains('/') || package.contains('\\') || package.ends_with(".rhai");

    if is_explicit_path {
        // For explicit paths, verify they exist
        let as_path = PathBuf::from(package);
        if as_path.exists() {
            return Ok(as_path);
        }
        anyhow::bail!("Recipe file not found: {}", package);
    }

    // For package names, validate to prevent path traversal
    validate_package_name(package)?;

    // Look for <name>.rhai in recipes directory
    let recipe_file = recipes_path.join(format!("{}.rhai", package));
    if recipe_file.exists() {
        return Ok(recipe_file);
    }

    // Look for <name>/<name>.rhai (subdirectory style)
    let subdir_recipe = recipes_path.join(package).join(format!("{}.rhai", package));
    if subdir_recipe.exists() {
        return Ok(subdir_recipe);
    }

    anyhow::bail!(
        "Recipe not found: {}\nSearched in: {}",
        package,
        recipes_path.display()
    )
}

/// Find all installed recipes
fn find_installed_recipes(recipes_path: &PathBuf) -> Result<Vec<PathBuf>> {
    let mut installed = Vec::new();

    if !recipes_path.exists() {
        return Ok(installed);
    }

    for entry in std::fs::read_dir(recipes_path)? {
        let entry = entry?;
        let path = entry.path();

        if path.extension().map(|e| e == "rhai").unwrap_or(false) {
            if let Ok(Some(true)) = recipe_state::get_var::<bool>(&path, "installed") {
                installed.push(path);
            }
        }
    }

    Ok(installed)
}

/// Find recipes with pending upgrades (version != installed_version)
fn find_upgradable_recipes(recipes_path: &PathBuf) -> Result<Vec<PathBuf>> {
    let mut upgradable = Vec::new();

    for path in find_installed_recipes(recipes_path)? {
        let version: Option<String> = recipe_state::get_var(&path, "version").unwrap_or(None);
        let installed_version: Option<recipe_state::OptionalString> =
            recipe_state::get_var(&path, "installed_version").unwrap_or(None);
        let installed_version: Option<String> = installed_version.and_then(|v| v.into());

        if version != installed_version {
            upgradable.push(path);
        }
    }

    Ok(upgradable)
}

/// List all packages
fn list_packages(recipes_path: &PathBuf) -> Result<()> {
    if !recipes_path.exists() {
        println!("No recipes found in {}", recipes_path.display());
        return Ok(());
    }

    let mut found = false;
    for entry in std::fs::read_dir(recipes_path)? {
        let entry = entry?;
        let path = entry.path();

        if path.extension().map(|e| e == "rhai").unwrap_or(false) {
            let name = path.file_stem().unwrap().to_string_lossy();
            let installed: Option<bool> = recipe_state::get_var(&path, "installed").unwrap_or(None);
            let version: Option<String> = recipe_state::get_var(&path, "version").unwrap_or(None);
            let installed_version: Option<recipe_state::OptionalString> =
                recipe_state::get_var(&path, "installed_version").unwrap_or(None);
            let installed_version: Option<String> = installed_version.and_then(|v| v.into());

            let status = if installed == Some(true) {
                if version != installed_version {
                    format!("[installed: {}, update: {}]",
                        installed_version.as_deref().unwrap_or("?"),
                        version.as_deref().unwrap_or("?"))
                } else {
                    format!("[installed: {}]", installed_version.as_deref().unwrap_or("?"))
                }
            } else {
                format!("[available: {}]", version.as_deref().unwrap_or("?"))
            };

            println!("  {} {}", name, status);
            found = true;
        }
    }

    if !found {
        println!("No recipes found in {}", recipes_path.display());
    }

    Ok(())
}

/// Search for packages
fn search_packages(pattern: &str, recipes_path: &PathBuf) -> Result<()> {
    if !recipes_path.exists() {
        println!("No recipes found");
        return Ok(());
    }

    let pattern_lower = pattern.to_lowercase();
    let mut found = false;

    for entry in std::fs::read_dir(recipes_path)? {
        let entry = entry?;
        let path = entry.path();

        if path.extension().map(|e| e == "rhai").unwrap_or(false) {
            let name = path.file_stem().unwrap().to_string_lossy();

            // Match against name
            if name.to_lowercase().contains(&pattern_lower) {
                let version: Option<String> = recipe_state::get_var(&path, "version").unwrap_or(None);
                let description: Option<String> = recipe_state::get_var(&path, "description").unwrap_or(None);

                println!("  {} - {} - {}",
                    name,
                    version.as_deref().unwrap_or("?"),
                    description.as_deref().unwrap_or("")
                );
                found = true;
            }
        }
    }

    if !found {
        println!("No packages matching '{}' found", pattern);
    }

    Ok(())
}

/// Show package info
fn show_info(recipe_path: &PathBuf) -> Result<()> {
    let name = recipe_path.file_stem().unwrap().to_string_lossy();

    let version: Option<String> = recipe_state::get_var(recipe_path, "version").unwrap_or(None);
    let description: Option<String> = recipe_state::get_var(recipe_path, "description").unwrap_or(None);
    let installed: Option<bool> = recipe_state::get_var(recipe_path, "installed").unwrap_or(None);
    let installed_version: Option<recipe_state::OptionalString> =
        recipe_state::get_var(recipe_path, "installed_version").unwrap_or(None);
    let installed_version: Option<String> = installed_version.and_then(|v| v.into());
    let installed_at: Option<i64> = recipe_state::get_var(recipe_path, "installed_at").unwrap_or(None);
    let installed_files: Option<Vec<String>> =
        recipe_state::get_var(recipe_path, "installed_files").unwrap_or(None);

    println!("Name:        {}", name);
    println!("Version:     {}", version.as_deref().unwrap_or("?"));
    if let Some(desc) = description {
        println!("Description: {}", desc);
    }
    println!("Recipe:      {}", recipe_path.display());
    println!();

    if installed == Some(true) {
        println!("Status:      Installed");
        if let Some(ver) = installed_version {
            println!("Installed:   {}", ver);
        }
        if let Some(ts) = installed_at {
            // Convert timestamp to human readable
            let datetime = chrono_lite(ts);
            println!("Installed at: {}", datetime);
        }
        if let Some(files) = installed_files {
            println!("Files:       {} installed", files.len());
            if files.len() <= 10 {
                for f in &files {
                    println!("             {}", f);
                }
            } else {
                for f in files.iter().take(5) {
                    println!("             {}", f);
                }
                println!("             ... and {} more", files.len() - 5);
            }
        }
    } else {
        println!("Status:      Not installed");
    }

    Ok(())
}

/// Simple timestamp to string conversion
fn chrono_lite(timestamp: i64) -> String {
    // Basic conversion without external dependency
    use std::time::{Duration, UNIX_EPOCH};

    let dt = UNIX_EPOCH + Duration::from_secs(timestamp as u64);
    format!("{:?}", dt)
}
