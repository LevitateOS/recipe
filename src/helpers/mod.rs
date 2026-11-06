//! Recipe helper functions
//!
//! This module contains all the functions available to recipe scripts.
//! These are the public API that recipe authors use.
//!
//! ## Design: All Pure Functions
//!
//! All helper functions are PURE - they take explicit inputs and return
//! explicit outputs, with no hidden state. This makes data flow visible:
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
//!   - cmd, fs_utils, hash, progress, url_utils
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
    engine.register_fn("shell", |cmd: &str| {
        helper_result("shell", util::shell(cmd))
    });
    engine.register_fn("shell_in", |dir: &str, cmd: &str| {
        helper_result("shell_in", util::shell_in(dir, cmd))
    });
    engine.register_fn("shell_status", |cmd: &str| {
        util::shell_status(cmd)
    });
    engine.register_fn("shell_status_in", |dir: &str, cmd: &str| {
        util::shell_status_in(dir, cmd)
    });
    engine.register_fn("shell_output", |cmd: &str| {
        helper_result("shell_output", util::shell_output(cmd))
    });
    engine.register_fn("shell_output_in", |dir: &str, cmd: &str| {
        helper_result("shell_output_in", util::shell_output_in(dir, cmd))
    });

    // I/O utilities (install/io)
    engine.register_fn("read_file", |path: &str| {
        helper_result("read_file", install::read_file(path))
    });
    engine.register_fn("read_file_or_empty", |path: &str| {
        install::read_file_or_empty(path)
    });
    engine.register_fn("write_file", |path: &str, content: &str| {
        helper_result("write_file", install::write_file(path, content))
    });
    engine.register_fn("append_file", |path: &str, content: &str| {
        helper_result("append_file", install::append_file(path, content))
    });
    engine.register_fn("append_line_if_missing", |path: &str, line: &str| {
        helper_result(
            "append_line_if_missing",
            install::append_line_if_missing(path, line),
        )
    });
    engine.register_fn("glob_list", |pattern: &str| {
        install::glob_list(pattern)
    });
    engine.register_fn("glob_exists", |pattern: &str| {
        helper_result("glob_exists", install::glob_exists(pattern))
    });
    engine.register_fn("copy_into_dir", |pattern: &str, dest_dir: &str| {
        helper_result("copy_into_dir", install::copy_into_dir(pattern, dest_dir))
    });
    engine.register_fn("copy_file", |src: &str, dst: &str| {
        helper_result("copy_file", install::copy_file(src, dst))
    });
    engine.register_fn("copy_file_reflink", |src: &str, dst: &str| {
        helper_result("copy_file_reflink", install::copy_file_reflink(src, dst))
    });
    engine.register_fn("copy_tree_contents", |src_dir: &str, dst_dir: &str| {
        helper_result(
            "copy_tree_contents",
            install::copy_tree_contents(src_dir, dst_dir),
        )
    });
    engine.register_fn("copy_first_existing", |sources: rhai::Array, dst: &str| {
        helper_result(
            "copy_first_existing",
            install::copy_first_existing(sources, dst),
        )
    });

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
    engine.register_fn("mkdir", |path: &str| {
        helper_result("mkdir", install::mkdir(path))
    });
    engine.register_fn("rm", |pattern: &str| {
        helper_result("rm_files", install::rm_files(pattern))
    });
    engine.register_fn("mv", |src: &str, dst: &str| {
        helper_result("move_file", install::move_file(src, dst))
    });
    engine.register_fn("ln", |target: &str, link: &str| {
        helper_result("symlink", install::symlink(target, link))
    });
    engine.register_fn("ln_force", |target: &str, link: &str| {
        helper_result("symlink_force", install::symlink_force(target, link))
    });
    engine.register_fn("chmod", |path: &str, mode: i64| {
        helper_result("chmod_file", install::chmod_file(path, mode))
    });
    engine.register_fn("replace_in_file", |path: &str, from: &str, to: &str| {
        helper_result("replace_in_file", install::replace_in_file(path, from, to))
    });

    // Acquire helpers (acquire/download, acquire/verify)
    // download(url, dest, sha256[, resume]) -> path string
    // verify_sha256(path, expected) -> () (throws on mismatch)
    engine.register_fn("download", |url: &str, dest: &str, sha256: &str| {
        helper_result("download", acquire::download(url, dest, sha256, false))
    });
    engine.register_fn(
        "download",
        |url: &str, dest: &str, sha256: &str, resume: bool| {
            helper_result("download", acquire::download(url, dest, sha256, resume))
        },
    );
    engine.register_fn("verify_sha256", |path: &str, expected: &str| {
        helper_result("verify_sha256", acquire::verify_sha256(path, expected))
    });
    engine.register_fn("verify_sha512", |path: &str, expected: &str| {
        helper_result("verify_sha512", acquire::verify_sha512(path, expected))
    });
    engine.register_fn("verify_blake3", |path: &str, expected: &str| {
        helper_result("verify_blake3", acquire::verify_blake3(path, expected))
    });
    engine.register_fn("fetch_sha256", |url: &str, filename: &str| {
        helper_result("fetch_sha256", acquire::fetch_sha256(url, filename))
    });

    // Build helpers (build/extract)
    // extract(archive, dest) -> ()
    // extract_with_format(archive, dest, format) -> ()
    engine.register_fn("extract", |archive: &str, dest: &str| {
        helper_result("extract", build::extract(archive, dest))
    });
    engine.register_fn(
        "extract_with_format",
        |archive: &str, dest: &str, format: &str| {
            helper_result(
                "extract_with_format",
                build::extract_with_format(archive, dest, format),
            )
        },
    );

    // Environment utilities (util/env)
    engine.register_fn("env", |name: &str| {
        util::get_env(name)
    });
    engine.register_fn("http_get", |url: &str| {
        helper_result("http_get", acquire::http_get(url))
    });
    engine.register_fn("github_latest_release", |repo: &str| {
        helper_result(
            "github_latest_release",
            acquire::github_latest_release(repo),
        )
    });
    engine.register_fn("github_latest_tag", |repo: &str| {
        helper_result("github_latest_tag", acquire::github_latest_tag(repo))
    });
    engine.register_fn("parse_version", |s: &str| {
        acquire::parse_version(s)
    });
    engine.register_fn(
        "github_download_release",
        |repo: &str, pattern: &str, dest: &str, sha256: &str| {
            helper_result(
                "github_download_release",
                acquire::github_download_release(repo, pattern, dest, sha256),
            )
        },
    );
    engine.register_fn(
        "check_disk_space",
        |path: &str, required: i64| -> Result<(), Box<rhai::EvalAltResult>> {
            install::disk::check_disk_space(std::path::Path::new(path), required as u64)
                .map_err(|e| helper_error("check_disk_space", e.to_string().into()))
        },
    );

    // Process execution utilities (util/process)
    engine.register_fn("exec", |cmd: &str, args: rhai::Array| {
        helper_result("exec", util::exec(cmd, args))
    });
    engine.register_fn(
        "exec",
        |cmd: &str, args: rhai::Array, options: rhai::Map| {
            helper_result(
                "exec_with_options",
                util::exec_with_options(cmd, args, options),
            )
        },
    );
    engine.register_fn("exec_checked", |cmd: &str, args: rhai::Array| {
        helper_result("exec_checked", util::exec_checked(cmd, args))
    });
    engine.register_fn(
        "exec_checked",
        |cmd: &str, args: rhai::Array, options: rhai::Map| {
            helper_result(
                "exec_checked_with_options",
                util::exec_checked_with_options(cmd, args, options),
            )
        },
    );
    engine.register_fn("exec_output", |cmd: &str, args: rhai::Array| {
        helper_result("exec_output", util::exec_output(cmd, args))
    });
    engine.register_fn(
        "exec_output",
        |cmd: &str, args: rhai::Array, options: rhai::Map| {
            helper_result(
                "exec_output_with_options",
                util::exec_output_with_options(cmd, args, options),
            )
        },
    );
    engine.register_fn("command_exists", |name: &str| {
        util::command_exists(name)
    });
    engine.register_fn("apk_installed", |name: &str| {
        helper_result("apk_installed", util::apk_installed(name))
    });
    engine.register_fn("apk_version", |name: &str| {
        helper_result("apk_version", util::apk_version(name))
    });
    engine.register_fn("apk_package_available", |name: &str| {
        helper_result("apk_package_available", util::apk_package_available(name))
    });
    engine.register_fn("apk_install", |packages: rhai::Array| {
        helper_result("apk_install", util::apk_install(packages))
    });
    engine.register_fn("apk_add_repo", |url: &str| {
        helper_result("apk_add_repo", util::apk_add_repo(url))
    });
    engine.register_fn("apk_download", |packages: rhai::Array, dest_dir: &str| {
        helper_result("apk_download", util::apk_download(packages, dest_dir))
    });
    engine.register_fn("rpm_installed", |name: &str| {
        helper_result("rpm_installed", util::rpm_installed(name))
    });
    engine.register_fn("rpm_version", |name: &str| {
        helper_result("rpm_version", util::rpm_version(name))
    });
    engine.register_fn("dnf_package_available", |name: &str| {
        helper_result("dnf_package_available", util::dnf_package_available(name))
    });
    engine.register_fn("dnf_install", |packages: rhai::Array| {
        helper_result("dnf_install", util::dnf_install(packages))
    });
    engine.register_fn("dnf_install_allow_erasing", |packages: rhai::Array| {
        helper_result(
            "dnf_install_allow_erasing",
            util::dnf_install_allow_erasing(packages),
        )
    });
    engine.register_fn("dnf_add_repo", |url: &str| {
        helper_result("dnf_add_repo", util::dnf_add_repo(url))
    });
    engine.register_fn(
        "dnf_download",
        |packages: rhai::Array, dest_dir: &str, arches: rhai::Array| {
            helper_result(
                "dnf_download",
                util::dnf_download(packages, dest_dir, arches),
            )
        },
    );
    engine.register_fn(
        "dnf_download",
        |packages: rhai::Array, dest_dir: &str, arches: rhai::Array, resolve: bool| {
            helper_result(
                "dnf_download_with_resolve",
                util::dnf_download_with_resolve(packages, dest_dir, arches, resolve),
            )
        },
    );

    // Git utilities (acquire/git)
    engine.register_fn("git_checkout", |url: &str, dest: &str, commit: &str| {
        helper_result("git_checkout", acquire::git_checkout(url, dest, commit))
    });
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
