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
/// Input names the coordinator writes itself; callers may not stage them.
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
/// Environment flag that marks a coordinator-started worker.
pub const ENV_WORKER: &str = "RECIPE_WORKER";
/// Environment variable carrying the invocation id into the worker.
pub const ENV_INVOCATION: &str = "RECIPE_INVOCATION";
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
