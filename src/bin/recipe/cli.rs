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
    #[arg(short = 'r', long, global = true)]
    #[arg(help_heading = "Recipes")]
    pub(crate) recipes_path: Option<PathBuf>,

    /// Independent package storage/profile; overrides root's immutable-system route.
    #[arg(long, global = true, help_heading = "Installed software")]
    pub(crate) package_root: Option<PathBuf>,

    /// Produce outputs only; do not activate packages or use the installed-package receipt.
    #[arg(long, global = true, help_heading = "Installed software")]
    pub(crate) no_activate: bool,

    /// Apply/list system software through recab's prepared inactive system.
    #[arg(long, global=true, conflicts_with_all=["no_activate","package_root"], help_heading="Installed software")]
    pub(crate) system: bool,
    /// Inspect the prepared selection rather than the running selection.
    #[arg(long, global = true, help_heading = "Installed software")]
    pub(crate) prepared: bool,
    #[arg(
        long,
        global = true,
        requires = "revision",
        help_heading = "Installed software"
    )]
    pub(crate) preparation: Option<String>,
    #[arg(
        long,
        global = true,
        requires = "preparation",
        help_heading = "Installed software"
    )]
    pub(crate) revision: Option<u64>,

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
    /// Install or update a package whose build is linked against a C library other than this system's.
    #[arg(long, global = true)]
    #[arg(help_heading = "Target")]
    pub(crate) any_libc: bool,

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

    /// Keep this file current with the running recipe's `progress()` reports (one JSON object, replaced atomically at most once a second).
    #[arg(long, global = true, value_name = "PATH")]
    #[arg(help_heading = "Output")]
    pub(crate) progress_file: Option<PathBuf>,

    /// Select an LLM profile from XDG `recipe/llm.toml` (under `[profiles.<name>]`) for autofix/LLM features.
    #[arg(long, global = true)]
    #[arg(help_heading = "AI-assisted repair")]
    pub(crate) llm_profile: Option<String>,

    /// Disable state/file recovery. Filesystem effects still occur.
    #[arg(long = "no-persist-state", global = true, default_value_t = false)]
    #[arg(help_heading = "Build and state")]
    pub(crate) no_persist_state: bool,

    /// Reuse saved state only from byte-identical Recipe and Ginux (release builds); also RECIPE_EXACT_IDENTITY=1.
    #[arg(long, global = true)]
    #[arg(help_heading = "Build and state")]
    pub(crate) exact_identity: bool,
}

#[derive(Subcommand)]
pub(crate) enum Commands {
    /// Check a recipe's declared inputs, dependencies and permissions without running it
    #[command(
        after_help = "Examples:\n  recipe validate kitty\n  recipe validate ./recipes/kitty.rhai --input source=./source\n\nChecks declarations and required inputs without executing recipe code.\nRequires a compatible Ginux launcher. Use --json to see the full report."
    )]
    Validate { recipe: PathBuf },
    /// Show a recipe's permissions and resource limits without running it
    #[command(
        after_help = "Examples:\n  recipe inspect kitty\n  recipe inspect ./recipes/kitty.rhai --json\n\nShows declared inputs, outputs, network endpoints and effective resource limits.\nNo recipe code is executed. Requires a compatible Ginux launcher."
    )]
    Inspect { recipe: PathBuf },
    /// Explain why an operation is allowed or denied for a recipe or past run
    #[command(
        after_help = "Examples:\n  recipe explain kitty filesystem.path.read\n  recipe explain /path/to/retained-invocation network.tcp.connect\n\nA recipe explains declared rules; a retained invocation explains recorded\ndecisions. Missing observations do not imply that an operation succeeded."
    )]
    Explain { subject: PathBuf, operation: String },
    /// Compare the permissions and resource limits of two recipes
    #[command(
        after_help = "Example:\n  recipe diff ./old/kitty.rhai ./new/kitty.rhai\n\nCompares declared inputs, outputs, network endpoints, dependencies and limits.\nNo recipe code is executed. Requires a compatible Ginux launcher."
    )]
    Diff { old: PathBuf, new: PathBuf },
    /// Run a recipe and activate its package output, if declared
    #[command(
        after_help = "Examples:\n  recipe install ripgrep\n  recipe install ./hello.rhai\n  recipe install ./producer.rhai --no-activate\n\nRecipes use the same Ginux engine. A directory output named package is activated\natomically in the selected package root's profile; other outputs remain products.\nOS builders use --no-activate. Acquisition or activation failure preserves the old profile."
    )]
    Install {
        /// Recipe path or recipe name. If the file is not found directly, recipe will also look in --recipes-path and try appending `.rhai`.
        recipe: PathBuf,

        /// Run only if the recipe file has this SHA-256: what was looked at is what runs.
        #[arg(long, value_name = "HASH")]
        sha256: Option<String>,

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

    /// Update an installed package to the version pinned by its current recipe
    #[command(
        after_help = "Example:\n  recipe update ripgrep\n\nUses the current catalog recipe, or the installed source path if no catalog entry\nexists. It does not edit version pins or fetch an unverified latest release.\nThe previous package remains active until the replacement is ready."
    )]
    Update { recipe: PathBuf },

    /// List package receipts; the system route distinguishes active and prepared selection
    #[command(
        after_help = "With --json each package also names what its recipe was allowed to use when it\nwas installed (needs), when (installed_on), the length of its files (size_bytes)\nand the earlier versions 'recipe go-back' can return to (kept)."
    )]
    Installed,

    /// Say which installed packages 'recipe update' would change, and what the newer recipe may use that the installed one may not
    #[command(
        after_help = "Examples:\n  recipe updates\n  recipe updates --json\n\nReads what is on this computer: the installed receipts and the recipes of the\ncollection and the sources as they were last fetched. Nothing is fetched and\nnothing is run; 'recipe refresh' fetches the sources' lists first. A package\nhas an update when its recipe names another version, or when its source\npublishes another recipe file for it. \"changes\" compares NEEDS with what was\nrecorded when the package was installed, and is null when nothing was."
    )]
    Updates,

    /// Activate the earlier version of an installed package again
    #[command(name = "go-back")]
    #[command(
        after_help = "Examples:\n  recipe go-back ripgrep\n  recipe go-back ripgrep --to 14.1.0\n\nUses the files an earlier profile generation still holds; no recipe is run and\nGinux is not needed. The other packages stay as they are. The version left\nbehind stays kept, so 'recipe go-back' again returns to it. 'recipe installed\n--json' lists the kept versions; 'recipe storage --clear-earlier' deletes them."
    )]
    GoBack {
        /// The installed package's name.
        name: String,
        /// The kept version to return to (default: the newest that differs).
        #[arg(long, value_name = "VERSION")]
        to: Option<String>,
    },

    /// Show what was done to the installed software, newest first
    #[command(
        after_help = "Examples:\n  recipe history\n  recipe history --json\n\nLists installs, updates, removals, goings back, holds, releases and clearings of\nearlier versions, from the log in the package root. The log keeps the newest\nentries; it is not the earlier profile generations, which 'recipe storage\n--clear-earlier' deletes. With --json (and when piped) it prints\n{\"schema_version\": 1, \"history\": [...]}; each entry has at (seconds since 1970),\nwhat, package, from, to and source, null where one does not apply."
    )]
    History,

    /// Keep an installed package from being updated by accident
    #[command(
        after_help = "Example:\n  recipe hold ripgrep\n\n'recipe update' refuses a held package; 'recipe updates' still lists it, marked\nheld. Installing, removing and going back still work. Release it with\n'recipe unhold'. Needs neither the recipe nor Ginux."
    )]
    Hold {
        /// The installed package's name.
        name: String,
    },

    /// Let an installed package be updated again
    #[command(after_help = "Example:\n  recipe unhold ripgrep")]
    Unhold {
        /// The installed package's name.
        name: String,
    },

    /// Show what installed software and its earlier versions take on disk
    #[command(
        after_help = "Examples:\n  recipe storage\n  recipe storage --json\n  recipe storage --clear-earlier\n\nEarlier profile generations are what 'recipe go-back' returns to. A file that\nan earlier generation shares with the active one is counted with the active\none. --clear-earlier deletes every inactive generation: nothing installed\nchanges, and no package can be gone back on afterwards."
    )]
    Storage {
        /// Delete every inactive profile generation.
        #[arg(long)]
        clear_earlier: bool,
    },

    /// Remove an active package, or run an output-only recipe's remove hook
    #[command(
        after_help = "Examples:\n  recipe remove ripgrep\n  recipe remove ./producer.rhai --no-activate\n\nActive package removal uses its installed receipt and needs neither the recipe\nnor Ginux. Inactive profile generations and build caches are retained.\n--no-activate runs only the recipe's removal hook in its owned output tree."
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

    /// Check active package files, or an output-only recipe's installed state
    #[command(name = "isinstalled")]
    #[command(
        after_help = "Examples:\n  recipe isinstalled ripgrep\n  recipe isinstalled ./recipes/ripgrep.rhai --no-activate\n\nActive packages are checked without Ginux. Use --no-activate to run the producer's is_installed hook instead."
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
    #[command(
        after_help = "Examples:\n  recipe list\n  recipe list --recipes-path ./recipes\n  recipe list --json\n\n--json prints one entry per recipe for a front end: name, version, description,\nABOUT, NEEDS, dependency names and defined hooks. Nothing is run and no Ginux\nlauncher is needed. A recipe that cannot be read is reported under \"problems\"."
    )]
    List,

    /// Show, add or remove the sources recipes come from
    #[command(
        after_help = "Examples:\n  recipe source list\n  recipe source add https://github.com/OWNER/NAME\n  recipe source add 'https://example.org/OWNER/NAME/raw/{rev}/{path}' --branch main\n  recipe source add --path ~/recipes\n  recipe source remove NAME\n\nA remote source is held as its published index (see 'recipe index'); a folder\nis read as it is. Sources belong to the package root: root's are in\n/var/lib/recipe/sources, a person's in their own Recipe data. An explicit\n--recipes-path or $RECIPE_PATH uses no sources."
    )]
    Source {
        #[command(subcommand)]
        command: SourceCommands,
    },

    /// Fetch the index of every remote source again, or of one
    #[command(after_help = "Examples:\n  recipe refresh\n  recipe refresh levitate")]
    Refresh {
        /// The source to refresh (default: all remote sources).
        name: Option<String>,
    },

    /// Write the index a source of recipes publishes
    #[command(
        after_help = "Examples:\n  recipe index\n  recipe index ./recipes\n  recipe index --check\n\nWrites index.json in the folder: the folder's repo.toml, every recipe as\n'recipe list --json' prints it, the SHA-256 and size of each recipe file and of\nthe files beside it that share its name, and the commit they are at. Commit the\nresult. Nothing is written while a recipe cannot be read or a listed file\ndiffers from what is committed. The same commit always gives the same index."
    )]
    Index {
        /// Folder of recipes with a repo.toml, inside a Git repository.
        #[arg(default_value = ".")]
        folder: PathBuf,
        /// Write nothing; fail when index.json is missing or out of date.
        #[arg(long)]
        check: bool,
    },

    /// Show, fetch or stage the runtime Recipe runs recipes in (Ginux and its image)
    #[command(
        after_help = "Examples:\n  recipe runtime status\n  recipe runtime fetch\n  recipe runtime stage --root ./pkg --launcher ./ginux --reference oci:NAME@sha256:DIGEST --catalog ./recipes\n\nThe image is downloaded on first use when it is not on this computer; 'fetch'\ndoes it now. RECIPE_RUNTIME_URL names another address ({digest} and {short} are\nreplaced). GINUX_ROOTFS selects a local image; its SHA-256 must match."
    )]
    Runtime {
        #[command(subcommand)]
        command: RuntimeCommands,
    },

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

#[derive(Subcommand)]
pub(crate) enum SourceCommands {
    /// List the sources, and when each remote one was last fetched
    List,
    /// Add a remote source by its address, or a folder with --path
    Add {
        /// A repository on github.com or gitlab.com, or the address its files
        /// are fetched from with {rev} and {path} in it.
        #[arg(required_unless_present = "path", conflicts_with = "path")]
        address: Option<String>,
        /// The branch whose index is followed.
        #[arg(long, default_value = "main", conflicts_with = "path")]
        branch: String,
        /// Add a folder of recipes that is read as it is.
        #[arg(long, value_name = "FOLDER")]
        path: Option<PathBuf>,
        /// The name of a folder that has no repo.toml (default: the folder's name).
        #[arg(long, requires = "path")]
        name: Option<String>,
    },
    /// Remove a source; a source the image ships is switched off
    Remove { name: String },
}

#[derive(Subcommand)]
pub(crate) enum RuntimeCommands {
    /// Show the selected runtime image, the launcher and the shipped sources
    Status,
    /// Download the selected runtime image now, unless it is already here
    Fetch,
    /// Write a runtime tree (launcher, reference, sources, profile snippet) into a staging root
    Stage {
        /// The staging root; it may already hold other files.
        #[arg(long, value_name = "DIR")]
        root: PathBuf,
        /// Where the runtime lives under the root, as installed.
        #[arg(long, default_value = "/usr", value_name = "PREFIX")]
        prefix: PathBuf,
        /// The Ginux launcher to ship.
        #[arg(long, value_name = "FILE")]
        launcher: PathBuf,
        /// The runtime image reference, oci:NAME@sha256:DIGEST.
        #[arg(long, value_name = "REF")]
        reference: String,
        /// The image tar to ship; without it packages download the image on first use.
        #[arg(long, value_name = "FILE")]
        image: Option<PathBuf>,
        /// The official recipes folder, with its index.json.
        #[arg(long, value_name = "DIR")]
        catalog: PathBuf,
    },
}
