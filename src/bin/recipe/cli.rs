use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "recipe")]
#[command(about = "Local-first package manager using Rhai recipes")]
#[command(version)]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub(crate) command: Commands,

    /// Recipes directory used for name lookup, listing, and default recipe resolution.
    #[arg(
        short = 'r',
        long,
        global = true,
        default_value_os_t = super::metadata::default_recipes_path()
    )]
    pub(crate) recipes_path: PathBuf,

    /// Build directory used for acquire/build/install work; uses a temporary directory if omitted.
    #[arg(short, long, global = true)]
    pub(crate) build_dir: Option<PathBuf>,

    /// Define scope constants (KEY=VALUE), injected into Rhai scope before execution
    #[arg(short, long = "define", global = true, value_name = "KEY=VALUE")]
    pub(crate) defines: Vec<String>,

    /// Write final ctx JSON to a file instead of stdout; useful when scripting around recipe output.
    #[arg(long, global = true)]
    pub(crate) json_output: Option<PathBuf>,

    /// Emit machine-readable hook events (JSON objects) alongside the normal human-readable progress log.
    #[arg(long, global = true)]
    pub(crate) machine_events: bool,

    /// Select an LLM profile from XDG `recipe/llm.toml` (under `[profiles.<name>]`) for autofix/LLM features.
    #[arg(long, global = true)]
    pub(crate) llm_profile: Option<String>,

    /// Do not persist updated ctx back into recipe source files; useful for inspection and dry debugging.
    #[arg(long, global = true, default_value_t = false)]
    pub(crate) no_persist_ctx: bool,
}

#[derive(Subcommand)]
pub(crate) enum Commands {
    /// Install a package from a recipe file
    #[command(
        after_help = "Examples:\n  recipe install kitty\n  recipe install kitty.rhai\n  recipe install ./recipes/kitty.rhai\n  recipe install kitty --autofix --autofix-attempts 3\n\n<RECIPE> may be:\n  - an absolute path\n  - a relative path\n  - a recipe name resolved under --recipes-path\n  - the same name with .rhai appended under --recipes-path"
    )]
    Install {
        /// Recipe path or recipe name. If the file is not found directly, recipe will also look in --recipes-path and try appending `.rhai`.
        recipe: PathBuf,

        /// Automatically attempt to patch build/install failures using the configured LLM provider.
        #[arg(long)]
        autofix: bool,

        /// Maximum number of patch attempts per failing install step.
        #[arg(long, default_value_t = 2)]
        autofix_attempts: u8,

        /// Working directory used for LLM invocation and patch application. Defaults to the current working directory.
        #[arg(long)]
        autofix_cwd: Option<PathBuf>,

        /// Extra instructions appended to the built-in autofix prompt.
        #[arg(long)]
        autofix_prompt_file: Option<PathBuf>,

        /// Allowed roots for patched file paths (repeatable). If omitted, defaults to the repo root.
        #[arg(long = "autofix-allow-path")]
        autofix_allow_path: Vec<PathBuf>,
    },

    /// Remove an installed package
    #[command(
        after_help = "Examples:\n  recipe remove kitty\n  recipe remove ./recipes/kitty.rhai"
    )]
    Remove {
        /// Recipe path or recipe name. Name lookup follows the same rules as `recipe install`.
        recipe: PathBuf,
    },

    /// Clean up build artifacts
    #[command(
        after_help = "Examples:\n  recipe cleanup kitty\n  recipe cleanup kitty --reason manual\n  recipe cleanup ./recipes/kitty.rhai --build-dir /tmp/recipe-build"
    )]
    Cleanup {
        /// Recipe path or recipe name. Name lookup follows the same rules as `recipe install`.
        recipe: PathBuf,

        /// Cleanup reason passed to `cleanup(ctx, reason)`. Defaults to `manual`.
        #[arg(long)]
        reason: Option<String>,
    },

    /// Evaluate the recipe's `is_installed(ctx)` hook manually
    #[command(name = "isinstalled")]
    #[command(
        after_help = "Examples:\n  recipe isinstalled kitty\n  recipe isinstalled ./recipes/kitty.rhai --no-persist-ctx\n\nUseful when debugging whether install state detection is correct."
    )]
    IsInstalled {
        /// Recipe path or recipe name. Name lookup follows the same rules as `recipe install`.
        recipe: PathBuf,
    },

    /// Evaluate the recipe's `is_built(ctx)` hook manually
    #[command(name = "isbuilt")]
    #[command(
        after_help = "Examples:\n  recipe isbuilt kitty\n  recipe isbuilt ./recipes/kitty.rhai --build-dir /tmp/recipe-build\n\nUseful when debugging whether build outputs are being detected correctly."
    )]
    IsBuilt {
        /// Recipe path or recipe name. Name lookup follows the same rules as `recipe install`.
        recipe: PathBuf,
    },

    /// Evaluate the recipe's `is_acquired(ctx)` hook manually
    #[command(name = "isacquired")]
    #[command(
        after_help = "Examples:\n  recipe isacquired kitty\n  recipe isacquired ./recipes/kitty.rhai --build-dir /tmp/recipe-build\n\nUseful when debugging source/download state detection."
    )]
    IsAcquired {
        /// Recipe path or recipe name. Name lookup follows the same rules as `recipe install`.
        recipe: PathBuf,
    },

    /// List recipes in directory
    #[command(
        after_help = "Examples:\n  recipe list\n  recipe list --recipes-path ./recipes"
    )]
    List,

    /// Show recipe information
    #[command(
        after_help = "Examples:\n  recipe info kitty\n  recipe info ./recipes/kitty.rhai"
    )]
    Info {
        /// Recipe path or recipe name. Name lookup follows the same rules as `recipe install`.
        recipe: PathBuf,
    },

    /// Compute hashes for a file
    #[command(
        after_help = "Examples:\n  recipe hash ./downloads/foo.tar.xz\n\nPrints sha256, sha512, and blake3 values for use in recipe acquire() steps."
    )]
    Hash {
        /// File path to hash.
        file: PathBuf,
    },
}
