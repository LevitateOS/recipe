//! End-to-end tests for the recipe CLI
//!
//! These tests run the actual CLI binary and verify behavior.
//!
//! # Preconditions (Ginux suites)
//!
//! Every case that goes through `recipe_command` runs a recipe in a real
//! Ginux instance on the public lifecycle (`create`, `inject`, `start --wait`,
//! `status`, `extract`, `delete`). Missing preconditions are failures, never
//! skipped tests. The runner provides:
//!
//! - privileges: run as root, or as a member of a group the sudoers rule
//!   written by `recipe runtime stage --sudoers GROUP` names (the rule names
//!   Recipe's guard `/usr/libexec/recipe/recipe-ginux-guard`, installed
//!   root-owned, and pins `/var/lib/recipe/ginux` and `/run/recipe/ginux`;
//!   with it, export `RECIPE_GINUX_STATE_ROOT` and `RECIPE_GINUX_RUN_ROOT` to
//!   those folders). `guard.rs` and `runtime_stage.rs` need neither;
//! - the Ginux files installed root-owned: `/usr/libexec/ginux/ginux-helper`,
//!   `ginux-image`, `ginux-runtime` and the allocator, or a root-owned wrapper
//!   named by `GINUX_INSTANCE_HELPER`;
//! - `RECIPE_GINUX`: the `ginux` CLI;
//! - `RECIPE_IMAGE` (`local/recipe-compiler@sha256:<manifest>`) and
//!   `RECIPE_IMAGE_LAYOUT` (the OCI layout tar of that image,
//!   `image.oci.tar`; Ginux refuses a directory); the minimal pair `RECIPE_MINIMAL_IMAGE[_LAYOUT]` for
//!   the cases that use it;
//! - the images imported and allowed on the host. The guard allows no
//!   `import-image` and, for a caller other than root, only images listed in
//!   its root-owned allowlist `/usr/libexec/recipe/runtime-images`, so a
//!   non-root run cannot import. PRECONDITION: when the runner is not root,
//!   it can use `sudo` (without a password prompt: `sudo -n`), and the
//!   harness runs `sudo env RECIPE_IMAGE=… RECIPE_IMAGE_LAYOUT=… recipe
//!   runtime fetch` ONCE per test process before the first Ginux case; that
//!   imports the image(s) into the shared state root and lists them in the
//!   host's allowlist (a host-wide change, as installing Recipe is). Root
//!   runs import directly with `ginux import-image`, as before;
//! - `RECIPE_WORKER_BIN`: the worker (the recipe binary under test);
//! - nothing else of the old runtime: `GINUX_BIN`, `GINUX_ROOT`,
//!   `GINUX_ROOTFS`, `GINUX_MINIMAL_ROOT` and `GINUX_MINIMAL_ROOTFS` are
//!   removed from every command the fixtures start (Recipe refuses them).
//!
//! The fixtures use ONE state root for the whole run, `/var/lib/recipe-e2e/shared`
//! (run root `/run/recipe-e2e/shared`), unless `RECIPE_GINUX_STATE_ROOT` is set.
//! A state root per test would import the 1.3 GB compiler image once per test;
//! instance names carry the uid and the recipe's pid, so parallel runs do not
//! collide, and the cases that look at instances look at their own pid. The
//! first `recipe_command` imports the image layout once (as root, or through
//! `sudo recipe runtime fetch`: see the preconditions).

use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

/// Get the path to the recipe binary
fn recipe_bin() -> std::path::PathBuf {
    std::env::var_os("RECIPE_TEST_BIN")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_recipe").into())
}

/// Select the exact producer recipe inputs for standalone retained runs.
fn production_recipes() -> std::path::PathBuf {
    std::env::var_os("RECIPE_TEST_BUILDER_RECIPES")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../distro/builder/os-recipes")
        })
}

/// Create a test environment
fn create_test_env() -> (TempDir, std::path::PathBuf) {
    let retained = std::env::var_os("RECIPE_TEST_ARTIFACTS");
    let mut builder = tempfile::Builder::new();
    builder.prefix("recipe e2e ");
    let mut dir = match &retained {
        Some(parent) => builder.tempdir_in(parent),
        None => builder.tempdir(),
    }
    .unwrap();
    dir.disable_cleanup(retained.is_some());
    let recipes = dir.path().join("recipes");
    std::fs::create_dir_all(&recipes).unwrap();
    (dir, recipes)
}

/// Write a recipe file
fn write_recipe(recipes_dir: &Path, name: &str, content: &str) {
    let path = recipes_dir.join(format!("{}.rhai", name));
    std::fs::write(&path, content).unwrap();
}

/// Run recipe CLI with arguments
fn run_recipe(args: &[&str], recipes: &Path) -> std::process::Output {
    recipe_command(recipes)
        .args(args)
        .arg("--recipes-path")
        .arg(recipes)
        .arg("--build-dir")
        .arg(recipes.parent().unwrap().join("build"))
        .output()
        .expect("Failed to execute recipe command")
}

/// The CLI in Ginux mode: these suites exercise the Ginux domain (their
/// recipes spell domain paths such as `/out` literally). Host runs are
/// covered by [`host_command`].
fn recipe_command(recipes: &Path) -> Command {
    let mut command = host_command(recipes);
    ginux_env(&mut command);
    command.args(["--execution", "ginux"]);
    command
}

/// The variables of the runtime that was replaced by the public lifecycle.
/// Recipe stops when one is set (it names the variable to use instead), so a
/// stale shell must never reach a fixture command.
const GONE_VARIABLES: [&str; 5] = [
    "GINUX_BIN",
    "GINUX_ROOT",
    "GINUX_ROOTFS",
    "GINUX_MINIMAL_ROOT",
    "GINUX_MINIMAL_ROOTFS",
];

/// The state root the fixtures give to Ginux: the runner's, or the one shared
/// root of this suite.
fn ginux_state_root() -> std::path::PathBuf {
    std::env::var_os("RECIPE_GINUX_STATE_ROOT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| "/var/lib/recipe-e2e/shared".into())
}

fn ginux_run_root() -> std::path::PathBuf {
    std::env::var_os("RECIPE_GINUX_RUN_ROOT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| "/run/recipe-e2e/shared".into())
}

/// The real `ginux` CLI, or a failure that names every precondition.
fn require_ginux() -> std::ffi::OsString {
    std::env::var_os("RECIPE_GINUX").expect(
        "Ginux e2e cases need a real Ginux: RECIPE_GINUX (the ginux CLI), the Ginux files \
         root-owned at /usr/libexec/ginux (or GINUX_INSTANCE_HELPER), RECIPE_IMAGE and \
         RECIPE_IMAGE_LAYOUT, RECIPE_WORKER_BIN, and root or the sudoers rule of \
         `recipe runtime stage --sudoers GROUP` and, when not root, `sudo -n` for the one \
         `sudo recipe runtime fetch` that imports the image (see the header of tests/e2e.rs)",
    )
}

/// Variables `sudo` resets that `recipe runtime fetch` needs to find the
/// same Ginux, state root and image layouts as the fixture commands.
const FETCH_VARIABLES: [&str; 8] = [
    "RECIPE_GINUX",
    "GINUX_INSTANCE_HELPER",
    "RECIPE_IMAGE",
    "RECIPE_IMAGE_LAYOUT",
    "RECIPE_MINIMAL_IMAGE",
    "RECIPE_MINIMAL_IMAGE_LAYOUT",
    "RECIPE_RUNTIME_URL",
    "XDG_CACHE_HOME",
];

/// Point a command at the fixtures' Ginux state and run roots and make sure
/// the compiler image is imported there and allowed (once per test process):
/// root imports directly; any other user runs `sudo recipe runtime fetch`,
/// because the guard allows no `import-image` and lists only the images root
/// allowed (a precondition, see the header).
fn ginux_env(command: &mut Command) {
    let state = ginux_state_root();
    let run = ginux_run_root();
    command
        .env("RECIPE_GINUX_STATE_ROOT", &state)
        .env("RECIPE_GINUX_RUN_ROOT", &run);
    static IMPORTED: std::sync::Once = std::sync::Once::new();
    IMPORTED.call_once(|| {
        let Some(cli) = std::env::var_os("RECIPE_GINUX") else {
            return;
        };
        if unsafe { libc::geteuid() } != 0 {
            let mut fetch = Command::new("sudo");
            fetch.arg("-n").arg("env");
            for name in FETCH_VARIABLES {
                if let Some(value) = std::env::var_os(name) {
                    let mut pair = std::ffi::OsString::from(format!("{name}="));
                    pair.push(value);
                    fetch.arg(pair);
                }
            }
            fetch
                .arg(format!("RECIPE_GINUX_STATE_ROOT={}", state.display()))
                .arg(format!("RECIPE_GINUX_RUN_ROOT={}", run.display()))
                .arg(recipe_bin())
                .args(["runtime", "fetch"])
                .stdin(std::process::Stdio::null());
            for name in GONE_VARIABLES {
                fetch.env_remove(name);
            }
            let output = fetch
                .output()
                .unwrap_or_else(|error| panic!("e2e: cannot start sudo: {error}"));
            assert!(
                output.status.success(),
                "e2e precondition: `sudo recipe runtime fetch` failed ({}): {}{}",
                output.status,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        for variable in ["RECIPE_IMAGE_LAYOUT", "RECIPE_MINIMAL_IMAGE_LAYOUT"] {
            let Some(layout) = std::env::var_os(variable) else {
                continue;
            };
            let output = Command::new(&cli)
                .arg("--state-root")
                .arg(&state)
                .arg("--run-root")
                .arg(&run)
                .arg("import-image")
                .arg(&layout)
                .stdin(std::process::Stdio::null())
                .output();
            match output {
                Ok(output) if output.status.success() => {}
                Ok(output) => eprintln!(
                    "e2e: importing {variable} failed ({}): {}",
                    output.status,
                    String::from_utf8_lossy(&output.stderr)
                ),
                Err(error) => eprintln!("e2e: cannot start {cli:?}: {error}"),
            }
        }
    });
}

/// Names of the Ginux instances of this uid whose recipe process is gone.
/// A finished run deletes its instance on every path, so after a run this is
/// empty (the next Recipe command would sweep a leftover; a test looks first).
fn leaked_instances() -> Vec<String> {
    leaked_instances_in(&ginux_state_root())
}

/// [`leaked_instances`] for a state root other than the suite's.
fn leaked_instances_in(state: &Path) -> Vec<String> {
    let uid = unsafe { libc::geteuid() };
    let prefix = format!("recipe-{uid}-");
    std::fs::read_dir(state.join("instances"))
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| {
            name.strip_prefix(&prefix)
                .and_then(|rest| rest.split('-').next())
                .and_then(|pid| pid.parse::<i32>().ok())
                .is_some_and(|pid| {
                    let alive = unsafe { libc::kill(pid, 0) } == 0;
                    !alive && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
                })
        })
        .collect()
}

/// Names of the instances that belong to the recipe process `pid`.
fn instances_of(pid: u32) -> Vec<String> {
    let uid = unsafe { libc::geteuid() };
    let prefix = format!("recipe-{uid}-{pid}-");
    std::fs::read_dir(ginux_state_root().join("instances"))
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with(&prefix))
        .collect()
}

/// The line `recipe` prints for a clean `ginux start --wait` (the diagnostics
/// of an invocation; the wording is owned by `src/backend.rs`).
const START_EXIT_OK: &str = "ginux start exit 0; result exited";

/// Runtime text (the start summary, the native audit lines, the old
/// `ginux: audit` form) must never appear in the user's diagnostics: it is
/// kept in the retained supervisor log, not relayed.
fn leaks_runtime_text(diagnostics: &str) -> bool {
    [
        "GINUX_NATIVE_AUDIT",
        "GINUX_INSTANCE_START",
        "GINUX_AUDIT_DROPPED",
        "ginux: audit",
    ]
    .iter()
    .any(|marker| diagnostics.contains(marker))
}

/// A stand-in for the `ginux` CLI that runs the REAL CLI for every verb and
/// lets `body` (Python) change one of them. Globals in `body`: `VERB` (the
/// verb after the `--state-root`/`--run-root` options), `ARGS` (the CLI's
/// arguments), `option(name)` (the value after a `--flag`), `real()` (run the
/// real CLI and return its exit code) and `passthrough()` (exec the real CLI).
/// Use [`with_wrapper`] to install it. The real CLI is never replaced by a
/// protocol stub: the wrapper only edits the bytes at the coordinator's
/// boundary (the tar it injects, the archive it extracts) or the exit it sees.
fn ginux_wrapper(dir: &Path, name: &str, body: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    std::fs::write(
        &path,
        format!(
            r#"#!/usr/bin/python3
import os, subprocess, sys
REAL = os.environ['RECIPE_TEST_REAL_GINUX']
ARGS = sys.argv[1:]
def _verb():
    i = 0
    while i < len(ARGS) and ARGS[i].startswith('--'):
        i += 2
    return ARGS[i] if i < len(ARGS) else ''
VERB = _verb()
def option(name):
    return ARGS[ARGS.index(name) + 1]
def real():
    return subprocess.run([REAL] + ARGS).returncode
def passthrough():
    os.execv(REAL, [REAL] + ARGS)
{body}
"#
        ),
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// Make `command` use `wrapper` as its `ginux` CLI, with the real one behind it.
fn with_wrapper(command: &mut Command, wrapper: &Path) {
    command
        .env("RECIPE_TEST_REAL_GINUX", require_ginux())
        .env("RECIPE_GINUX", wrapper);
}

/// The CLI with the default `--execution auto`: a recipe that starts no
/// program runs on the host. The package root is the fixture's own, the one
/// an unprivileged run takes from `XDG_DATA_HOME`: a root run (the managed CI
/// job) would otherwise share the computer's `/var/lib/recipe` with every
/// other case.
fn host_command(recipes: &Path) -> Command {
    let root = recipes
        .parent()
        .expect("recipes directory has a fixture root");
    let mut command = Command::new(recipe_bin());
    for gone in GONE_VARIABLES {
        command.env_remove(gone);
    }
    command
        .current_dir(root)
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_STATE_HOME", root.join("state"))
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_CACHE_HOME", root.join("cache"))
        .arg("--package-root")
        .arg(root.join("data/recipe"));
    command
}

fn assert_success(output: &std::process::Output) {
    assert!(
        output.status.success(),
        "recipe failed: {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

// =============================================================================
// CLI Help and Version Tests

#[path = "e2e/archives.rs"]
mod archives;
#[path = "e2e/cli.rs"]
mod cli;
#[path = "e2e/command_options.rs"]
mod command_options;
#[path = "e2e/downloads.rs"]
mod downloads;
#[path = "e2e/failures.rs"]
mod failures;
#[path = "e2e/git_acquisition.rs"]
mod git_acquisition;
#[path = "e2e/guard.rs"]
mod guard;
#[path = "e2e/invocation.rs"]
mod invocation;
#[path = "e2e/kernel_acquisition.rs"]
mod kernel_acquisition;
#[path = "e2e/kernel_recipes.rs"]
mod kernel_recipes;
#[path = "e2e/preseed_recipes.rs"]
mod preseed_recipes;
#[path = "e2e/process.rs"]
mod process;
#[path = "e2e/query.rs"]
mod query;
#[path = "e2e/runtime_stage.rs"]
mod runtime_stage;

#[path = "e2e/cleanup.rs"]
mod cleanup;

#[path = "e2e/dependencies.rs"]
mod dependencies;

#[cfg(target_os = "linux")]
#[path = "e2e/managed_inputs.rs"]
mod managed_inputs;

#[path = "e2e/worker_entry.rs"]
mod worker_entry;

#[path = "e2e/managed_state.rs"]
mod managed_state;

#[cfg(target_os = "linux")]
#[path = "e2e/managed_target.rs"]
mod managed_target;

#[path = "e2e/managed_storage.rs"]
mod managed_storage;

#[path = "e2e/managed_cancellation.rs"]
mod managed_cancellation;

#[path = "e2e/managed_retention.rs"]
mod managed_retention;

#[path = "e2e/target_metadata.rs"]
mod target_metadata;

#[path = "e2e/needs.rs"]
mod needs;
