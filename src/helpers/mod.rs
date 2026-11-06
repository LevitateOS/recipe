//! Recipe helper functions
//!
//! This module contains all the functions available to recipe scripts.
//! These are the public API that recipe authors use.
//!
//! ## Design: explicit data flow
//!
//! Helpers take their inputs as arguments and return their results; paths
//! come from `RUNTIME`, not from a current directory. Commands, package
//! managers and the environment overlay are the side effects they exist for.
//!
//! ```rhai
//! // EXPLICIT: you can see where archive comes from and where it goes
//! let archive = download(url, RUNTIME.paths.build + "/foo.tar.gz", ctx.sha256);
//! verify_sha256(archive, "abc123...");
//! extract(archive, RUNTIME.paths.build);
//! ```
//!
//! ## Module Organization (by lifecycle phase)
//!
//! - **internal**: Internal utilities (NOT exposed to Rhai scripts)
//!   - cmd, fs_utils, hash, progress
//!
//! - **acquire**: ACQUIRE phase - getting sources
//!   - download, verify_sha256/512/blake3, http_get, git_checkout
//!
//! - **build**: BUILD phase - transforming sources
//!   - extract, extract_with_format
//!
//! - **install**: INSTALL phase - placing files
//!   - exists, mkdir, rm, mv, ln, chmod, read_file, write_file, check_disk_space
//!
//! - **util**: Cross-phase utilities
//!   - join_path, basename, dirname, trim, contains, replace, split
//!   - shell, shell_in, shell_output, exec_checked, exec, exec_output
//!   - env, log, debug, warn
//!

// Internal utility modules (used by other helpers, NOT exposed to Rhai)
pub mod internal;

// Recipe-facing helper modules organized by lifecycle phase
pub mod acquire;
pub mod build;
pub mod install;
pub mod util;


use rhai::Engine;


/// Register a fallible helper under `name`; its errors name the same function.
macro_rules! helper {
    ($engine:expr, $name:literal, |$($arg:ident : $ty:ty),*| $body:expr) => {
        $engine.register_fn($name, move |$($arg: $ty),*| helper_result($name, $body))
    };
}

/// Register all helper functions with the Rhai engine
pub fn register_all(engine: &mut Engine) {
    engine.register_fn("ready", |ctx: rhai::Map| -> rhai::Map {
        [
            ("status".into(), "ready".into()),
            ("ctx".into(), ctx.into()),
        ]
        .into()
    });
    engine.register_fn("needs_work", |ctx: rhai::Map, reason: &str| -> rhai::Map {
        [
            ("status".into(), "needs_work".into()),
            ("ctx".into(), ctx.into()),
            ("reason".into(), reason.into()),
        ]
        .into()
    });
    // ========================================================================
    // Pure helpers - all functions take explicit inputs, return explicit outputs
    // ========================================================================

    // Path utilities (util/paths)
    engine.register_fn("join_path", |a: &str, b: &str| {
        util::join_path(a, b)
    });
    engine.register_fn("basename", |path: &str| {
        util::basename(path)
    });
    engine.register_fn("dirname", |path: &str| {
        util::dirname(path)
    });

    // String utilities (util/string)
    engine.register_fn("trim", |s: &str| {
        util::trim(s)
    });
    engine.register_fn("starts_with", |s: &str, prefix: &str| {
        util::starts_with(s, prefix)
    });
    engine.register_fn("ends_with", |s: &str, suffix: &str| {
        util::ends_with(s, suffix)
    });
    engine.register_fn("contains", |s: &str, pat: &str| {
        util::contains(s, pat)
    });
    engine.register_fn("replace", |s: &str, from: &str, to: &str| {
        util::replace(s, from, to)
    });
    engine.register_fn("split", |s: &str, sep: &str| {
        util::split(s, sep)
    });

    // Logging utilities (util/log)
    engine.register_fn("log", |msg: &str| {
        util::log(msg)
    });
    // Rhai owns print/debug conversion. Registering a unit-returning `debug`
    // overload breaks its built-in formatter, which expects a string.
    engine.on_print(|msg| {
        crate::diagnostics::line(msg);
    });
    engine.on_debug(|msg, source, position| {
        util::debug(&format!("{}:{position}: {msg}", source.unwrap_or("recipe")));
    });
    engine.register_fn("warn", |msg: &str| {
        util::warn(msg)
    });

    // Shell utilities (util/shell)
    helper!(engine, "shell", |cmd: &str| util::shell(cmd));
    helper!(engine, "shell_in", |dir: &str, cmd: &str| util::shell_in(dir, cmd));
    engine.register_fn("shell_status", |cmd: &str| {
        util::shell_status(cmd)
    });
    engine.register_fn("shell_status_in", |dir: &str, cmd: &str| {
        util::shell_status_in(dir, cmd)
    });
    helper!(engine, "shell_output", |cmd: &str| util::shell_output(cmd));
    helper!(engine, "shell_output_in", |dir: &str, cmd: &str| util::shell_output_in(dir, cmd));

    // I/O utilities (install/io)
    helper!(engine, "read_file", |path: &str| install::read_file(path));
    engine.register_fn("read_file_or_empty", |path: &str| {
        install::read_file_or_empty(path)
    });
    helper!(engine, "write_file", |path: &str, content: &str| install::write_file(path, content));
    helper!(engine, "append_file", |path: &str, content: &str| install::append_file(path, content));
    helper!(engine, "append_line_if_missing", |path: &str, line: &str| install::append_line_if_missing(path, line));
    helper!(engine, "glob_list", |pattern: &str| install::glob_list(pattern));
    helper!(engine, "glob_exists", |pattern: &str| install::glob_exists(pattern));
    helper!(engine, "copy_into_dir", |pattern: &str, dest_dir: &str| install::copy_into_dir(pattern, dest_dir));
    helper!(engine, "copy_file", |src: &str, dst: &str| install::copy_file(src, dst));
    helper!(engine, "copy_file_reflink", |src: &str, dst: &str| install::copy_file_reflink(src, dst));
    helper!(engine, "copy_tree_contents", |src_dir: &str, dst_dir: &str| install::copy_tree_contents(src_dir, dst_dir));
    helper!(engine, "copy_first_existing", |sources: rhai::Array, dst: &str| install::copy_first_existing(sources, dst));

    // Filesystem utilities (install/filesystem)
    engine.register_fn("exists", |path: &str| {
        install::exists(path)
    });
    engine.register_fn("is_file", |path: &str| {
        install::is_file(path)
    });
    engine.register_fn("is_dir", |path: &str| {
        install::is_dir(path)
    });
    helper!(engine, "mkdir", |path: &str| install::mkdir(path));
    helper!(engine, "rm", |pattern: &str| install::rm_files(pattern));
    helper!(engine, "mv", |src: &str, dst: &str| install::move_file(src, dst));
    helper!(engine, "ln", |target: &str, link: &str| install::symlink(target, link));
    helper!(engine, "ln_force", |target: &str, link: &str| install::symlink_force(target, link));
    helper!(engine, "chmod", |path: &str, mode: i64| install::chmod_file(path, mode));
    helper!(engine, "replace_in_file", |path: &str, from: &str, to: &str| install::replace_in_file(path, from, to));

    // Acquire helpers (acquire/download, acquire/verify)
    // download(url, dest, sha256[, resume]) -> path string
    // verify_sha256(path, expected) -> () (throws on mismatch)
    helper!(engine, "download", |url: &str, dest: &str, sha256: &str| acquire::download(url, dest, sha256, false));
    helper!(engine, "download", |url: &str, dest: &str, sha256: &str, resume: bool| acquire::download(url, dest, sha256, resume));
    helper!(engine, "verify_sha256", |path: &str, expected: &str| acquire::verify_sha256(path, expected));
    helper!(engine, "verify_sha512", |path: &str, expected: &str| acquire::verify_sha512(path, expected));
    helper!(engine, "verify_blake3", |path: &str, expected: &str| acquire::verify_blake3(path, expected));
    helper!(engine, "fetch_sha256", |url: &str, filename: &str| acquire::fetch_sha256(url, filename));

    // Build helpers (build/extract)
    // extract(archive, dest) -> ()
    // extract_with_format(archive, dest, format) -> ()
    helper!(engine, "extract", |archive: &str, dest: &str| build::extract(archive, dest));
    helper!(engine, "extract_with_format", |archive: &str, dest: &str, format: &str| build::extract_with_format(archive, dest, format));

    // Environment utilities (util/env)
    engine.register_fn("env", |name: &str| {
        util::get_env(name)
    });
    helper!(engine, "http_get", |url: &str| acquire::http_get(url));
    helper!(engine, "github_latest_release", |repo: &str| acquire::github_latest_release(repo));
    helper!(engine, "github_latest_tag", |repo: &str| acquire::github_latest_tag(repo));
    engine.register_fn("parse_version", |s: &str| {
        acquire::parse_version(s)
    });
    helper!(engine, "github_download_release", |repo: &str, pattern: &str, dest: &str, sha256: &str| acquire::github_download_release(repo, pattern, dest, sha256));
    engine.register_fn(
        "check_disk_space",
        |path: &str, required: i64| -> Result<(), Box<rhai::EvalAltResult>> {
            install::disk::check_disk_space(std::path::Path::new(path), required as u64)
                .map_err(|e| helper_error("check_disk_space", e.to_string().into()))
        },
    );

    // Process execution utilities (util/process)
    helper!(engine, "exec", |cmd: &str, args: rhai::Array| util::exec(cmd, args));
    helper!(engine, "exec", |cmd: &str, args: rhai::Array, options: rhai::Map| util::exec_with_options(cmd, args, options));
    helper!(engine, "exec_checked", |cmd: &str, args: rhai::Array| util::exec_checked(cmd, args));
    helper!(engine, "exec_checked", |cmd: &str, args: rhai::Array, options: rhai::Map| util::exec_checked_with_options(cmd, args, options));
    helper!(engine, "exec_output", |cmd: &str, args: rhai::Array| util::exec_output(cmd, args));
    helper!(engine, "exec_output", |cmd: &str, args: rhai::Array, options: rhai::Map| util::exec_output_with_options(cmd, args, options));
    engine.register_fn("command_exists", |name: &str| {
        util::command_exists(name)
    });
    helper!(engine, "apk_installed", |name: &str| util::apk_installed(name));
    helper!(engine, "apk_version", |name: &str| util::apk_version(name));
    helper!(engine, "apk_package_available", |name: &str| util::apk_package_available(name));
    helper!(engine, "apk_install", |packages: rhai::Array| util::apk_install(packages));
    helper!(engine, "apk_add_repo", |url: &str| util::apk_add_repo(url));
    helper!(engine, "apk_download", |packages: rhai::Array, dest_dir: &str| util::apk_download(packages, dest_dir));
    helper!(engine, "rpm_installed", |name: &str| util::rpm_installed(name));
    helper!(engine, "rpm_version", |name: &str| util::rpm_version(name));
    helper!(engine, "dnf_package_available", |name: &str| util::dnf_package_available(name));
    helper!(engine, "dnf_install", |packages: rhai::Array| util::dnf_install(packages));
    helper!(engine, "dnf_install_allow_erasing", |packages: rhai::Array| util::dnf_install_allow_erasing(packages));
    helper!(engine, "dnf_add_repo", |url: &str| util::dnf_add_repo(url));
    helper!(engine, "dnf_download", |packages: rhai::Array, dest_dir: &str, arches: rhai::Array| util::dnf_download(packages, dest_dir, arches));
    helper!(engine, "dnf_download", |packages: rhai::Array, dest_dir: &str, arches: rhai::Array, resolve: bool| util::dnf_download_with_resolve(packages, dest_dir, arches, resolve));

    // Git utilities (acquire/git)
    helper!(engine, "git_checkout", |url: &str, dest: &str, commit: &str| acquire::git_checkout(url, dest, commit));
}

// Keep native failures catchable in Rhai, but distinguish them from a recipe's
// explicit needs_work result from a readiness check.
fn helper_result<T>(
    name: &str,
    result: Result<T, Box<rhai::EvalAltResult>>,
) -> Result<T, Box<rhai::EvalAltResult>> {
    result.map_err(|error| helper_error(name, error))
}

fn helper_error(name: &str, error: Box<rhai::EvalAltResult>) -> Box<rhai::EvalAltResult> {
    rhai::EvalAltResult::ErrorInFunctionCall(
        name.into(),
        "<recipe-helper>".into(),
        error,
        rhai::Position::NONE,
    )
    .into()
}
