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

/// Register all helper functions with the Rhai engine
pub fn register_all(engine: &mut Engine) {
    // ========================================================================
    // Pure helpers - all functions take explicit inputs, return explicit outputs
    // ========================================================================

    // Path utilities (util/paths)
    engine.register_fn("join_path", util::join_path);
    engine.register_fn("basename", util::basename);
    engine.register_fn("dirname", util::dirname);

    // String utilities (util/string)
    engine.register_fn("trim", util::trim);
    engine.register_fn("starts_with", util::starts_with);
    engine.register_fn("ends_with", util::ends_with);
    engine.register_fn("contains", util::contains);
    engine.register_fn("replace", util::replace);
    engine.register_fn("split", util::split);

    // Logging utilities (util/log)
    engine.register_fn("log", util::log);
    engine.register_fn("debug", util::debug);
    engine.register_fn("warn", util::warn);

    // Shell utilities (util/shell)
    engine.register_fn("shell", util::shell);
    engine.register_fn("shell_in", util::shell_in);
    engine.register_fn("shell_status", util::shell_status);
    engine.register_fn("shell_status_in", util::shell_status_in);
    engine.register_fn("shell_output", util::shell_output);
    engine.register_fn("shell_output_in", util::shell_output_in);

    // I/O utilities (install/io)
    engine.register_fn("read_file", install::read_file);
    engine.register_fn("read_file_or_empty", install::read_file_or_empty);
    engine.register_fn("write_file", install::write_file);
    engine.register_fn("append_file", install::append_file);
    engine.register_fn("glob_list", install::glob_list);

    // Filesystem utilities (install/filesystem)
    engine.register_fn("exists", install::exists);
    engine.register_fn("file_exists", install::file_exists);
    engine.register_fn("is_file", install::is_file);
    engine.register_fn("dir_exists", install::dir_exists);
    engine.register_fn("is_dir", install::is_dir);
    engine.register_fn("mkdir", install::mkdir);
    engine.register_fn("rm", install::rm_files);
    engine.register_fn("mv", install::move_file);
    engine.register_fn("ln", install::symlink);
    engine.register_fn("chmod", install::chmod_file);

    // Acquire helpers (acquire/download, acquire/verify)
    // download(url, dest) -> path string
    // verify_sha256(path, expected) -> () (throws on mismatch)
    engine.register_fn("download", acquire::download);
    engine.register_fn("verify_sha256", acquire::verify_sha256);
    engine.register_fn("verify_sha512", acquire::verify_sha512);
    engine.register_fn("verify_blake3", acquire::verify_blake3);

    // Build helpers (build/extract)
    // extract(archive, dest) -> ()
    // extract_with_format(archive, dest, format) -> ()
    engine.register_fn("extract", build::extract);
    engine.register_fn("extract_with_format", build::extract_with_format);

    // Environment utilities (util/env)
    engine.register_fn("env", util::get_env);
    engine.register_fn("set_env", util::set_env);

    // HTTP utilities for update checking (acquire/http)
    engine.register_fn("http_get", acquire::http_get);
    engine.register_fn("github_latest_release", acquire::github_latest_release);
    engine.register_fn("github_latest_tag", acquire::github_latest_tag);
    engine.register_fn("parse_version", acquire::parse_version);
    engine.register_fn("github_download_release", acquire::github_download_release);
    engine.register_fn("extract_from_tarball", acquire::extract_from_tarball);

    // Disk space utilities (install/disk)
    engine.register_fn(
        "check_disk_space",
        |path: &str, required: i64| -> Result<(), Box<rhai::EvalAltResult>> {
            install::disk::check_disk_space(std::path::Path::new(path), required as u64)
                .map_err(|e| e.to_string().into())
        },
    );

    // Process execution utilities (util/process)
    engine.register_fn("exec", util::exec);
    engine.register_fn("exec_output", util::exec_output);

    // Git utilities (acquire/git)
    // git_clone(url, dest_dir) -> path string
    // git_clone_depth(url, dest_dir, depth) -> path string
    engine.register_fn("git_clone", acquire::git_clone);
    engine.register_fn("git_clone_depth", acquire::git_clone_depth);

    // Torrent/download utilities (acquire/torrent)
    // torrent(url, dest_dir) -> path string
    // download_with_resume(url, dest_path) -> path string
    engine.register_fn("torrent", acquire::torrent);
    engine.register_fn("download_with_resume", acquire::download_with_resume);

    // LLM utilities (for complex version/URL extraction)
    engine.register_fn("llm_extract", llm::llm_extract);
    engine.register_fn("llm_find_latest_version", llm::llm_find_latest_version);
    engine.register_fn("llm_find_download_url", llm::llm_find_download_url);
}
