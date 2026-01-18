//! recipe - Package manager for LevitateOS
//!
//! Installs packages from S-expression recipes.
//! Self-sufficient: handles ALL dependencies through recipes, no external package managers.

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use levitate_recipe::{parse, Context as ExecContext, DepSpec, Executor, Recipe};
use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;

/// Default recipe directory
const RECIPE_DIR: &str = "/usr/share/recipe/recipes";

/// Track installed packages to avoid cycles and redundant work
const INSTALLED_DB: &str = "/var/lib/recipe/installed";

#[derive(Parser)]
#[command(name = "recipe")]
#[command(about = "LevitateOS package manager - self-sufficient, no external dependencies")]
#[command(version)]
struct Cli {
    /// Recipe directory (default: /usr/share/recipe/recipes)
    #[arg(long, env = "RECIPE_DIR")]
    recipe_dir: Option<PathBuf>,

    /// Installation prefix (default: /usr/local)
    #[arg(long, default_value = "/usr/local")]
    prefix: PathBuf,

    /// Verbose output
    #[arg(short, long)]
    verbose: bool,

    /// Dry run - show commands without executing
    #[arg(short = 'n', long)]
    dry_run: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Install a package (with all dependencies)
    Install {
        /// Package name or path to .recipe file
        package: String,
    },

    /// Remove a package
    Remove {
        /// Package name
        package: String,
    },

    /// List available packages
    List,

    /// Show package info (including dependencies)
    Info {
        /// Package name
        package: String,
    },

    /// Show dependency tree for a package
    Deps {
        /// Package name
        package: String,
    },

    /// Bootstrap a base system to target directory
    Bootstrap {
        /// Target directory (e.g., /mnt)
        target: PathBuf,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    let recipe_dir = cli
        .recipe_dir
        .unwrap_or_else(|| PathBuf::from(RECIPE_DIR));

    match cli.command {
        Commands::Install { package } => {
            let mut installed = load_installed_db();
            install_with_deps(&package, &recipe_dir, &cli.prefix, cli.verbose, cli.dry_run, &mut installed)?;
            if !cli.dry_run {
                save_installed_db(&installed)?;
            }
            Ok(())
        }
        Commands::Remove { package } => {
            remove_package(&package, &recipe_dir, &cli.prefix, cli.verbose, cli.dry_run)
        }
        Commands::List => list_packages(&recipe_dir),
        Commands::Info { package } => show_info(&package, &recipe_dir),
        Commands::Deps { package } => show_deps(&package, &recipe_dir),
        Commands::Bootstrap { target } => {
            bootstrap_system(&target, &recipe_dir, cli.verbose, cli.dry_run)
        }
    }
}

/// Load the set of installed packages
fn load_installed_db() -> HashSet<String> {
    let path = PathBuf::from(INSTALLED_DB);
    if path.exists() {
        if let Ok(content) = fs::read_to_string(&path) {
            return content.lines().map(|s| s.to_string()).collect();
        }
    }
    HashSet::new()
}

/// Save the set of installed packages
fn save_installed_db(installed: &HashSet<String>) -> Result<()> {
    let path = PathBuf::from(INSTALLED_DB);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let content: Vec<&str> = installed.iter().map(|s| s.as_str()).collect();
    fs::write(&path, content.join("\n"))?;
    Ok(())
}

/// Find a recipe by name or path
fn find_recipe(package: &str, recipe_dir: &PathBuf) -> Result<(PathBuf, String)> {
    // If it's a path to a .recipe file, use it directly
    if package.ends_with(".recipe") {
        let path = PathBuf::from(package);
        if path.exists() {
            let content = fs::read_to_string(&path)
                .with_context(|| format!("Failed to read recipe: {}", path.display()))?;
            return Ok((path, content));
        }
    }

    // Look in recipe directory
    let recipe_path = recipe_dir.join(format!("{}.recipe", package));
    if recipe_path.exists() {
        let content = fs::read_to_string(&recipe_path)
            .with_context(|| format!("Failed to read recipe: {}", recipe_path.display()))?;
        return Ok((recipe_path, content));
    }

    bail!(
        "Recipe not found: {}\nLooked in: {}\n\nThis package needs a recipe. Create: {}.recipe",
        package,
        recipe_dir.display(),
        package
    )
}

/// Parse a recipe from content
fn parse_recipe(content: &str, path: &PathBuf) -> Result<Recipe> {
    let expr = parse(content)
        .map_err(|e| anyhow::anyhow!("Parse error in {}: {}", path.display(), e))?;

    Recipe::from_expr(&expr)
        .map_err(|e| anyhow::anyhow!("Recipe error in {}: {}", path.display(), e))
}

/// Install a package WITH all its dependencies (build and runtime)
fn install_with_deps(
    package: &str,
    recipe_dir: &PathBuf,
    prefix: &PathBuf,
    verbose: bool,
    dry_run: bool,
    installed: &mut HashSet<String>,
) -> Result<()> {
    // Skip if already installed
    if installed.contains(package) {
        if verbose {
            println!("  [skip] {} (already installed)", package);
        }
        return Ok(());
    }

    // Find and parse the recipe
    let (path, content) = find_recipe(package, recipe_dir)?;
    let recipe = parse_recipe(&content, &path)?;

    // First, install BUILD dependencies (needed to compile this package)
    // For now, we install all unconditional deps - feature selection is TODO
    let build_dep_names: Vec<&str> = recipe.build_deps.iter()
        .filter_map(|d| match d {
            DepSpec::Always(dep) => Some(dep.name.as_str()),
            DepSpec::Conditional { .. } => None, // Skip conditional deps for now
        })
        .collect();

    if !build_dep_names.is_empty() {
        if verbose {
            println!("  [deps] {} requires build deps: {:?}", package, build_dep_names);
        }
        for dep in &build_dep_names {
            install_with_deps(dep, recipe_dir, prefix, verbose, dry_run, installed)?;
        }
    }

    // Then, install RUNTIME dependencies (needed at runtime)
    let runtime_dep_names: Vec<&str> = recipe.deps.iter()
        .filter_map(|d| match d {
            DepSpec::Always(dep) => Some(dep.name.as_str()),
            DepSpec::Conditional { .. } => None, // Skip conditional deps for now
        })
        .collect();

    if !runtime_dep_names.is_empty() {
        if verbose {
            println!("  [deps] {} requires runtime deps: {:?}", package, runtime_dep_names);
        }
        for dep in &runtime_dep_names {
            install_with_deps(dep, recipe_dir, prefix, verbose, dry_run, installed)?;
        }
    }

    // Now install the package itself
    println!("Installing {} {}...", recipe.name, recipe.version);

    let ctx = ExecContext::with_prefix(prefix)
        .verbose(verbose)
        .dry_run(dry_run);

    let executor = Executor::new(ctx);

    executor
        .execute(&recipe)
        .map_err(|e| anyhow::anyhow!("Build failed for {}: {}", package, e))?;

    // Mark as installed
    installed.insert(package.to_string());

    if !dry_run {
        println!("Installed {} {}", recipe.name, recipe.version);
    } else {
        println!("[dry-run] Would install {} {}", recipe.name, recipe.version);
    }

    Ok(())
}

/// Remove a package
fn remove_package(
    package: &str,
    recipe_dir: &PathBuf,
    prefix: &PathBuf,
    verbose: bool,
    dry_run: bool,
) -> Result<()> {
    let (path, content) = find_recipe(package, recipe_dir)?;
    let recipe = parse_recipe(&content, &path)?;

    println!("Removing {} {}...", recipe.name, recipe.version);

    let ctx = ExecContext::with_prefix(prefix)
        .verbose(verbose)
        .dry_run(dry_run);

    let executor = Executor::new(ctx);

    if let Some(ref remove_spec) = recipe.remove {
        executor
            .remove(remove_spec, &recipe)
            .map_err(|e| anyhow::anyhow!("Remove failed: {}", e))?;
    } else {
        bail!("Recipe {} does not have a remove section", recipe.name);
    }

    // Remove from installed db
    if !dry_run {
        let mut installed = load_installed_db();
        installed.remove(package);
        save_installed_db(&installed)?;
        println!("Removed {} {}", recipe.name, recipe.version);
    }

    Ok(())
}

/// List available packages
fn list_packages(recipe_dir: &PathBuf) -> Result<()> {
    if !recipe_dir.exists() {
        bail!("Recipe directory not found: {}", recipe_dir.display());
    }

    let installed = load_installed_db();
    let mut packages = Vec::new();

    for entry in fs::read_dir(recipe_dir)? {
        let entry = entry?;
        let path = entry.path();

        if path.extension().map(|e| e == "recipe").unwrap_or(false) {
            if let Ok(content) = fs::read_to_string(&path) {
                if let Ok(expr) = parse(&content) {
                    if let Ok(recipe) = Recipe::from_expr(&expr) {
                        let is_installed = installed.contains(&recipe.name);
                        packages.push((recipe.name, recipe.version, recipe.description, is_installed));
                    }
                }
            }
        }
    }

    packages.sort_by(|a, b| a.0.cmp(&b.0));

    if packages.is_empty() {
        println!("No recipes found in {}", recipe_dir.display());
    } else {
        println!("Available packages:\n");
        for (name, version, desc, is_installed) in packages {
            let desc = desc.as_deref().unwrap_or("");
            let status = if is_installed { "[*]" } else { "[ ]" };
            println!("  {} {:<20} {:<10} {}", status, name, version, desc);
        }
        println!("\n  [*] = installed");
    }

    Ok(())
}

/// Show package info
fn show_info(package: &str, recipe_dir: &PathBuf) -> Result<()> {
    let (path, content) = find_recipe(package, recipe_dir)?;
    let recipe = parse_recipe(&content, &path)?;

    let installed = load_installed_db();
    let status = if installed.contains(&recipe.name) { "installed" } else { "not installed" };

    println!("Package: {} ({})", recipe.name, status);
    println!("Version: {}", recipe.version);

    if let Some(desc) = &recipe.description {
        println!("Description: {}", desc);
    }
    if !recipe.license.is_empty() {
        println!("License: {}", recipe.license.join(", "));
    }
    if let Some(homepage) = &recipe.homepage {
        println!("Homepage: {}", homepage);
    }

    if !recipe.build_deps.is_empty() {
        let deps: Vec<String> = recipe.build_deps.iter().map(|d| format!("{}", d)).collect();
        println!("Build deps: {}", deps.join(", "));
    }
    if !recipe.deps.is_empty() {
        let deps: Vec<String> = recipe.deps.iter().map(|d| format!("{}", d)).collect();
        println!("Runtime deps: {}", deps.join(", "));
    }

    println!("Recipe: {}", path.display());

    Ok(())
}

/// Show dependency tree for a package
fn show_deps(package: &str, recipe_dir: &PathBuf) -> Result<()> {
    let mut visited = HashSet::new();
    println!("Dependency tree for {}:\n", package);
    show_deps_recursive(package, recipe_dir, 0, &mut visited)?;
    Ok(())
}

fn show_deps_recursive(
    package: &str,
    recipe_dir: &PathBuf,
    depth: usize,
    visited: &mut HashSet<String>,
) -> Result<()> {
    let indent = "  ".repeat(depth);

    if visited.contains(package) {
        println!("{}{} (circular)", indent, package);
        return Ok(());
    }
    visited.insert(package.to_string());

    let (path, content) = match find_recipe(package, recipe_dir) {
        Ok(r) => r,
        Err(_) => {
            println!("{}{} (MISSING RECIPE)", indent, package);
            return Ok(());
        }
    };

    let recipe = parse_recipe(&content, &path)?;
    let installed = load_installed_db();
    let status = if installed.contains(&recipe.name) { "*" } else { " " };

    println!("{}[{}] {} {}", indent, status, recipe.name, recipe.version);

    // Show build deps
    for dep in &recipe.build_deps {
        let dep_name = match dep {
            DepSpec::Always(d) => d.name.as_str(),
            DepSpec::Conditional { dep: d, feature } => {
                println!("{}  (build, if {}) {}", indent, feature, d.name);
                continue;
            }
        };
        print!("{}  (build) ", indent);
        show_deps_recursive(dep_name, recipe_dir, depth + 1, visited)?;
    }

    // Show runtime deps
    for dep in &recipe.deps {
        let dep_name = match dep {
            DepSpec::Always(d) => d.name.as_str(),
            DepSpec::Conditional { dep: d, feature } => {
                println!("{}  (if {}) {}", indent, feature, d.name);
                continue;
            }
        };
        show_deps_recursive(dep_name, recipe_dir, depth + 1, visited)?;
    }

    Ok(())
}

/// Base packages required for a minimal LevitateOS installation
const BASE_PACKAGES: &[&str] = &[
    "base",
    "linux",
    "linux-firmware",
    "systemd",
    "networkmanager",
    "bash",
    "coreutils",
    "util-linux",
    "recipe",
];

/// Bootstrap a complete base system to a target directory
fn bootstrap_system(
    target: &std::path::Path,
    recipe_dir: &PathBuf,
    verbose: bool,
    dry_run: bool,
) -> Result<()> {
    println!("Bootstrapping LevitateOS to {}...", target.display());

    // 1. Verify all base recipes exist
    let mut missing = Vec::new();
    for pkg in BASE_PACKAGES {
        let recipe_path = recipe_dir.join(format!("{}.recipe", pkg));
        if !recipe_path.exists() {
            missing.push(*pkg);
        }
    }
    if !missing.is_empty() {
        bail!(
            "Missing required base recipes: {}\nEnsure recipe-dir contains all base packages.",
            missing.join(", ")
        );
    }

    // 2. Create filesystem hierarchy
    let dirs = [
        "bin", "boot", "dev", "etc", "home", "lib", "lib64",
        "mnt", "opt", "proc", "root", "run", "sbin", "srv",
        "sys", "tmp", "usr", "var",
        "usr/bin", "usr/lib", "usr/lib64", "usr/share", "usr/local",
        "usr/share/recipe/recipes",
        "var/lib", "var/lib/recipe", "var/log", "var/tmp",
        "etc/systemd/system",
    ];
    for dir in dirs {
        let path = target.join(dir);
        if verbose {
            println!("mkdir -p {}", path.display());
        }
        if !dry_run {
            fs::create_dir_all(&path)?;
        }
    }

    // 3. Install base packages
    let mut installed: HashSet<String> = HashSet::new();
    for pkg in BASE_PACKAGES {
        if verbose {
            println!("\n==> Installing {}...", pkg);
        }
        install_with_deps_to_target(pkg, target, recipe_dir, &mut installed, verbose, dry_run)?;
    }

    // 4. Copy recipes to target for self-hosting
    let target_recipes = target.join("usr/share/recipe/recipes");
    if verbose {
        println!("\nCopying recipes to {}...", target_recipes.display());
    }
    if !dry_run {
        for entry in fs::read_dir(recipe_dir)? {
            let entry = entry?;
            if entry.path().extension().map(|e| e == "recipe").unwrap_or(false) {
                fs::copy(entry.path(), target_recipes.join(entry.file_name()))?;
            }
        }
    }

    // 5. Save installed database to target
    let db_path = target.join("var/lib/recipe/installed");
    if !dry_run {
        let contents: Vec<&str> = installed.iter().map(|s| s.as_str()).collect();
        fs::write(&db_path, contents.join("\n"))?;
    }

    println!("\nBootstrap complete!");
    println!("Installed {} packages to {}", BASE_PACKAGES.len(), target.display());
    Ok(())
}

/// Install a package with dependencies to a specific target directory
fn install_with_deps_to_target(
    package: &str,
    target: &std::path::Path,
    recipe_dir: &PathBuf,
    installed: &mut HashSet<String>,
    verbose: bool,
    dry_run: bool,
) -> Result<()> {
    // Skip if already installed
    if installed.contains(package) {
        if verbose {
            println!("  [skip] {} (already installed)", package);
        }
        return Ok(());
    }

    // Find and parse the recipe
    let (path, content) = find_recipe(package, recipe_dir)?;
    let recipe = parse_recipe(&content, &path)?;

    // Install build dependencies first
    for dep in &recipe.build_deps {
        let dep_name = match dep {
            DepSpec::Always(d) => &d.name,
            DepSpec::Conditional { .. } => continue,
        };
        install_with_deps_to_target(dep_name, target, recipe_dir, installed, verbose, dry_run)?;
    }

    // Install runtime dependencies
    for dep in &recipe.deps {
        let dep_name = match dep {
            DepSpec::Always(d) => &d.name,
            DepSpec::Conditional { .. } => continue,
        };
        install_with_deps_to_target(dep_name, target, recipe_dir, installed, verbose, dry_run)?;
    }

    // Execute recipe with target as prefix
    println!("Installing {} {}...", recipe.name, recipe.version);

    let ctx = ExecContext::with_prefix(target)
        .verbose(verbose)
        .dry_run(dry_run);

    let executor = Executor::new(ctx);

    executor
        .execute(&recipe)
        .map_err(|e| anyhow::anyhow!("Build failed for {}: {}", package, e))?;

    installed.insert(package.to_string());

    if !dry_run {
        println!("Installed {} {}", recipe.name, recipe.version);
    } else {
        println!("[dry-run] Would install {} {}", recipe.name, recipe.version);
    }

    Ok(())
}

