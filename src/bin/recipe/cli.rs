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

    /// Independent package storage/profile; overrides the system route.
    #[arg(long, global = true, help_heading = "Installed software")]
    pub(crate) package_root: Option<PathBuf>,

    /// Produce outputs only; do not activate packages or use the installed-package receipt.
    #[arg(long, global = true, help_heading = "Installed software")]
    pub(crate) no_activate: bool,

    /// System scope: install, update, remove and list software for the computer's root. On an A/B system the change is written into the other (unlocked) root and becomes active when that root is booted; the booted root is never written.
    #[arg(long, global=true, conflicts_with_all=["no_activate","package_root"], help_heading="Installed software")]
    pub(crate) system: bool,

    /// With --system: write the system change into DIR, a root that is not running (an installer's /mnt, an image tree), and read installed/state from it. No recab call, no A/B record. Given before the command: `recipe --system --root /mnt install NAME`.
    #[arg(
        long,
        value_name = "DIR",
        requires = "system",
        help_heading = "Installed software"
    )]
    pub(crate) root: Option<PathBuf>,

    /// With --system: do not run the root's effects recipe (usr/lib/recipe/effects.rhai) after the change; for a caller that runs it itself once at the end.
    #[arg(long, requires = "system", help_heading = "Installed software")]
    pub(crate) no_effects: bool,

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
    /// Where recipes run: auto (on the host when a recipe starts no program, else in Ginux), ginux, or host (refuse a recipe that starts a program).
    #[arg(long, global = true, default_value_t = defaults().execution, value_parser = clap::value_parser!(levitate_recipe::ExecutionMode))]
    #[arg(help_heading = "Build and state")]
    pub(crate) execution: levitate_recipe::ExecutionMode,
    /// Install or update a package whose build is linked against a C library other than this system's.
    #[arg(long, global = true)]
    #[arg(help_heading = "Target")]
    pub(crate) any_libc: bool,
    /// Install or update a package on a system its ABOUT.systems does not list (by os-release ID).
    #[arg(long, global = true)]
    #[arg(help_heading = "Target")]
    pub(crate) any_system: bool,

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

    /// Give the recipe the system root PATH as its read-only input `root`, bound read-only for the run instead of copied (as root, in Ginux; otherwise copied like --input root=PATH). `--system` passes it to the root's effects recipe.
    #[arg(long, global = true, value_name = "PATH", conflicts_with = "system")]
    #[arg(help_heading = "Recipe inputs")]
    pub(crate) effects_root: Option<PathBuf>,

    /// Write the full JSON result to a file instead of stdout
    #[arg(long, global = true)]
    #[arg(help_heading = "Output")]
    pub(crate) json_output: Option<PathBuf>,

    /// Emit machine-readable events (plan, step, fetch, progress, done, failed) (JSON objects) alongside the normal human-readable progress log.
    #[arg(long, global = true)]
    #[arg(help_heading = "Output")]
    pub(crate) machine_events: bool,

    /// Keep this file current with the running recipe's `progress()` reports (one JSON object, replaced atomically at most once a second).
    #[arg(long, global = true, value_name = "PATH")]
    #[arg(help_heading = "Output")]
    pub(crate) progress_file: Option<PathBuf>,

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
    /// Do every step still missing (download, build, install) and activate the package output, if declared
    #[command(
        after_help = "Examples:\n  recipe install ripgrep\n  recipe install ./hello.rhai\n  recipe install ./producer.rhai --no-activate\n\nRecipe decides what is done from its own records: downloads kept by SHA-256, a\nkept result with the current identity whose files still match, the receipt. A\nprogram installed from the same recipe file is not installed again. A directory\noutput named package is activated atomically in the selected package root's\nprofile; other outputs are published. OS builders use --no-activate. A failure\npreserves the old profile."
    )]
    Install {
        /// Recipe path or recipe name. If the file is not found directly, recipe will also look in --recipes-path and try appending `.rhai`.
        recipe: PathBuf,

        /// With --system: more recipes installed in the same run, each written once; one the root already holds from the same recipe (digest and version) is skipped.
        #[arg(value_name = "MORE")]
        more: Vec<PathBuf>,

        /// Run only if the recipe file has this SHA-256: what was looked at is what runs.
        #[arg(long, value_name = "HASH")]
        sha256: Option<String>,

        #[command(flatten)]
        keep: KeepFlags,
    },

    /// Update an installed package to the version pinned by its current recipe
    #[command(
        after_help = "Example:\n  recipe update ripgrep\n\nUses the current catalog recipe, or the installed source path if no catalog entry\nexists. It does not edit version pins or fetch an unverified latest release.\nThe previous package remains active until the replacement is active; then it is\ndeleted."
    )]
    Update {
        recipe: PathBuf,

        #[command(flatten)]
        keep: KeepFlags,
    },

    /// Download a recipe's sources and pins only (the download step)
    #[command(
        after_help = "Example:\n  recipe download gh\n\nThe downloads are kept until the program is built, installed or cleaned up\n('recipe cleanup NAME --downloads'). Nothing is done when Recipe keeps them already."
    )]
    Download {
        /// Recipe path or recipe name. Name lookup follows the same rules as `recipe install`.
        recipe: PathBuf,
    },

    /// Download what is missing, then build; the result is kept, not installed
    #[command(
        after_help = "Example:\n  recipe build kitty\n\nA kept result with the current identity whose files still match is not built\nagain. 'recipe install' installs the kept result; 'recipe cleanup NAME --build'\nremoves it."
    )]
    Build {
        /// Recipe path or recipe name. Name lookup follows the same rules as `recipe install`.
        recipe: PathBuf,

        #[command(flatten)]
        keep: KeepFlags,
    },

    /// List package receipts; with --system on an A/B system, the running root's software and the other root's
    #[command(
        after_help = "With --json each package also names what its recipe was allowed to use when it\nwas installed (needs), when (installed_on), the length of its files\n(size_bytes), what it holds beside them (on_disk) and whether the recipe as last\nfetched names another version (update)."
    )]
    Installed,

    /// Say which installed packages 'recipe update' would change, and what the newer recipe may use that the installed one may not
    #[command(
        after_help = "Examples:\n  recipe updates\n  recipe updates --json\n  recipe updates --upstream\n  recipe updates --upstream ripgrep\n\nReads what is on this computer: the installed receipts and the recipes of the\ncollection and the sources as they were last fetched. Nothing is fetched and\nnothing is run; 'recipe refresh' fetches the sources' lists first. A package\nhas an update when its recipe names another version, or when its source\npublishes another recipe file for it. \"changes\" compares NEEDS with what was\nrecorded when the package was installed, and is null when nothing was.\n\nWith --upstream (and optionally recipe names) Recipe also asks upstream, only now\nand only for this command: GitHub's newest release for a recipe with UPSTREAM, or\nthe recipe's latest() run on the host. It tells \"upstream has X, this recipe\ninstalls Y\" in a list of its own and never changes a recipe or installs a version\nthe recipe does not name. A failed question is an error field, not a failure."
    )]
    Updates {
        /// Also ask upstream (GitHub, or the recipe's latest()) which version is newest; the only way a command here reaches the network for it
        #[arg(long)]
        upstream: bool,

        /// With --upstream: ask only about these programs (recipe names; need not be installed)
        #[arg(value_name = "NAME", requires = "upstream")]
        names: Vec<String>,
    },

    /// Show what was done to the installed software, newest first
    #[command(
        after_help = "Examples:\n  recipe history\n  recipe history --json\n\nLists installs, updates and removals, from the log in the package root. The log\nkeeps the newest entries. With --json (and when piped) it prints\n{\"schema_version\": 1, \"history\": [...]}; each entry has at (seconds since 1970),\nwhat, package, from, to and source, null where one does not apply."
    )]
    History,

    /// Show everything Recipe holds on disk, by kind, and what removes each
    #[command(
        after_help = "Examples:\n  recipe storage\n  recipe storage --json\n\nSeven kinds, each byte counted once: installed (the package store), downloaded,\nbuilt, kept (material of installed programs with --keep), unfinished\n(published outputs no command took),\nbuild_image (the cached compiler image; 'recipe runtime remove') and logs\n('recipe cleanup --logs'). Nothing is changed."
    )]
    Storage,

    /// Remove an installed program by its receipt, and its downloads and build material
    #[command(
        after_help = "Examples:\n  recipe remove ripgrep\n  recipe remove ripgrep --keep\n  recipe remove ./producer.rhai --no-activate\n\nThe installed files are removed by the receipt; no recipe code runs and no\nGinux is needed. Then whatever is left of the program's downloads and build\nmaterial is deleted, unless --keep. A program that is not installed loses only\nits material."
    )]
    Remove {
        /// Recipe path or recipe name. Name lookup follows the same rules as `recipe install`.
        recipe: PathBuf,
        /// Remove only the installed program; keep its downloads and build material.
        #[arg(long)]
        keep: bool,
    },

    /// Delete downloads and build material; installed programs stay
    #[command(
        after_help = "Examples:\n  recipe cleanup kitty\n  recipe cleanup kitty --downloads\n  recipe cleanup kitty --build\n  recipe cleanup\n  recipe cleanup --logs\n\nWith a name: that program's downloads and kept results (and\nthose of the dependency runs made for it); then the recipe's own\ncleanup(\"manual\") runs when it defines one. Without a name: the same for every\nprogram, and the published outputs no running invocation holds. Material kept\nwith --keep is deleted too; the keep option stays on. The installed programs\nstay. --logs deletes the diagnostic logs instead."
    )]
    Cleanup {
        /// Recipe path or recipe name; without it, every program. Name lookup follows the same rules as `recipe install`.
        recipe: Option<PathBuf>,
        /// Only the downloads.
        #[arg(long, conflicts_with = "build")]
        downloads: bool,
        /// Only the build material: kept results.
        #[arg(long)]
        build: bool,
        /// The diagnostic logs of finished runs, instead of any program's material.
        #[arg(long, conflicts_with_all = ["recipe", "downloads", "build"])]
        logs: bool,
    },

    /// Print a program's state: none, downloaded, built or installed
    #[command(
        after_help = "Examples:\n  recipe state gh\n  recipe state gh --json\n\nDecided from Recipe's own records, never from recipe code: downloads kept by\nSHA-256, a kept result with the current identity whose files still match, the\nreceipt. With --json it also names the steps the recipe has and what it holds on\ndisk (on_disk). It exits 0 whatever the state."
    )]
    State {
        /// Recipe path or recipe name. Name lookup follows the same rules as `recipe install`.
        recipe: PathBuf,
    },

    /// List recipe declarations in the selected collection, not installed applications
    #[command(
        after_help = "Examples:\n  recipe list\n  recipe list --recipes-path ./recipes\n  recipe list --json\n\n--json prints one entry per recipe for a front end: name, version, description,\nABOUT, NEEDS, dependency names, steps, functions, upstream, and the state on this\nsystem. Nothing is run and no Ginux launcher is needed. A recipe that cannot be read is reported under \"problems\"."
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
        after_help = "Examples:\n  recipe runtime status\n  recipe runtime fetch\n  recipe runtime stage --root ./pkg --launcher ./ginux --reference oci:NAME@sha256:DIGEST --catalog ./recipes\n\nRecipe has two runtime images: the full compiler image, and the minimal image a\nrecipe with NEEDS.runtime \"minimal\" runs in (without one named, it runs in the\nfull image). An image is downloaded on first use when it is not on this\ncomputer; 'fetch' does it now. RECIPE_RUNTIME_URL names another address ({digest}\nand {short} are replaced). GINUX_ROOTFS and GINUX_MINIMAL_ROOTFS select local\nimages; their SHA-256 must match GINUX_ROOT and GINUX_MINIMAL_ROOT."
    )]
    Runtime {
        #[command(subcommand)]
        command: RuntimeCommands,
    },

    /// With --system: apply a fetched system release to a root (its base, then its group recipes, then the root's effects)
    #[command(
        name = "apply-release",
        after_help = "Examples:\n  recipe --system apply-release /var/cache/recupdate/7\n  recipe --system --root /mnt apply-release ./levitateos-7\n\nThe folder holds release.json, base.tar.zst and recipes.tar.zst as 'recupdate\nfetch' leaves them; the signature is checked by the caller, the archives'\nSHA-256 here. The root's entry `base` becomes the release's base tree (files the\nrelease drops are removed, configuration the administrator changed under /etc is\nkept with the new version beside it as NAME.recipe-new, the machine's own files\nand accounts stay), every package the root holds that the release has a recipe\nfor is installed from the release's revision of it, and the effects run once.\nOn an A/B system the other root is written; on a system without A/B, the\nrunning root (an interrupted run is finished by running it again)."
    )]
    ApplyRelease {
        /// The fetched release folder.
        folder: PathBuf,
    },

    /// With --system --root TREE: record TREE's base tree as the receipt entry `base` (image builders)
    #[command(
        name = "record-base",
        after_help = "Example:\n  recipe --system --root ./rootfs record-base --release ./release.json\n\nCopies the manifest to TREE/usr/lib/levitate/release.json and records every\nfile of the tree's base (all but var/, mount points, machine files and what\nother packages of the receipt own) with its digest, mode, owner and attributes,\nso that a later 'apply-release' knows which files are the base's. Run it as the\nlast change to the tree before the image is made from it."
    )]
    RecordBase {
        /// The release manifest of the image being built (os, version and serial are read).
        #[arg(long, value_name = "FILE")]
        release: PathBuf,
    },

    /// Show what a recipe declares, read without running it
    #[command(
        after_help = "Examples:\n  recipe info kitty\n  recipe info ./recipes/kitty.rhai --json\n\nReads the declarations (PACKAGE, SOURCES, FILES, PACKAGES with the pins file, ...)\nand refuses an old recipe form with the line to change. --json prints the entry\n'recipe list --json' gives for it."
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
    /// Show the selected runtime images, the launcher and the shipped sources
    Status,
    /// Download the selected runtime images now, unless they are already here
    Fetch,
    /// Delete the downloaded runtime image and its unpacked copies; it is downloaded again when a recipe needs it
    Remove,
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
        /// The minimal runtime image reference, oci:NAME@sha256:DIGEST, which a
        /// recipe with NEEDS.runtime "minimal" runs in (a root's effects recipe).
        #[arg(long, value_name = "REF")]
        minimal_reference: Option<String>,
        /// The minimal image tar to ship; without it packages download it on first use.
        #[arg(long, value_name = "FILE", requires = "minimal_reference")]
        minimal_image: Option<PathBuf>,
        /// The official recipes folder, with its index.json.
        #[arg(long, value_name = "DIR")]
        catalog: PathBuf,
        /// Also write etc/apparmor.d/recipe-ginux, which lets the launcher use
        /// a user namespace on hosts that restrict them (Ubuntu 23.10 and later).
        #[arg(long)]
        apparmor: bool,
    },
}

/// The keep option of install, update and build (docs/recipe-format.md,
/// section 6.4).
#[derive(clap::Args, Clone, Copy, Debug, Default)]
pub(crate) struct KeepFlags {
    /// Keep the downloads and build material after a successful install; remembered for this program.
    #[arg(long, conflicts_with = "no_keep")]
    pub(crate) keep: bool,
    /// Turn the keep option off, and delete kept material of an installed program.
    #[arg(long)]
    pub(crate) no_keep: bool,
}

impl KeepFlags {
    /// `Some(true)` for --keep, `Some(false)` for --no-keep, None to leave
    /// the remembered setting.
    pub(crate) fn choice(self) -> Option<bool> {
        match (self.keep, self.no_keep) {
            (true, _) => Some(true),
            (_, true) => Some(false),
            _ => None,
        }
    }
}
