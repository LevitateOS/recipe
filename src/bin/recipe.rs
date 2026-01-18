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
use levitate_recipe::{deps, output, recipe_state, RecipeEngine};
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

        /// Also install dependencies
        #[arg(short = 'd', long = "deps")]
        with_deps: bool,
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

    /// Show package dependencies
    Deps {
        /// Package name
        package: String,

        /// Show install order (resolved dependencies)
        #[arg(long)]
        resolve: bool,
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
        Commands::Install { package, with_deps } => {
            let engine = create_engine(&cli.prefix, cli.build_dir.as_deref(), &recipes_path)?;

            if with_deps {
                // Resolve dependencies and install in order
                let install_order = deps::resolve_deps(&package, &recipes_path)?;
                let uninstalled = deps::filter_uninstalled(install_order)?;

                if uninstalled.is_empty() {
                    output::skip(&format!("{} and all dependencies already installed", package));
                } else {
                    let names: Vec<_> = uninstalled.iter().map(|(n, _)| n.as_str()).collect();
                    output::info(&format!("Installing {} package(s): {}", names.len(), names.join(", ")));

                    let total = uninstalled.len();
                    for (i, (name, path)) in uninstalled.into_iter().enumerate() {
                        output::action_numbered(i + 1, total, &format!("Installing {}", name));
                        engine.execute(&path)?;
                    }
                }
            } else {
                let recipe_path = resolve_recipe(&package, &recipes_path)?;
                engine.execute(&recipe_path)?;
            }
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
                output::action("Checking for updates...");
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
                output::action("Upgrading packages...");
                let recipes = find_upgradable_recipes(&recipes_path)?;
                if recipes.is_empty() {
                    output::info("All packages are up to date");
                } else {
                    for recipe_path in recipes {
                        engine.upgrade(&recipe_path)?;
                    }
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

        Commands::Deps { package, resolve } => {
            use owo_colors::OwoColorize;

            if resolve {
                // Show resolved install order
                let install_order = deps::resolve_deps(&package, &recipes_path)?;
                output::info(&format!("Install order for {}:", package.bold()));
                for (i, (name, path)) in install_order.iter().enumerate() {
                    let installed: Option<bool> =
                        recipe_state::get_var(path, "installed").unwrap_or(None);
                    if installed == Some(true) {
                        println!("  {}. {} {}", i + 1, name.green(), "[installed]".dimmed());
                    } else {
                        println!("  {}. {}", i + 1, name);
                    }
                }
            } else {
                // Show direct dependencies only
                let recipe_path = resolve_recipe(&package, &recipes_path)?;
                let pkg_deps: Option<Vec<String>> =
                    recipe_state::get_var(&recipe_path, "deps").unwrap_or(None);

                output::info(&format!("Dependencies for {}:", package.bold()));
                match pkg_deps {
                    Some(ref d) if !d.is_empty() => {
                        for dep in d {
                            println!("  {} {}", "-".cyan(), dep);
                        }
                    }
                    _ => {
                        println!("  {}", "(none)".dimmed());
                    }
                }
            }
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
    use owo_colors::OwoColorize;

    if !recipes_path.exists() {
        output::info(&format!("No recipes found in {}", recipes_path.display()));
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

            let is_installed = installed == Some(true);
            let status = if is_installed {
                if version != installed_version {
                    format!("[installed: {}, {} available]",
                        installed_version.as_deref().unwrap_or("?"),
                        version.as_deref().unwrap_or("?").yellow())
                } else {
                    format!("[installed: {}]", installed_version.as_deref().unwrap_or("?"))
                }
            } else {
                format!("[available: {}]", version.as_deref().unwrap_or("?"))
            };

            output::list_item(&name, &status, is_installed);
            found = true;
        }
    }

    if !found {
        output::info(&format!("No recipes found in {}", recipes_path.display()));
    }

    Ok(())
}

/// Search for packages
fn search_packages(pattern: &str, recipes_path: &PathBuf) -> Result<()> {
    use owo_colors::OwoColorize;

    if !recipes_path.exists() {
        output::info("No recipes found");
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

                println!("  {} {} {}",
                    name.bold(),
                    version.as_deref().unwrap_or("?").cyan(),
                    description.as_deref().unwrap_or("").dimmed()
                );
                found = true;
            }
        }
    }

    if !found {
        output::info(&format!("No packages matching '{}' found", pattern));
    }

    Ok(())
}

/// Show package info
fn show_info(recipe_path: &PathBuf) -> Result<()> {
    use owo_colors::OwoColorize;

    let name = recipe_path.file_stem().unwrap().to_string_lossy();

    let version: Option<String> = recipe_state::get_var(recipe_path, "version").unwrap_or(None);
    let description: Option<String> = recipe_state::get_var(recipe_path, "description").unwrap_or(None);
    let pkg_deps: Option<Vec<String>> = recipe_state::get_var(recipe_path, "deps").unwrap_or(None);
    let installed: Option<bool> = recipe_state::get_var(recipe_path, "installed").unwrap_or(None);
    let installed_version: Option<recipe_state::OptionalString> =
        recipe_state::get_var(recipe_path, "installed_version").unwrap_or(None);
    let installed_version: Option<String> = installed_version.and_then(|v| v.into());
    let installed_at: Option<i64> = recipe_state::get_var(recipe_path, "installed_at").unwrap_or(None);
    let installed_files: Option<Vec<String>> =
        recipe_state::get_var(recipe_path, "installed_files").unwrap_or(None);

    println!("{:<12} {}", "Name:".bold(), name.bold().cyan());
    println!("{:<12} {}", "Version:".bold(), version.as_deref().unwrap_or("?").green());
    if let Some(desc) = description {
        println!("{:<12} {}", "Description:".bold(), desc);
    }
    match pkg_deps {
        Some(ref deps) if !deps.is_empty() => {
            println!("{:<12} {}", "Depends:".bold(), deps.join(", "));
        }
        _ => {}
    }
    println!("{:<12} {}", "Recipe:".bold(), recipe_path.display().to_string().dimmed());
    println!();

    if installed == Some(true) {
        println!("{:<12} {}", "Status:".bold(), "Installed".green());
        if let Some(ver) = installed_version {
            println!("{:<12} {}", "Installed:".bold(), ver);
        }
        if let Some(ts) = installed_at {
            // Convert timestamp to human readable
            let datetime = chrono_lite(ts);
            println!("{:<12} {}", "Installed at:".bold(), datetime.dimmed());
        }
        if let Some(files) = installed_files {
            println!("{:<12} {} files", "Files:".bold(), files.len());
            if files.len() <= 10 {
                for f in &files {
                    println!("             {}", f.dimmed());
                }
            } else {
                for f in files.iter().take(5) {
                    println!("             {}", f.dimmed());
                }
                println!("             {} and {} more", "...".dimmed(), files.len() - 5);
            }
        }
    } else {
        println!("{:<12} {}", "Status:".bold(), "Not installed".yellow());
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use tempfile::TempDir;

    // ==================== Package Name Validation ====================

    #[test]
    fn test_valid_package_names() {
        assert!(validate_package_name("ripgrep").is_ok());
        assert!(validate_package_name("my-package").is_ok());
        assert!(validate_package_name("my_package").is_ok());
        assert!(validate_package_name("package123").is_ok());
        assert!(validate_package_name("123package").is_ok());
        assert!(validate_package_name("a").is_ok());
        assert!(validate_package_name("A").is_ok());
        assert!(validate_package_name("pkg-name_v2").is_ok());
    }

    #[test]
    fn test_empty_package_name() {
        let result = validate_package_name("");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("cannot be empty"));
    }

    #[test]
    fn test_path_traversal_attacks() {
        // These should all be rejected
        assert!(validate_package_name("..").is_err());
        assert!(validate_package_name("../etc/passwd").is_err());
        assert!(validate_package_name("../../etc/passwd").is_err());
        assert!(validate_package_name("foo/../bar").is_err());
        assert!(validate_package_name("/etc/passwd").is_err());
        assert!(validate_package_name("foo/bar").is_err());
    }

    #[test]
    fn test_special_characters_rejected() {
        assert!(validate_package_name("pkg!name").is_err());
        assert!(validate_package_name("pkg@name").is_err());
        assert!(validate_package_name("pkg#name").is_err());
        assert!(validate_package_name("pkg$name").is_err());
        assert!(validate_package_name("pkg%name").is_err());
        assert!(validate_package_name("pkg^name").is_err());
        assert!(validate_package_name("pkg&name").is_err());
        assert!(validate_package_name("pkg*name").is_err());
        assert!(validate_package_name("pkg(name").is_err());
        assert!(validate_package_name("pkg)name").is_err());
        assert!(validate_package_name("pkg+name").is_err());
        assert!(validate_package_name("pkg=name").is_err());
        assert!(validate_package_name("pkg[name").is_err());
        assert!(validate_package_name("pkg]name").is_err());
        assert!(validate_package_name("pkg{name").is_err());
        assert!(validate_package_name("pkg}name").is_err());
        assert!(validate_package_name("pkg|name").is_err());
        assert!(validate_package_name("pkg\\name").is_err());
        assert!(validate_package_name("pkg:name").is_err());
        assert!(validate_package_name("pkg;name").is_err());
        assert!(validate_package_name("pkg'name").is_err());
        assert!(validate_package_name("pkg\"name").is_err());
        assert!(validate_package_name("pkg<name").is_err());
        assert!(validate_package_name("pkg>name").is_err());
        assert!(validate_package_name("pkg,name").is_err());
        assert!(validate_package_name("pkg?name").is_err());
        assert!(validate_package_name("pkg`name").is_err());
        assert!(validate_package_name("pkg~name").is_err());
        assert!(validate_package_name("pkg name").is_err()); // space
        assert!(validate_package_name("pkg\tname").is_err()); // tab
        assert!(validate_package_name("pkg\nname").is_err()); // newline
    }

    #[test]
    fn test_dots_rejected() {
        // Single dot and double dot are dangerous
        assert!(validate_package_name(".").is_err());
        assert!(validate_package_name("..").is_err());
        // Dots within names are also rejected (keep it simple)
        assert!(validate_package_name("pkg.name").is_err());
        assert!(validate_package_name(".hidden").is_err());
    }

    // ==================== Recipe Resolution ====================

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
    fn test_resolve_recipe_simple_name() {
        let (_dir, recipes_path) = create_test_recipes_dir();
        write_recipe(&recipes_path, "ripgrep", "let name = \"ripgrep\";");

        let result = resolve_recipe("ripgrep", &recipes_path);
        assert!(result.is_ok());
        assert!(result.unwrap().ends_with("ripgrep.rhai"));
    }

    #[test]
    fn test_resolve_recipe_with_hyphen() {
        let (_dir, recipes_path) = create_test_recipes_dir();
        write_recipe(&recipes_path, "my-package", "let name = \"my-package\";");

        let result = resolve_recipe("my-package", &recipes_path);
        assert!(result.is_ok());
    }

    #[test]
    fn test_resolve_recipe_not_found() {
        let (_dir, recipes_path) = create_test_recipes_dir();
        let result = resolve_recipe("nonexistent", &recipes_path);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Recipe not found"));
    }

    #[test]
    fn test_resolve_recipe_path_traversal_rejected() {
        let (_dir, recipes_path) = create_test_recipes_dir();
        // Paths with "/" are treated as explicit paths
        let result = resolve_recipe("../../../etc/passwd", &recipes_path);
        assert!(result.is_err());
        // Explicit paths that don't exist return "not found"
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[test]
    fn test_validate_package_name_called_for_simple_names() {
        // Package names without path separators go through validation
        let (_dir, recipes_path) = create_test_recipes_dir();
        // "pkg!name" has no "/" but has invalid char "!"
        let result = resolve_recipe("pkg!name", &recipes_path);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Invalid package name"));
    }

    #[test]
    fn test_resolve_recipe_explicit_path() {
        let (_dir, recipes_path) = create_test_recipes_dir();
        let recipe_path = write_recipe(&recipes_path, "test", "let name = \"test\";");

        // Should accept explicit .rhai path
        let result = resolve_recipe(recipe_path.to_str().unwrap(), &recipes_path);
        assert!(result.is_ok());
    }

    #[test]
    fn test_resolve_recipe_subdir_style() {
        let (_dir, recipes_path) = create_test_recipes_dir();

        // Create ripgrep/ripgrep.rhai
        let subdir = recipes_path.join("ripgrep");
        std::fs::create_dir(&subdir).unwrap();
        std::fs::write(subdir.join("ripgrep.rhai"), "let name = \"ripgrep\";").unwrap();

        let result = resolve_recipe("ripgrep", &recipes_path);
        assert!(result.is_ok());
        assert!(result.unwrap().ends_with("ripgrep/ripgrep.rhai"));
    }

    #[test]
    fn test_resolve_recipe_prefers_direct_file() {
        let (_dir, recipes_path) = create_test_recipes_dir();

        // Create both ripgrep.rhai and ripgrep/ripgrep.rhai
        write_recipe(&recipes_path, "ripgrep", "let name = \"direct\";");
        let subdir = recipes_path.join("ripgrep");
        std::fs::create_dir(&subdir).unwrap();
        std::fs::write(subdir.join("ripgrep.rhai"), "let name = \"subdir\";").unwrap();

        // Should prefer the direct file
        let result = resolve_recipe("ripgrep", &recipes_path);
        assert!(result.is_ok());
        let path = result.unwrap();
        assert!(!path.to_string_lossy().contains("ripgrep/ripgrep"));
    }

    // ==================== Find Installed Recipes ====================

    #[test]
    fn test_find_installed_recipes_empty() {
        let (_dir, recipes_path) = create_test_recipes_dir();
        let result = find_installed_recipes(&recipes_path);
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }

    #[test]
    fn test_find_installed_recipes_finds_installed() {
        let (_dir, recipes_path) = create_test_recipes_dir();
        write_recipe(&recipes_path, "pkg1", "let installed = true;");
        write_recipe(&recipes_path, "pkg2", "let installed = false;");
        write_recipe(&recipes_path, "pkg3", "let installed = true;");

        let result = find_installed_recipes(&recipes_path).unwrap();
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn test_find_installed_recipes_nonexistent_dir() {
        let recipes_path = PathBuf::from("/nonexistent/path");
        let result = find_installed_recipes(&recipes_path);
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }

    // ==================== Find Upgradable Recipes ====================

    #[test]
    fn test_find_upgradable_recipes() {
        let (_dir, recipes_path) = create_test_recipes_dir();

        // Installed but up to date
        write_recipe(&recipes_path, "pkg1", r#"
let version = "1.0";
let installed = true;
let installed_version = "1.0";
"#);

        // Installed with update available
        write_recipe(&recipes_path, "pkg2", r#"
let version = "2.0";
let installed = true;
let installed_version = "1.0";
"#);

        // Not installed
        write_recipe(&recipes_path, "pkg3", r#"
let version = "1.0";
let installed = false;
"#);

        let result = find_upgradable_recipes(&recipes_path).unwrap();
        assert_eq!(result.len(), 1);
        assert!(result[0].to_string_lossy().contains("pkg2"));
    }
}
