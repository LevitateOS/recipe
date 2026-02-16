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
//! let archive = download(url, BUILD_DIR + "/foo.tar.gz");
//! verify_sha256(archive, "abc123...");
//! extract(archive, BUILD_DIR);
//! ```
//!
//! ## Module Organization (by lifecycle phase)
//!
//! - **internal**: Internal utilities (NOT exposed to Rhai scripts)
//!   - cmd, fs_utils, hash, progress, url_utils
//!
//! - **acquire**: ACQUIRE phase - getting sources
//!   - download, verify_sha256/512/blake3, http_get, git_clone, torrent
//!
//! - **build**: BUILD phase - transforming sources
//!   - extract, extract_with_format
//!
//! - **install**: INSTALL phase - placing files
//!   - exists, mkdir, rm, mv, ln, chmod, read_file, write_file, check_disk_space
//!
//! - **util**: Cross-phase utilities
//!   - join_path, basename, dirname, trim, contains, replace, split
//!   - shell, shell_in, shell_output, exec, exec_output
//!   - env, set_env, log, debug, warn
//!
//! - **llm**: AI/LLM helpers (standalone)

// Internal utility modules (used by other helpers, NOT exposed to Rhai)
pub mod internal;

// Recipe-facing helper modules organized by lifecycle phase
pub mod acquire;
pub mod build;
pub mod install;
pub mod util;

// Standalone modules
pub mod llm;

use rhai::Engine;
use std::sync::OnceLock;

fn trace_helpers_enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var("RECIPE_TRACE_HELPERS").ok().is_some_and(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
    })
}

fn trace_helper(name: &str) {
    if trace_helpers_enabled() {
        crate::core::output::detail(&format!("[helper] {name}"));
    }
}

/// Register all helper functions with the Rhai engine
pub fn register_all(engine: &mut Engine) {
    // ========================================================================
    // Pure helpers - all functions take explicit inputs, return explicit outputs
    // ========================================================================

    // Path utilities (util/paths)
    engine.register_fn("join_path", |a: &str, b: &str| {
        trace_helper("join_path");
        util::join_path(a, b)
    });
    engine.register_fn("basename", |path: &str| {
        trace_helper("basename");
        util::basename(path)
    });
    engine.register_fn("dirname", |path: &str| {
        trace_helper("dirname");
        util::dirname(path)
    });

    // String utilities (util/string)
    engine.register_fn("trim", |s: &str| {
        trace_helper("trim");
        util::trim(s)
    });
    engine.register_fn("starts_with", |s: &str, prefix: &str| {
        trace_helper("starts_with");
        util::starts_with(s, prefix)
    });
    engine.register_fn("ends_with", |s: &str, suffix: &str| {
        trace_helper("ends_with");
        util::ends_with(s, suffix)
    });
    engine.register_fn("contains", |s: &str, pat: &str| {
        trace_helper("contains");
        util::contains(s, pat)
    });
    engine.register_fn("replace", |s: &str, from: &str, to: &str| {
        trace_helper("replace");
        util::replace(s, from, to)
    });
    engine.register_fn("split", |s: &str, sep: &str| {
        trace_helper("split");
        util::split(s, sep)
    });

    // Logging utilities (util/log)
    engine.register_fn("log", |msg: &str| {
        trace_helper("log");
        util::log(msg)
    });
    engine.register_fn("debug", |msg: &str| {
        trace_helper("debug");
        util::debug(msg)
    });
    engine.register_fn("warn", |msg: &str| {
        trace_helper("warn");
        util::warn(msg)
    });

    // Shell utilities (util/shell)
    engine.register_fn("shell", |cmd: &str| {
        trace_helper("shell");
        util::shell(cmd)
    });
    engine.register_fn("shell_in", |dir: &str, cmd: &str| {
        trace_helper("shell_in");
        util::shell_in(dir, cmd)
    });
    engine.register_fn("shell_status", |cmd: &str| {
        trace_helper("shell_status");
        util::shell_status(cmd)
    });
    engine.register_fn("shell_status_in", |dir: &str, cmd: &str| {
        trace_helper("shell_status_in");
        util::shell_status_in(dir, cmd)
    });
    engine.register_fn("shell_output", |cmd: &str| {
        trace_helper("shell_output");
        util::shell_output(cmd)
    });
    engine.register_fn("shell_output_in", |dir: &str, cmd: &str| {
        trace_helper("shell_output_in");
        util::shell_output_in(dir, cmd)
    });

    // I/O utilities (install/io)
    engine.register_fn("read_file", |path: &str| {
        trace_helper("read_file");
        install::read_file(path)
    });
    engine.register_fn("read_file_or_empty", |path: &str| {
        trace_helper("read_file_or_empty");
        install::read_file_or_empty(path)
    });
    engine.register_fn("write_file", |path: &str, content: &str| {
        trace_helper("write_file");
        install::write_file(path, content)
    });
    engine.register_fn("append_file", |path: &str, content: &str| {
        trace_helper("append_file");
        install::append_file(path, content)
    });
    engine.register_fn("glob_list", |pattern: &str| {
        trace_helper("glob_list");
        install::glob_list(pattern)
    });

    // Filesystem utilities (install/filesystem)
    engine.register_fn("exists", |path: &str| {
        trace_helper("exists");
        install::exists(path)
    });
    engine.register_fn("file_exists", |path: &str| {
        trace_helper("file_exists");
        install::file_exists(path)
    });
    engine.register_fn("is_file", |path: &str| {
        trace_helper("is_file");
        install::is_file(path)
    });
    engine.register_fn("dir_exists", |path: &str| {
        trace_helper("dir_exists");
        install::dir_exists(path)
    });
    engine.register_fn("is_dir", |path: &str| {
        trace_helper("is_dir");
        install::is_dir(path)
    });
    engine.register_fn("mkdir", |path: &str| {
        trace_helper("mkdir");
        install::mkdir(path)
    });
    engine.register_fn("rm", |pattern: &str| {
        trace_helper("rm");
        install::rm_files(pattern)
    });
    engine.register_fn("mv", |src: &str, dst: &str| {
        trace_helper("mv");
        install::move_file(src, dst)
    });
    engine.register_fn("ln", |target: &str, link: &str| {
        trace_helper("ln");
        install::symlink(target, link)
    });
    engine.register_fn("chmod", |path: &str, mode: i64| {
        trace_helper("chmod");
        install::chmod_file(path, mode)
    });

    // Acquire helpers (acquire/download, acquire/verify)
    // download(url, dest) -> path string
    // verify_sha256(path, expected) -> () (throws on mismatch)
    engine.register_fn("download", |url: &str, dest: &str| {
        trace_helper("download");
        acquire::download(url, dest)
    });
    engine.register_fn("verify_sha256", |path: &str, expected: &str| {
        trace_helper("verify_sha256");
        acquire::verify_sha256(path, expected)
    });
    engine.register_fn("verify_sha512", |path: &str, expected: &str| {
        trace_helper("verify_sha512");
        acquire::verify_sha512(path, expected)
    });
    engine.register_fn("verify_blake3", |path: &str, expected: &str| {
        trace_helper("verify_blake3");
        acquire::verify_blake3(path, expected)
    });
    engine.register_fn("fetch_sha256", |url: &str, filename: &str| {
        trace_helper("fetch_sha256");
        acquire::fetch_sha256(url, filename)
    });

    // Build helpers (build/extract)
    // extract(archive, dest) -> ()
    // extract_with_format(archive, dest, format) -> ()
    engine.register_fn("extract", |archive: &str, dest: &str| {
        trace_helper("extract");
        build::extract(archive, dest)
    });
    engine.register_fn(
        "extract_with_format",
        |archive: &str, dest: &str, format: &str| {
            trace_helper("extract_with_format");
            build::extract_with_format(archive, dest, format)
        },
    );

    // Environment utilities (util/env)
    engine.register_fn("env", |name: &str| {
        trace_helper("env");
        util::get_env(name)
    });
    engine.register_fn("set_env", |name: &str, value: &str| {
        trace_helper("set_env");
        util::set_env(name, value)
    });

    // HTTP utilities for update checking (acquire/http)
    engine.register_fn("http_get", |url: &str| {
        trace_helper("http_get");
        acquire::http_get(url)
    });
    engine.register_fn("github_latest_release", |repo: &str| {
        trace_helper("github_latest_release");
        acquire::github_latest_release(repo)
    });
    engine.register_fn("github_latest_tag", |repo: &str| {
        trace_helper("github_latest_tag");
        acquire::github_latest_tag(repo)
    });
    engine.register_fn("parse_version", |s: &str| {
        trace_helper("parse_version");
        acquire::parse_version(s)
    });
    engine.register_fn(
        "github_download_release",
        |repo: &str, pattern: &str, dest_dir: &str| {
            trace_helper("github_download_release");
            acquire::github_download_release(repo, pattern, dest_dir)
        },
    );
    engine.register_fn(
        "extract_from_tarball",
        |url: &str, pattern: &str, dest: &str| {
            trace_helper("extract_from_tarball");
            acquire::extract_from_tarball(url, pattern, dest)
        },
    );

    // Disk space utilities (install/disk)
    engine.register_fn(
        "check_disk_space",
        |path: &str, required: i64| -> Result<(), Box<rhai::EvalAltResult>> {
            trace_helper("check_disk_space");
            install::disk::check_disk_space(std::path::Path::new(path), required as u64)
                .map_err(|e| e.to_string().into())
        },
    );

    // Process execution utilities (util/process)
    engine.register_fn("exec", |cmd: &str, args: rhai::Array| {
        trace_helper("exec");
        util::exec(cmd, args)
    });
    engine.register_fn("exec_output", |cmd: &str, args: rhai::Array| {
        trace_helper("exec_output");
        util::exec_output(cmd, args)
    });

    // Git utilities (acquire/git)
    // git_clone(url, dest_dir) -> path string
    // git_clone_depth(url, dest_dir, depth) -> path string
    engine.register_fn("git_clone", |url: &str, dest_dir: &str| {
        trace_helper("git_clone");
        acquire::git_clone(url, dest_dir)
    });
    engine.register_fn(
        "git_clone_depth",
        |url: &str, dest_dir: &str, depth: i64| {
            trace_helper("git_clone_depth");
            acquire::git_clone_depth(url, dest_dir, depth)
        },
    );

    // Torrent/download utilities (acquire/torrent)
    // torrent(url, dest_dir) -> path string
    // download_with_resume(url, dest_path) -> path string
    engine.register_fn("torrent", |url: &str, dest_dir: &str| {
        trace_helper("torrent");
        acquire::torrent(url, dest_dir)
    });
    engine.register_fn("download_with_resume", |url: &str, dest: &str| {
        trace_helper("download_with_resume");
        acquire::download_with_resume(url, dest)
    });

    // LLM utilities (for complex version/URL extraction)
    engine.register_fn("llm_extract", |content: &str, prompt: &str| {
        trace_helper("llm_extract");
        llm::llm_extract(content, prompt)
    });
    engine.register_fn("llm_find_latest_version", |url: &str, project: &str| {
        trace_helper("llm_find_latest_version");
        llm::llm_find_latest_version(url, project)
    });
    engine.register_fn("llm_find_download_url", |content: &str, criteria: &str| {
        trace_helper("llm_find_download_url");
        llm::llm_find_download_url(content, criteria)
    });
}
