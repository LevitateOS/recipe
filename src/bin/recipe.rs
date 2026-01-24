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
use levitate_recipe::{RecipeEngine, deps, output, recipe_state};
use std::path::{Path, PathBuf};

/// Recipe metadata loaded from state - bundles common queries into one struct
#[derive(Debug, Default)]
struct RecipeMetadata {
    pub name: String,
    pub installed: bool,
    pub version: Option<String>,
    pub installed_version: Option<String>,
    pub description: Option<String>,
    pub deps: Vec<String>,
    pub installed_at: Option<i64>,
    pub installed_files: Option<Vec<String>>,
}

impl RecipeMetadata {
    /// Load metadata for a recipe from its state file
    fn load(path: &Path) -> Self {
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .map(|s| s.to_string())
            .unwrap_or_default();

        let installed: Option<bool> = recipe_state::get_var(path, "installed").unwrap_or(None);
        let version: Option<String> = recipe_state::get_var(path, "version").unwrap_or(None);
        let installed_version: Option<recipe_state::OptionalString> =
            recipe_state::get_var(path, "installed_version").unwrap_or(None);
        let description: Option<String> =
            recipe_state::get_var(path, "description").unwrap_or(None);
        let deps: Option<Vec<String>> = recipe_state::get_var(path, "deps").unwrap_or(None);
        let installed_at: Option<i64> = recipe_state::get_var(path, "installed_at").unwrap_or(None);
        let installed_files: Option<Vec<String>> =
            recipe_state::get_var(path, "installed_files").unwrap_or(None);

        Self {
            name,
            installed: installed == Some(true),
            version,
            installed_version: installed_version.and_then(|v| v.into()),
            description,
            deps: deps.unwrap_or_default(),
            installed_at,
            installed_files,
        }
    }

    /// Check if an upgrade is available
    fn has_upgrade(&self) -> bool {
        self.installed && self.version != self.installed_version
    }
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

        /// Show what would be installed without actually installing
        #[arg(short = 'n', long = "dry-run")]
        dry_run: bool,

        /// Fail if resolved versions don't match recipe.lock
        #[arg(long)]
        locked: bool,
    },

    /// Remove an installed package
    Remove {
        /// Package name
        package: String,

        /// Force removal even if other packages depend on this one
        #[arg(short, long)]
        force: bool,
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

    /// Compute hashes for a file
    Hash {
        /// Path to file
        file: PathBuf,
    },

    /// List orphaned packages (dependencies no longer needed)
    Orphans,

    /// Remove orphaned packages
    Autoremove {
        /// Actually remove (without this, just shows what would be removed)
        #[arg(long)]
        yes: bool,
    },

    /// Show dependency tree for a package
    Tree {
        /// Package name
        package: String,
    },

    /// Show why a package is installed (what depends on it)
    Why {
        /// Package name
        package: String,
    },

    /// Show impact of removing a package (what would break)
    Impact {
        /// Package name
        package: String,
    },

    /// Lock file management
    Lock {
        #[command(subcommand)]
        action: LockAction,
    },
}

#[derive(Subcommand)]
enum LockAction {
    /// Generate/update lock file from current resolved versions
    Update,

    /// Show lock file contents
    Show,

    /// Verify current recipes match lock file
    Verify,
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
        Commands::Install {
            package,
            with_deps,
            dry_run,
            locked,
        } => {
            use levitate_recipe::lockfile::LockFile;
            use owo_colors::OwoColorize;

            if with_deps || dry_run || locked {
                // Resolve dependencies
                let install_order = deps::resolve_deps(&package, &recipes_path)?;

                // Validate against lock file if --locked flag is used
                if locked {
                    let lock_path = recipes_path.join("recipe.lock");
                    if !lock_path.exists() {
                        anyhow::bail!(
                            "Lock file not found: {}. Run 'recipe lock update' first.",
                            lock_path.display()
                        );
                    }
                    let lock = LockFile::read(&lock_path)?;

                    // Build resolved versions list
                    let resolved: Vec<(String, String)> = install_order
                        .iter()
                        .filter_map(|(name, path)| {
                            let meta = RecipeMetadata::load(path);
                            meta.version.map(|v| (name.clone(), v))
                        })
                        .collect();

                    let mismatches = lock.validate_against(&resolved);
                    if !mismatches.is_empty() {
                        eprintln!("{}", "Lock file mismatch:".red().bold());
                        for (name, locked_ver, resolved_ver) in &mismatches {
                            eprintln!(
                                "  {} locked: {}, resolved: {}",
                                name, locked_ver, resolved_ver
                            );
                        }
                        anyhow::bail!(
                            "Version mismatch with lock file. Update lock file or use without --locked."
                        );
                    }
                    output::info("Lock file validated successfully");
                }

                if dry_run {
                    // Dry run mode: show what would happen
                    output::info(&format!("Would install (in order) for {}:", package.bold()));
                    println!();

                    let mut to_install = 0;
                    let mut already_installed = 0;

                    for (i, (name, path)) in install_order.iter().enumerate() {
                        let meta = RecipeMetadata::load(path);
                        let ver = meta.version.as_deref().unwrap_or("?");

                        if meta.installed {
                            println!(
                                "  {}. {} {} {}",
                                i + 1,
                                name.green(),
                                ver.dimmed(),
                                "(already installed)".green()
                            );
                            already_installed += 1;
                        } else {
                            println!("  {}. {} {}", i + 1, name.bold(), ver.cyan());
                            to_install += 1;
                        }
                    }

                    println!();
                    output::info(&format!(
                        "Total: {} package(s) to install, {} already satisfied",
                        to_install, already_installed
                    ));
                } else {
                    // Actually install
                    let uninstalled = deps::filter_uninstalled(install_order)?;

                    if uninstalled.is_empty() {
                        output::skip(&format!(
                            "{} and all dependencies already installed",
                            package
                        ));
                    } else {
                        let names: Vec<_> = uninstalled.iter().map(|(n, _)| n.as_str()).collect();
                        output::info(&format!(
                            "Installing {} package(s): {}",
                            names.len(),
                            names.join(", ")
                        ));

                        let engine =
                            create_engine(&cli.prefix, cli.build_dir.as_deref(), &recipes_path)?;
                        let total = uninstalled.len();
                        for (i, (name, path)) in uninstalled.into_iter().enumerate() {
                            let is_dependency = name != package;

                            output::action_numbered(i + 1, total, &format!("Installing {}", name));
                            engine.execute(&path)?;

                            // Mark as dependency AFTER successful install (not before)
                            // This ensures we don't leave orphan markers on failed installs
                            if is_dependency {
                                let _ = recipe_state::set_var(&path, "installed_as_dep", &true);
                            }
                        }
                    }
                }
            } else {
                let recipe_path = resolve_recipe(&package, &recipes_path)?;
                let engine = create_engine(&cli.prefix, cli.build_dir.as_deref(), &recipes_path)?;
                engine.execute(&recipe_path)?;
            }
        }

        Commands::Remove { package, force } => {
            use owo_colors::OwoColorize;

            let recipe_path = resolve_recipe(&package, &recipes_path)?;

            // Check for reverse dependencies (packages that depend on this one)
            let rdeps = deps::reverse_deps_installed(&package, &recipes_path)?;

            if !rdeps.is_empty() && !force {
                output::error(&format!(
                    "Cannot remove '{}' - required by:",
                    package.bold()
                ));
                for (name, _) in &rdeps {
                    eprintln!("  {} {}", "-".red(), name);
                }
                eprintln!();
                eprintln!(
                    "{}",
                    "Use --force to remove anyway (will break dependents)".yellow()
                );
                anyhow::bail!("Package has dependents");
            }

            if !rdeps.is_empty() && force {
                output::warning(&format!(
                    "Force removing '{}' - this may break {} dependent package(s)",
                    package.bold(),
                    rdeps.len()
                ));
            }

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
                for recipe_path in find_installed_recipes(&recipes_path) {
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
                let recipes = find_upgradable_recipes(&recipes_path);
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
                    let meta = RecipeMetadata::load(path);
                    if meta.installed {
                        println!("  {}. {} {}", i + 1, name.green(), "[installed]".dimmed());
                    } else {
                        println!("  {}. {}", i + 1, name);
                    }
                }
            } else {
                // Show direct dependencies only
                let recipe_path = resolve_recipe(&package, &recipes_path)?;
                let meta = RecipeMetadata::load(&recipe_path);

                output::info(&format!("Dependencies for {}:", package.bold()));
                if meta.deps.is_empty() {
                    println!("  {}", "(none)".dimmed());
                } else {
                    for dep in &meta.deps {
                        println!("  {} {}", "-".cyan(), dep);
                    }
                }
            }
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

        Commands::Orphans => {
            use owo_colors::OwoColorize;

            let orphans = deps::find_orphans(&recipes_path)?;

            if orphans.is_empty() {
                output::info("No orphaned packages found");
            } else {
                output::info(&format!("Found {} orphaned package(s):", orphans.len()));
                for (name, path) in &orphans {
                    let meta = RecipeMetadata::load(path);
                    println!(
                        "  {} {}",
                        name.yellow(),
                        meta.installed_version.as_deref().unwrap_or("").dimmed()
                    );
                }
                println!();
                println!(
                    "{}",
                    "Run 'recipe autoremove --yes' to remove these packages".dimmed()
                );
            }
        }

        Commands::Autoremove { yes } => {
            use owo_colors::OwoColorize;

            let orphans = deps::find_orphans(&recipes_path)?;

            if orphans.is_empty() {
                output::info("No orphaned packages to remove");
                return Ok(());
            }

            output::info(&format!("Found {} orphaned package(s):", orphans.len()));
            for (name, path) in &orphans {
                let meta = RecipeMetadata::load(path);
                println!(
                    "  {} {}",
                    name.yellow(),
                    meta.installed_version.as_deref().unwrap_or("").dimmed()
                );
            }

            if !yes {
                println!();
                println!(
                    "{}",
                    "Run with --yes to actually remove these packages".cyan()
                );
                return Ok(());
            }

            println!();
            let engine = create_engine(&cli.prefix, cli.build_dir.as_deref(), &recipes_path)?;
            let mut removed = 0;

            for (name, path) in orphans {
                output::action(&format!("Removing orphan: {}", name));
                match engine.remove(&path) {
                    Ok(()) => removed += 1,
                    Err(e) => output::warning(&format!("Failed to remove {}: {}", name, e)),
                }
            }

            output::success(&format!("Removed {} orphaned package(s)", removed));
        }

        Commands::Tree { package } => {
            use owo_colors::OwoColorize;

            fn print_tree(
                name: &str,
                recipes_path: &Path,
                prefix: &str,
                is_last: bool,
                visited: &mut std::collections::HashSet<String>,
            ) -> Result<()> {
                let branch = if is_last { "└── " } else { "├── " };
                let recipe_path = recipes_path.join(format!("{}.rhai", name));

                // Check if recipe exists
                if !recipe_path.exists() {
                    println!(
                        "{}{}{} {}",
                        prefix,
                        branch,
                        name.red(),
                        "(missing recipe)".red().dimmed()
                    );
                    return Ok(());
                }

                let meta = RecipeMetadata::load(&recipe_path);
                let name_colored = if meta.installed {
                    name.green().to_string()
                } else {
                    name.to_string()
                };

                println!(
                    "{}{}{}{}",
                    prefix,
                    branch,
                    name_colored,
                    meta.version
                        .as_ref()
                        .map(|v| format!(" {}", v.dimmed()))
                        .unwrap_or_default()
                );

                // Prevent cycles
                if visited.contains(name) {
                    return Ok(());
                }
                visited.insert(name.to_string());

                let new_prefix = format!("{}{}", prefix, if is_last { "    " } else { "│   " });

                for (i, dep) in meta.deps.iter().enumerate() {
                    let dep_name = dep.split_whitespace().next().unwrap_or(dep);
                    let is_last_dep = i == meta.deps.len() - 1;
                    print_tree(dep_name, recipes_path, &new_prefix, is_last_dep, visited)?;
                }

                Ok(())
            }

            let recipe_path = resolve_recipe(&package, &recipes_path)?;
            let meta = RecipeMetadata::load(&recipe_path);

            let name_colored = if meta.installed {
                package.green().to_string()
            } else {
                package.clone()
            };

            println!(
                "{}{}",
                name_colored,
                meta.version
                    .as_ref()
                    .map(|v| format!(" {}", v.dimmed()))
                    .unwrap_or_default()
            );

            let mut visited = std::collections::HashSet::new();
            visited.insert(package.clone());

            for (i, dep) in meta.deps.iter().enumerate() {
                let dep_name = dep.split_whitespace().next().unwrap_or(dep);
                let is_last = i == meta.deps.len() - 1;
                print_tree(dep_name, &recipes_path, "", is_last, &mut visited)?;
            }
        }

        Commands::Why { package } => {
            use owo_colors::OwoColorize;

            let rdeps = deps::reverse_deps(&package, &recipes_path)?;

            if rdeps.is_empty() {
                output::info(&format!(
                    "'{}' is not required by any other package",
                    package.bold()
                ));
            } else {
                output::info(&format!(
                    "'{}' is required by {} package(s):",
                    package.bold(),
                    rdeps.len()
                ));
                for name in &rdeps {
                    let recipe_path = recipes_path.join(format!("{}.rhai", name));
                    let meta = RecipeMetadata::load(&recipe_path);
                    if meta.installed {
                        println!("  {} {}", name.green(), "(installed)".dimmed());
                    } else {
                        println!("  {}", name);
                    }
                }
            }
        }

        Commands::Impact { package } => {
            use owo_colors::OwoColorize;

            // Recursively find all packages that would be affected
            fn find_all_impacted(
                pkg: &str,
                recipes_path: &Path,
                impacted: &mut std::collections::HashSet<String>,
            ) -> Result<()> {
                let rdeps = deps::reverse_deps(pkg, recipes_path)?;
                for rdep in rdeps {
                    if impacted.insert(rdep.clone()) {
                        find_all_impacted(&rdep, recipes_path, impacted)?;
                    }
                }
                Ok(())
            }

            let mut impacted = std::collections::HashSet::new();
            find_all_impacted(&package, &recipes_path, &mut impacted)?;

            if impacted.is_empty() {
                output::info(&format!(
                    "Removing '{}' would not affect any other packages",
                    package.bold()
                ));
            } else {
                // Filter to only installed packages
                let installed_impacted: Vec<_> = impacted
                    .iter()
                    .filter(|name| {
                        let recipe_path = recipes_path.join(format!("{}.rhai", name));
                        RecipeMetadata::load(&recipe_path).installed
                    })
                    .collect();

                if installed_impacted.is_empty() {
                    output::info(&format!(
                        "Removing '{}' would not affect any installed packages",
                        package.bold()
                    ));
                } else {
                    output::warning(&format!(
                        "Removing '{}' would break {} installed package(s):",
                        package.bold(),
                        installed_impacted.len()
                    ));
                    for name in &installed_impacted {
                        println!("  {} {}", name.red(), "(installed)".dimmed());
                    }
                }
            }
        }

        Commands::Lock { action } => {
            use levitate_recipe::lockfile::LockFile;
            use owo_colors::OwoColorize;

            let lock_path = recipes_path.join("recipe.lock");

            match action {
                LockAction::Update => {
                    // Scan all recipes and build lock file
                    let mut lock = LockFile::new();

                    for path in enumerate_recipes(&recipes_path) {
                        let meta = RecipeMetadata::load(&path);
                        if !meta.name.is_empty()
                            && let Some(ver) = meta.version
                        {
                            lock.add_package(meta.name, ver);
                        }
                    }

                    lock.update_metadata();
                    lock.write(&lock_path)?;

                    output::success(&format!(
                        "Updated lock file with {} package(s): {}",
                        lock.packages.len(),
                        lock_path.display()
                    ));
                }

                LockAction::Show => {
                    if !lock_path.exists() {
                        anyhow::bail!("Lock file not found: {}", lock_path.display());
                    }

                    let lock = LockFile::read(&lock_path)?;

                    if lock.packages.is_empty() {
                        output::info("Lock file is empty");
                    } else {
                        output::info(&format!("Lock file ({} packages):", lock.packages.len()));
                        for (name, version) in &lock.packages {
                            println!("  {} {}", name.bold(), version.cyan());
                        }
                        if let Some(generated) = &lock.metadata.generated {
                            println!();
                            println!("{}", format!("Generated: {}", generated).dimmed());
                        }
                    }
                }

                LockAction::Verify => {
                    if !lock_path.exists() {
                        anyhow::bail!("Lock file not found: {}", lock_path.display());
                    }

                    let lock = LockFile::read(&lock_path)?;
                    let mut mismatches = Vec::new();
                    let mut matches = 0;

                    for (name, locked_version) in &lock.packages {
                        let recipe_path = recipes_path.join(format!("{}.rhai", name));
                        if !recipe_path.exists() {
                            mismatches.push((
                                name.clone(),
                                locked_version.clone(),
                                "missing".to_string(),
                            ));
                            continue;
                        }

                        let meta = RecipeMetadata::load(&recipe_path);
                        match meta.version {
                            Some(v) if &v == locked_version => matches += 1,
                            Some(v) => mismatches.push((name.clone(), locked_version.clone(), v)),
                            None => mismatches.push((
                                name.clone(),
                                locked_version.clone(),
                                "unknown".to_string(),
                            )),
                        }
                    }

                    if mismatches.is_empty() {
                        output::success(&format!(
                            "Lock file verified: {} package(s) match",
                            matches
                        ));
                    } else {
                        output::error("Lock file verification failed:");
                        for (name, locked, current) in &mismatches {
                            eprintln!("  {} locked: {}, current: {}", name.red(), locked, current);
                        }
                        anyhow::bail!("{} package(s) do not match lock file", mismatches.len());
                    }
                }
            }
        }
    }

    Ok(())
}

/// Create a recipe engine with proper configuration
fn create_engine(
    prefix: &Path,
    build_dir: Option<&std::path::Path>,
    recipes_path: &Path,
) -> Result<RecipeEngine> {
    // Create or use provided build directory
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

    // Ensure prefix exists
    std::fs::create_dir_all(prefix)
        .with_context(|| format!("Failed to create prefix directory: {}", prefix.display()))?;

    let engine = RecipeEngine::new(prefix.to_path_buf(), build_dir)
        .with_recipes_path(recipes_path.to_path_buf());

    Ok(engine)
}

/// Validate a package name to prevent path traversal attacks
fn validate_package_name(package: &str) -> Result<()> {
    if package.is_empty() {
        anyhow::bail!("Package name cannot be empty");
    }

    // Package names must be simple identifiers (alphanumeric, underscore, hyphen)
    // This prevents path traversal attacks like "../../../etc/passwd"
    if !package
        .chars()
        .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
    {
        anyhow::bail!(
            "Invalid package name '{}': only alphanumeric characters, underscores, and hyphens are allowed",
            package
        );
    }

    Ok(())
}

/// Resolve a package name to a recipe path
fn resolve_recipe(package: &str, recipes_path: &Path) -> Result<PathBuf> {
    // If it's already a path (contains path separators or ends with .rhai), handle specially
    let is_explicit_path =
        package.contains('/') || package.contains('\\') || package.ends_with(".rhai");

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
fn find_installed_recipes(recipes_path: &Path) -> Vec<PathBuf> {
    if !recipes_path.exists() {
        return Vec::new();
    }

    enumerate_recipes(recipes_path)
        .filter(|path| RecipeMetadata::load(path).installed)
        .collect()
}

/// Find recipes with pending upgrades (version != installed_version)
fn find_upgradable_recipes(recipes_path: &Path) -> Vec<PathBuf> {
    find_installed_recipes(recipes_path)
        .into_iter()
        .filter(|path| RecipeMetadata::load(path).has_upgrade())
        .collect()
}

/// List all packages
fn list_packages(recipes_path: &Path) -> Result<()> {
    use owo_colors::OwoColorize;

    if !recipes_path.exists() {
        output::info(&format!("No recipes found in {}", recipes_path.display()));
        return Ok(());
    }

    let mut found = false;
    for path in enumerate_recipes(recipes_path) {
        let meta = RecipeMetadata::load(&path);

        let status = if meta.installed {
            if meta.has_upgrade() {
                format!(
                    "[installed: {}, {} available]",
                    meta.installed_version.as_deref().unwrap_or("?"),
                    meta.version.as_deref().unwrap_or("?").yellow()
                )
            } else {
                format!(
                    "[installed: {}]",
                    meta.installed_version.as_deref().unwrap_or("?")
                )
            }
        } else {
            format!("[available: {}]", meta.version.as_deref().unwrap_or("?"))
        };

        output::list_item(&meta.name, &status, meta.installed);
        found = true;
    }

    if !found {
        output::info(&format!("No recipes found in {}", recipes_path.display()));
    }

    Ok(())
}

/// Search for packages
fn search_packages(pattern: &str, recipes_path: &Path) -> Result<()> {
    use owo_colors::OwoColorize;

    if !recipes_path.exists() {
        output::info("No recipes found");
        return Ok(());
    }

    let pattern_lower = pattern.to_lowercase();
    let mut found = false;

    for path in enumerate_recipes(recipes_path) {
        let meta = RecipeMetadata::load(&path);

        if meta.name.to_lowercase().contains(&pattern_lower) {
            println!(
                "  {} {} {}",
                meta.name.bold(),
                meta.version.as_deref().unwrap_or("?").cyan(),
                meta.description.as_deref().unwrap_or("").dimmed()
            );
            found = true;
        }
    }

    if !found {
        output::info(&format!("No packages matching '{}' found", pattern));
    }

    Ok(())
}

/// Show package info
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
    if !meta.deps.is_empty() {
        println!("{:<12} {}", "Depends:".bold(), meta.deps.join(", "));
    }
    println!(
        "{:<12} {}",
        "Recipe:".bold(),
        recipe_path.display().to_string().dimmed()
    );
    println!();

    if meta.installed {
        println!("{:<12} {}", "Status:".bold(), "Installed".green());
        if let Some(ver) = &meta.installed_version {
            println!("{:<12} {}", "Installed:".bold(), ver);
        }
        if let Some(ts) = meta.installed_at {
            let datetime = chrono_lite(ts);
            println!("{:<12} {}", "Installed at:".bold(), datetime.dimmed());
        }
        if let Some(ref files) = meta.installed_files {
            println!("{:<12} {} files", "Files:".bold(), files.len());
            if files.len() <= 10 {
                for f in files {
                    println!("             {}", f.dimmed());
                }
            } else {
                for f in files.iter().take(5) {
                    println!("             {}", f.dimmed());
                }
                println!(
                    "             {} and {} more",
                    "...".dimmed(),
                    files.len() - 5
                );
            }
        }
    } else {
        println!("{:<12} {}", "Status:".bold(), "Not installed".yellow());
    }

    Ok(())
}

/// Simple timestamp to string conversion (ISO 8601 format)
fn chrono_lite(timestamp: i64) -> String {
    // Convert Unix timestamp to human-readable ISO 8601 format
    let unix_secs = timestamp as u64;
    let days_since_1970 = unix_secs / 86400;
    let secs_today = unix_secs % 86400;

    let hours = secs_today / 3600;
    let minutes = (secs_today % 3600) / 60;
    let seconds = secs_today % 60;

    // Calculate year/month/day
    let mut year: i64 = 1970;
    let mut remaining_days = days_since_1970 as i64;

    loop {
        let days_in_year = if (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0) {
            366
        } else {
            365
        };
        if remaining_days < days_in_year {
            break;
        }
        remaining_days -= days_in_year;
        year += 1;
    }

    let is_leap = (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0);
    let days_in_months: [i64; 12] = [
        31,
        if is_leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];

    let mut month = 1;
    for &days in &days_in_months {
        if remaining_days < days {
            break;
        }
        remaining_days -= days;
        month += 1;
    }

    let day = remaining_days + 1;

    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02} UTC",
        year, month, day, hours, minutes, seconds
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use leviso_cheat_test::{cheat_aware, cheat_reviewed};
    use std::path::Path;
    use tempfile::TempDir;

    // ==================== Package Name Validation ====================

    #[cheat_reviewed("Validation test - valid package name patterns accepted")]
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

    #[cheat_reviewed("Validation test - empty package name rejected")]
    #[test]
    fn test_empty_package_name() {
        let result = validate_package_name("");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("cannot be empty"));
    }

    #[cheat_aware(
        protects = "User's system is protected from path traversal attacks",
        severity = "CRITICAL",
        ease = "EASY",
        cheats = [
            "Skip package name validation entirely",
            "Allow '.' and '/' in package names",
            "Validate after path resolution instead of before"
        ],
        consequence = "Attacker runs 'recipe install ../../../etc/passwd' - reads/overwrites system files"
    )]
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

    #[cheat_aware(
        protects = "User's system is protected from shell injection via package names",
        severity = "CRITICAL",
        ease = "EASY",
        cheats = [
            "Only check for path separators, not other special chars",
            "Escape special chars instead of rejecting",
            "Allow some special chars 'for flexibility'"
        ],
        consequence = "Attacker runs 'recipe install pkg;rm -rf /' - shell injection"
    )]
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

    #[cheat_aware(
        protects = "User is protected from dot-based path traversal",
        severity = "CRITICAL",
        ease = "EASY",
        cheats = [
            "Only check for '..' at start, not in middle",
            "Allow single dots",
            "Check for dots after other validation"
        ],
        consequence = "Attacker uses '.hidden' or 'pkg.name' for path traversal"
    )]
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

    #[cheat_reviewed("Resolution test - simple package name resolves to .rhai file")]
    #[test]
    fn test_resolve_recipe_simple_name() {
        let (_dir, recipes_path) = create_test_recipes_dir();
        write_recipe(&recipes_path, "ripgrep", "let name = \"ripgrep\";");

        let result = resolve_recipe("ripgrep", &recipes_path);
        assert!(result.is_ok());
        assert!(result.unwrap().ends_with("ripgrep.rhai"));
    }

    #[cheat_reviewed("Resolution test - hyphenated package names work")]
    #[test]
    fn test_resolve_recipe_with_hyphen() {
        let (_dir, recipes_path) = create_test_recipes_dir();
        write_recipe(&recipes_path, "my-package", "let name = \"my-package\";");

        let result = resolve_recipe("my-package", &recipes_path);
        assert!(result.is_ok());
    }

    #[cheat_aware(
        protects = "User gets clear error when package not found",
        severity = "MEDIUM",
        ease = "EASY",
        cheats = [
            "Return empty path instead of error",
            "Create empty recipe on the fly",
            "Return success with null path"
        ],
        consequence = "User typos package name - confusing behavior instead of clear error"
    )]
    #[test]
    fn test_resolve_recipe_not_found() {
        let (_dir, recipes_path) = create_test_recipes_dir();
        let result = resolve_recipe("nonexistent", &recipes_path);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Recipe not found"));
    }

    #[cheat_aware(
        protects = "Path traversal in resolve_recipe is blocked",
        severity = "CRITICAL",
        ease = "EASY",
        cheats = [
            "Only validate in validate_package_name, skip in resolve_recipe",
            "Treat path separators as valid package name chars",
            "Resolve path before validation"
        ],
        consequence = "Attacker bypasses validate_package_name by going through resolve_recipe"
    )]
    #[test]
    fn test_resolve_recipe_path_traversal_rejected() {
        let (_dir, recipes_path) = create_test_recipes_dir();
        // Paths with "/" are treated as explicit paths
        let result = resolve_recipe("../../../etc/passwd", &recipes_path);
        assert!(result.is_err());
        // Explicit paths that don't exist return "not found"
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[cheat_aware(
        protects = "Package names with invalid chars are rejected at resolution time",
        severity = "HIGH",
        ease = "EASY",
        cheats = [
            "Skip validation for names that look like packages",
            "Only validate names with path separators",
            "Call validation after file lookup"
        ],
        consequence = "User bypasses validation - 'pkg!name' gets through"
    )]
    #[test]
    fn test_validate_package_name_called_for_simple_names() {
        // Package names without path separators go through validation
        let (_dir, recipes_path) = create_test_recipes_dir();
        // "pkg!name" has no "/" but has invalid char "!"
        let result = resolve_recipe("pkg!name", &recipes_path);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Invalid package name")
        );
    }

    #[cheat_reviewed("Resolution test - explicit .rhai paths accepted")]
    #[test]
    fn test_resolve_recipe_explicit_path() {
        let (_dir, recipes_path) = create_test_recipes_dir();
        let recipe_path = write_recipe(&recipes_path, "test", "let name = \"test\";");

        // Should accept explicit .rhai path
        let result = resolve_recipe(recipe_path.to_str().unwrap(), &recipes_path);
        assert!(result.is_ok());
    }

    #[cheat_reviewed("Resolution test - subdirectory style recipes found")]
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

    #[cheat_reviewed("Resolution test - direct file preferred over subdirectory")]
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

    #[cheat_reviewed("Query test - empty directory returns empty list")]
    #[test]
    fn test_find_installed_recipes_empty() {
        let (_dir, recipes_path) = create_test_recipes_dir();
        let result = find_installed_recipes(&recipes_path);
        assert!(result.is_empty());
    }

    #[cheat_aware(
        protects = "User gets accurate list of installed packages",
        severity = "MEDIUM",
        ease = "EASY",
        cheats = [
            "Return all recipes regardless of installed state",
            "Only check if file exists, not installed flag",
            "Return empty list always"
        ],
        consequence = "User runs 'recipe list' - sees wrong installed status"
    )]
    #[test]
    fn test_find_installed_recipes_finds_installed() {
        let (_dir, recipes_path) = create_test_recipes_dir();
        write_recipe(&recipes_path, "pkg1", "let installed = true;");
        write_recipe(&recipes_path, "pkg2", "let installed = false;");
        write_recipe(&recipes_path, "pkg3", "let installed = true;");

        let result = find_installed_recipes(&recipes_path);
        assert_eq!(result.len(), 2);
    }

    #[cheat_reviewed("Edge case - nonexistent directory returns empty list")]
    #[test]
    fn test_find_installed_recipes_nonexistent_dir() {
        let recipes_path = PathBuf::from("/nonexistent/path");
        let result = find_installed_recipes(&recipes_path);
        assert!(result.is_empty());
    }

    // ==================== Find Upgradable Recipes ====================

    #[cheat_aware(
        protects = "User sees accurate upgrade availability",
        severity = "HIGH",
        ease = "MEDIUM",
        cheats = [
            "Don't compare versions, mark all as upgradable",
            "Only check installed flag, not version mismatch",
            "Ignore installed_version field"
        ],
        consequence = "User runs 'recipe upgrade' - reinstalls everything or misses real updates"
    )]
    #[test]
    fn test_find_upgradable_recipes() {
        let (_dir, recipes_path) = create_test_recipes_dir();

        // Installed but up to date
        write_recipe(
            &recipes_path,
            "pkg1",
            r#"
let version = "1.0";
let installed = true;
let installed_version = "1.0";
"#,
        );

        // Installed with update available
        write_recipe(
            &recipes_path,
            "pkg2",
            r#"
let version = "2.0";
let installed = true;
let installed_version = "1.0";
"#,
        );

        // Not installed
        write_recipe(
            &recipes_path,
            "pkg3",
            r#"
let version = "1.0";
let installed = false;
"#,
        );

        let result = find_upgradable_recipes(&recipes_path);
        assert_eq!(result.len(), 1);
        assert!(result[0].to_string_lossy().contains("pkg2"));
    }

    // ==================== Chrono Lite ====================

    #[cheat_reviewed("Timestamp test - Unix epoch")]
    #[test]
    fn test_chrono_lite_epoch() {
        // Unix epoch should be 1970-01-01 00:00:00
        let result = super::chrono_lite(0);
        assert_eq!(result, "1970-01-01 00:00:00 UTC");
    }

    #[cheat_reviewed("Timestamp test - known date calculation")]
    #[test]
    fn test_chrono_lite_known_date() {
        // 2024-01-15 12:40:45 UTC = 1705322445
        // Calculation: 1705322445 % 86400 = 45645 secs into day
        // 45645 / 3600 = 12 hours, (45645 % 3600) / 60 = 40 mins, 45645 % 60 = 45 secs
        let result = super::chrono_lite(1705322445);
        assert_eq!(result, "2024-01-15 12:40:45 UTC");
    }

    #[cheat_reviewed("Timestamp test - leap year handling")]
    #[test]
    fn test_chrono_lite_leap_year() {
        // 2024-02-29 00:00:00 UTC = 1709164800 (2024 is a leap year)
        let result = super::chrono_lite(1709164800);
        assert_eq!(result, "2024-02-29 00:00:00 UTC");
    }

    #[cheat_reviewed("Timestamp test - year boundary")]
    #[test]
    fn test_chrono_lite_year_boundary() {
        // 2023-12-31 23:59:59 UTC = 1704067199
        let result = super::chrono_lite(1704067199);
        assert_eq!(result, "2023-12-31 23:59:59 UTC");
    }
}
