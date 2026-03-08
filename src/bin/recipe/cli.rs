use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "recipe")]
#[command(about = "Local-first package manager using Rhai recipes")]
#[command(version)]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub(crate) command: Commands,

    /// Path to recipes directory
    #[arg(
        short = 'r',
        long,
        global = true,
        default_value_os_t = super::metadata::default_recipes_path()
    )]
    pub(crate) recipes_path: PathBuf,

    /// Build directory (uses temp dir if not specified)
    #[arg(short, long, global = true)]
    pub(crate) build_dir: Option<PathBuf>,

    /// Define scope constants (KEY=VALUE), injected into Rhai scope before execution
    #[arg(short, long = "define", global = true, value_name = "KEY=VALUE")]
    pub(crate) defines: Vec<String>,

    /// Write JSON output to file instead of stdout (keeps stdout clean for build output)
    #[arg(long, global = true)]
    pub(crate) json_output: Option<PathBuf>,

    /// Emit machine-readable hook events (JSON objects) in addition to human-readable hook logs.
    #[arg(long, global = true)]
    pub(crate) machine_events: bool,

    /// Select an LLM profile from XDG `recipe/llm.toml` (under `[profiles.<name>]`).
    #[arg(long, global = true)]
    pub(crate) llm_profile: Option<String>,

    /// Do not persist updated ctx back into recipe source files.
    #[arg(long, global = true, default_value_t = false)]
    pub(crate) no_persist_ctx: bool,
}

#[derive(Subcommand)]
pub(crate) enum Commands {
    /// Install a package from a recipe file
    Install {
        /// Path to recipe file
        recipe: PathBuf,

        /// Automatically attempt to fix build/install failures using the configured LLM provider.
        #[arg(long)]
        autofix: bool,

        /// Maximum number of patch attempts per failing install.
        #[arg(long, default_value_t = 2)]
        autofix_attempts: u8,

        /// Working directory used for LLM invocation and patch application.
        #[arg(long)]
        autofix_cwd: Option<PathBuf>,

        /// Optional extra instructions appended to the built-in autofix prompt.
        #[arg(long)]
        autofix_prompt_file: Option<PathBuf>,

        /// Allowed roots for patched file paths (repeatable). If omitted, defaults to repo root.
        #[arg(long = "autofix-allow-path")]
        autofix_allow_path: Vec<PathBuf>,
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

        /// Cleanup reason passed to cleanup(ctx, reason)
        #[arg(long)]
        reason: Option<String>,
    },

    /// Run is_installed(ctx) manually
    #[command(name = "isinstalled")]
    IsInstalled {
        /// Path to recipe file
        recipe: PathBuf,
    },

    /// Run is_built(ctx) manually
    #[command(name = "isbuilt")]
    IsBuilt {
        /// Path to recipe file
        recipe: PathBuf,
    },

    /// Run is_acquired(ctx) manually
    #[command(name = "isacquired")]
    IsAcquired {
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
