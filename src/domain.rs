//! The Ginux domain layout shared by the coordinator and the in-domain worker.
//!
//! Every path a recipe, the worker or the coordinator names inside the domain
//! is defined here. Coordinator code builds the domain from these values and
//! worker code reads them, so neither side spells a path itself.

macro_rules! inputs_root {
    () => {
        "/inputs"
    };
}
macro_rules! main_recipe_name {
    () => {
        "main.rhai"
    };
}
macro_rules! worker_name {
    () => {
        "recipe-worker"
    };
}
macro_rules! input_modes_name {
    () => {
        ".recipe-input-modes.json"
    };
}

/// Read-only root holding every staged input.
pub const INPUTS: &str = inputs_root!();
/// Staged name of the invoked recipe.
pub const MAIN_RECIPE_NAME: &str = main_recipe_name!();
/// Domain path of the invoked recipe; also the identity of its saved state.
pub const MAIN_RECIPE: &str = concat!(inputs_root!(), "/", main_recipe_name!());
/// Staged name of the worker executable.
pub const WORKER_NAME: &str = worker_name!();
/// Domain path of the worker executable (the domain init).
pub const WORKER: &str = concat!(inputs_root!(), "/", worker_name!());
/// Staged name of the input permission manifest ([`InputModes`]).
pub const INPUT_MODES_NAME: &str = input_modes_name!();
/// Domain path of the input permission manifest.
pub const INPUT_MODES: &str = concat!(inputs_root!(), "/", input_modes_name!());
/// Input holding each dependency's published outputs, as `DEP/OUTPUT`. The
/// coordinator stages it; a recipe may not declare it in `NEEDS.inputs`.
pub const DEPENDENCY_INPUT: &str = "recipe-dependencies";
/// Input holding the dependencies' merged `tools` outputs; staged like
/// [`DEPENDENCY_INPUT`] and seeded into [`TOOLS`].
pub const DEPENDENCY_TOOLS: &str = "recipe-tools";
/// Input names the coordinator writes itself; nothing else may stage them.
pub const RESERVED_INPUT_NAMES: [&str; 2] = [WORKER_NAME, INPUT_MODES_NAME];

/// Logical input name → permission bits, written into [`INPUT_MODES`].
pub type InputModes = std::collections::BTreeMap<String, u32>;
/// Upper bound on the serialized [`InputModes`] manifest.
pub const MAX_INPUT_MODES_BYTES: u64 = 32 * 1024 * 1024;

/// Writable scratch filesystem root.
pub const WORK: &str = "/work";
/// The user id domain processes run as (and own the files they write).
pub const UID: u64 = 1000;
/// The main recipe's build directory.
pub const BUILD: &str = "/work/build";
/// Shared install prefix for dependency tools.
pub const TOOLS: &str = "/work/tools";
/// Tool executable directories below [`TOOLS`], in PATH order.
pub const TOOL_BIN_DIRS: [&str; 4] = ["usr/bin", "usr/sbin", "bin", "sbin"];
/// Temporary directory (`TMPDIR`).
pub const TMP: &str = "/work/tmp";
/// Installed-target and product tree. Published outputs come from it or from [`WORK`].
pub const OUT: &str = "/out";
/// Every writable directory the domain gets, parents first.
pub const WRITABLE_DIRS: [&str; 5] = [WORK, BUILD, TOOLS, TMP, OUT];
/// Top-level names of the writable trees, as they appear in the export archive.
pub const WRITABLE_ROOTS: [&str; 2] = ["work", "out"];
/// Top-level names a root image may not provide.
pub const RESERVED_ROOTS: [&str; 3] = ["inputs", "work", "out"];

/// Longest domain path the launcher accepts (its path buffer minus the NUL).
pub const MAX_PATH: usize = 255;
/// Longest staged input name, so `/inputs/NAME` fits [`MAX_PATH`].
pub const MAX_INPUT_NAME: usize = MAX_PATH - INPUTS.len() - 1;

/// Private argv marker the coordinator passes to the staged worker.
pub const WORKER_ENTRY_ARG: &str = "--__recipe-worker";
/// Worker argv after argv[0] (the launcher supplies argv[0] from the init).
pub const WORKER_ARGS: [&str; 2] = [WORKER_ENTRY_ARG, MAIN_RECIPE];
/// Worker argv after argv[0] for a host-mode worker (`crate::execution`). A
/// worker that predates host mode refuses it at its bootstrap check instead
/// of running domain paths on the host.
pub const WORKER_HOST_ARGS: [&str; 2] = [WORKER_ENTRY_ARG, "--host"];
/// Environment flag that marks a coordinator-started worker.
pub const ENV_WORKER: &str = "RECIPE_WORKER";
/// Environment variable carrying the invocation id into the worker.
pub const ENV_INVOCATION: &str = "RECIPE_INVOCATION";
/// The pid of the coordinator that started a host-mode worker: the worker
/// ends as soon as that is no longer its parent (`crate::worker`).
pub const ENV_COORDINATOR: &str = "RECIPE_COORDINATOR_PID";
/// The value of [`ENV_WORKER`] in a coordinator-started worker.
pub const ENV_WORKER_VALUE: &str = "1";

/// Longest invocation id or output name.
pub const MAX_NAME: usize = 64;

/// The rule for invocation ids and output names, `[A-Za-z0-9_-]{1,64}`: they
/// travel in the environment, result records and host paths.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Network endpoints a domain may be granted (the Ginux `[network]` limit).
pub const MAX_NETWORK_ENDPOINTS: usize = 8;
/// Concurrent mediated connections a domain gets.
pub const MAX_NETWORK_CONNECTIONS: usize = 16;
/// Longest execution or frame-stall deadline, in seconds.
pub const MAX_TIMEOUT_SECS: u64 = 86_400;

/// Domain path of a staged input.
pub fn input_path(name: &str) -> String {
    format!("{INPUTS}/{name}")
}

/// Staged input name of a domain path under [`INPUTS`].
pub fn input_name(path: &str) -> Option<&str> {
    path.strip_prefix(INPUTS)?.strip_prefix('/')
}

/// Tool executable directories as absolute domain paths, in PATH order.
pub fn tool_bin_dirs() -> impl Iterator<Item = String> {
    TOOL_BIN_DIRS.iter().map(|dir| format!("{TOOLS}/{dir}"))
}

/// Domain-path bounds, traversal and reserved names. Source filenames may use
/// spaces, punctuation and Unicode; they are never interpolated into commands.
pub fn validate_input_name(name: &str) -> anyhow::Result<()> {
    if name.is_empty() || name.len() > MAX_INPUT_NAME {
        anyhow::bail!(
            "input preparation failure: input name {name:?} has {} B, want 1..={MAX_INPUT_NAME} B \
             (so `/inputs/` + NAME fits the launcher path bound)",
            name.len(),
        );
    }
    if name
        .split('/')
        .any(|part| part.is_empty() || part == "." || part == "..")
        || name
            .split('/')
            .next()
            .is_some_and(|first| RESERVED_INPUT_NAMES.contains(&first))
    {
        anyhow::bail!("input preparation failure: input name {name:?} is reserved");
    }
    if name.chars().any(char::is_control) {
        anyhow::bail!("input preparation failure: input name {name:?} contains control characters");
    }
    Ok(())
}

/// The rule for a published path, relative to the domain root: bounded, no
/// traversal, and inside a writable tree.
pub fn validate_export_path(path: &str) -> anyhow::Result<()> {
    if path.len() > MAX_PATH
        || path.contains('\0')
        || path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        anyhow::bail!("invalid export path {path:?}");
    }
    if !path
        .split('/')
        .next()
        .is_some_and(|root| WRITABLE_ROOTS.contains(&root))
    {
        anyhow::bail!("output is outside the writable domain filesystem: {path:?}");
    }
    Ok(())
}

/// The host directory standing in for the domain root in a host-mode worker
/// (`crate::execution`), set once from the request before recipe code runs.
/// Unset in a Ginux worker, where the domain paths above are real.
static HOST_ROOT: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();

/// Select the host root of this worker process. Only the worker calls it,
/// once, before it loads a recipe.
pub fn set_host_root(root: std::path::PathBuf) -> anyhow::Result<()> {
    anyhow::ensure!(
        root.is_absolute() && root.parent().is_some(),
        "host root must be an absolute directory"
    );
    HOST_ROOT
        .set(root)
        .map_err(|_| anyhow::anyhow!("host root is already set"))
}

/// The host root when this worker runs on the host.
pub fn host_root() -> Option<&'static std::path::Path> {
    HOST_ROOT.get().map(std::path::PathBuf::as_path)
}

/// Where the worker reaches the domain path `path` (one of the constants
/// above, or a path below them): the path itself in Ginux, the same path
/// below the host root on the host.
pub fn local(path: &str) -> std::path::PathBuf {
    match host_root() {
        Some(root) => root.join(path.trim_start_matches('/')),
        None => std::path::PathBuf::from(path),
    }
}

/// The domain path of a worker path, the inverse of [`local`]. None for a path
/// outside the host root on the host.
pub fn domain_path(path: &std::path::Path) -> Option<String> {
    match host_root() {
        Some(root) => {
            let relative = path.strip_prefix(root).ok()?;
            Some(format!("/{}", relative.to_str()?))
        }
        None => path.to_str().map(str::to_owned),
    }
}
