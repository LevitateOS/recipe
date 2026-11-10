use clap::{Parser, Subcommand};
use std::path::PathBuf;

/// The library owns option defaults; flags only expose them.
fn defaults() -> levitate_recipe::ExecutionOptions {
    levitate_recipe::ExecutionOptions::default()
}

#[derive(Parser)]
#[command(name = "recipe")]
#[command(about = "Local-first package manager using Rhai recipes")]
#[command(version)]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub(crate) command: Commands,

    /// Use JSON for execution/inspection results (default when piped; list, info and hash use text)
    #[arg(long, global = true)]
    #[arg(help_heading = "Output")]
    pub(crate) json: bool,

    /// Recipe collection used for name lookup and listing (the OS builder selects its own collection).
    #[arg(
        short = 'r',
        long,
        global = true,
        default_value_os_t = super::metadata::default_recipes_path()
    )]
    #[arg(help_heading = "Recipes")]
    pub(crate) recipes_path: PathBuf,

    /// Logical build destination and lock (default: stable destination under the state directory).
    #[arg(short, long, global = true)]
    #[arg(help_heading = "Build and state")]
    pub(crate) build_dir: Option<PathBuf>,

    /// Store resumable state here (default: the per-user application-state directory).
    #[arg(long, global = true)]
    #[arg(help_heading = "Build and state")]
    pub(crate) state_dir: Option<PathBuf>,

    /// Execution target OS (only native Linux is currently supported).
    #[arg(long, global = true, default_value_t = defaults().target_os)]
    #[arg(help_heading = "Target")]
    pub(crate) target_os: String,
    /// Execution target architecture (only native x86_64 is currently supported).
    #[arg(long, global = true, default_value_t = defaults().target_arch)]
    #[arg(help_heading = "Target")]
    pub(crate) target_arch: String,
    /// Parallel jobs requested by recipes; Ginux task limits still apply.
    #[arg(short = 'j', long, global = true, default_value_t = defaults().jobs)]
    #[arg(help_heading = "Build and state")]
    pub(crate) jobs: u16,
    /// Rerun acquisition, build and installation for the main recipe.
    #[arg(long, global = true)]
    #[arg(help_heading = "Rerun phases")]
    pub(crate) force_acquire: bool,
    /// Rerun build and installation for the main recipe.
    #[arg(long, global = true)]
    #[arg(help_heading = "Rerun phases")]
    pub(crate) force_build: bool,
    /// Rerun installation for the main recipe.
    #[arg(long, global = true)]
    #[arg(help_heading = "Rerun phases")]
    pub(crate) force_install: bool,

    /// Cap NEEDS.timeout_secs across the dependency graph (including silent work).
    #[arg(long, global = true)]
    #[arg(help_heading = "Resource limits")]
    pub(crate) timeout_secs: Option<u64>,

    /// Cap each recipe’s NEEDS.memory_mib: per-process virtual address space and storage budget.
    #[arg(long, global = true)]
    #[arg(help_heading = "Resource limits")]
    pub(crate) memory_mib: Option<u64>,
    /// Cap each recipe’s NEEDS.storage_max_objects.
    #[arg(long, global = true)]
    #[arg(help_heading = "Resource limits")]
    pub(crate) storage_max_objects: Option<u64>,
    /// Cap each recipe’s NEEDS.storage_max_bytes (also bounded by effective memory/metadata).
    #[arg(long, global = true)]
    #[arg(help_heading = "Resource limits")]
    pub(crate) storage_max_bytes: Option<u64>,

    /// Intersect each recipe’s NEEDS.network with these HOST:PORT endpoints (repeatable).
    #[arg(long = "network-host", global = true, value_name = "HOST:PORT")]
    #[arg(help_heading = "Network limits")]
    pub(crate) network_hosts: Vec<String>,
    /// Disable networking across the dependency graph.
    #[arg(long, global = true, conflicts_with = "network_hosts")]
    #[arg(help_heading = "Network limits")]
    pub(crate) no_network: bool,
    /// Cap cumulative sent and received network bytes in each recipe instance.
    #[arg(long, global = true)]
    #[arg(help_heading = "Network limits")]
    pub(crate) network_max_bytes: Option<u64>,

    /// Define scope constants (KEY=VALUE), injected into Rhai scope before execution
    #[arg(short, long = "define", global = true, value_name = "KEY=VALUE")]
    #[arg(help_heading = "Recipe inputs")]
    pub(crate) defines: Vec<String>,

    /// Import NAME=PATH as read-only /inputs/NAME; trees exclude root .git and target.
    #[arg(long = "input", global = true, value_name = "NAME=PATH")]
    #[arg(help_heading = "Recipe inputs")]
    pub(crate) inputs: Vec<String>,

    /// Write the full JSON result to a file instead of stdout
    #[arg(long, global = true)]
    #[arg(help_heading = "Output")]
    pub(crate) json_output: Option<PathBuf>,

    /// Emit machine-readable hook events (JSON objects) alongside the normal human-readable progress log.
    #[arg(long, global = true)]
    #[arg(help_heading = "Output")]
    pub(crate) machine_events: bool,

    /// Select an LLM profile from XDG `recipe/llm.toml` (under `[profiles.<name>]`) for autofix/LLM features.
    #[arg(long, global = true)]
    #[arg(help_heading = "AI-assisted repair")]
    pub(crate) llm_profile: Option<String>,

    /// Disable state/file recovery. Filesystem effects still occur.
    #[arg(long = "no-persist-state", global = true, default_value_t = false)]
    #[arg(help_heading = "Build and state")]
    pub(crate) no_persist_state: bool,
}

#[derive(Subcommand)]
pub(crate) enum Commands {
    /// Check a recipe's declared inputs, dependencies and permissions without running it
    #[command(after_help = "Examples:\n  recipe validate kitty\n  recipe validate ./recipes/kitty.rhai --input source=./source\n\nChecks declarations and required inputs without executing recipe code.\nRequires a compatible Ginux launcher. Use --json to see the full report.")]
    Validate { recipe: PathBuf },
    /// Show a recipe's permissions and resource limits without running it
    #[command(after_help = "Examples:\n  recipe inspect kitty\n  recipe inspect ./recipes/kitty.rhai --json\n\nShows declared inputs, outputs, network endpoints and effective resource limits.\nNo recipe code is executed. Requires a compatible Ginux launcher.")]
    Inspect { recipe: PathBuf },
    /// Explain why an operation is allowed or denied for a recipe or past run
    #[command(after_help = "Examples:\n  recipe explain kitty filesystem.path.read\n  recipe explain /path/to/retained-invocation network.tcp.connect\n\nA recipe explains declared rules; a retained invocation explains recorded\ndecisions. Missing observations do not imply that an operation succeeded.")]
    Explain { subject: PathBuf, operation: String },
    /// Compare the permissions and resource limits of two recipes
    #[command(after_help = "Example:\n  recipe diff ./old/kitty.rhai ./new/kitty.rhai\n\nCompares declared inputs, outputs, network endpoints, dependencies and limits.\nNo recipe code is executed. Requires a compatible Ginux launcher.")]
    Diff { old: PathBuf, new: PathBuf },
    /// Install into a recipe's owned output tree and publish its products
    #[command(
        after_help = "Examples (with your own recipe):\n  recipe install ./hello.rhai\n  recipe install hello --recipes-path ./my-recipes\n  recipe install ./hello.rhai --autofix --autofix-attempts 3\n\n<RECIPE> may be:\n  - an absolute path\n  - a relative path\n  - a recipe name resolved under --recipes-path\n  - the same name with .rhai appended under --recipes-path\n\nOS producers and user-software recipes use the same format and engine.\nThis command publishes outputs; it does not activate software on the running system."
    )]
    Install {
        /// Recipe path or recipe name. If the file is not found directly, recipe will also look in --recipes-path and try appending `.rhai`.
        recipe: PathBuf,

        /// Automatically attempt to patch build/install failures using the configured LLM provider.
        #[arg(long)]
        autofix: bool,

        /// Maximum number of patch attempts per failing install step.
        #[arg(long, default_value_t = levitate_recipe::AutoFixConfig::default().attempts)]
        autofix_attempts: u8,

        /// Working directory used for LLM invocation and patch application. Defaults to the git repository of the current directory, else the recipe's.
        #[arg(long)]
        autofix_cwd: Option<PathBuf>,

        /// Extra instructions appended to the built-in autofix prompt.
        #[arg(long)]
        autofix_prompt_file: Option<PathBuf>,

        /// Allowed roots for patched file paths (repeatable). If omitted, defaults to the repo root.
        #[arg(long = "autofix-allow-path")]
        autofix_allow_path: Vec<PathBuf>,
    },

    /// Run a recipe's remove hook on its owned output tree
    #[command(
        after_help = "Example (use the same recipe, inputs and destination as install):\n  recipe remove ./hello.rhai\n\nRemoval changes the retained output tree. It does not remove earlier published\ngenerations or software from the running system."
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
        after_help = "Examples:\n  recipe isinstalled kitty\n  recipe isinstalled ./recipes/kitty.rhai --no-persist-state\n\nUseful when debugging whether install state detection is correct."
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

    /// List recipe declarations in the selected collection, not installed applications
    #[command(after_help = "Examples:\n  recipe list\n  recipe list --recipes-path ./recipes")]
    List,

    /// Show recipe information
    #[command(after_help = "Examples:\n  recipe info kitty\n  recipe info ./recipes/kitty.rhai")]
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
