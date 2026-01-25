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
//! ## Categories
//!
//! - **paths**: join_path, basename, dirname
//! - **string**: trim, starts_with, ends_with, contains, replace, split
//! - **log**: log, debug, warn
//! - **shell**: shell, shell_in, shell_status, shell_output
//! - **io**: read_file, write_file, append_file, glob_list
//! - **filesystem**: exists, is_file, is_dir, mkdir, rm, mv, ln, chmod
//! - **acquire**: download(url, dest), verify_sha256(path, hash)
//! - **build**: extract(archive, dest)
//! - **env**: env, set_env
//! - **http**: http_get, github_latest_release, github_latest_tag
//! - **process**: exec, exec_output
//! - **git**: git_clone(url, dest), git_clone_depth(url, dest, depth)
//! - **torrent**: torrent(url, dest), download_with_resume(url, dest)
//! - **disk**: check_disk_space
//! - **llm**: llm_extract, llm_find_latest_version, llm_find_download_url

// Internal utility modules (used by other helpers)
pub mod cmd;
pub mod fs_utils;
pub mod hash;
pub mod progress;
pub mod url_utils;

// Recipe-facing helper modules (all pure functions)
pub mod acquire;
pub mod build;
pub mod disk;
pub mod env;
pub mod filesystem;
pub mod git;
pub mod http;
pub mod io;
pub mod llm;
pub mod log;
pub mod paths;
pub mod process;
pub mod shell;
pub mod string;
pub mod torrent;

use rhai::Engine;

/// Register all helper functions with the Rhai engine
pub fn register_all(engine: &mut Engine) {
    // ========================================================================
    // Pure helpers - all functions take explicit inputs, return explicit outputs
    // ========================================================================

    // Path utilities
    engine.register_fn("join_path", paths::join_path);
    engine.register_fn("basename", paths::basename);
    engine.register_fn("dirname", paths::dirname);

    // String utilities
    engine.register_fn("trim", string::trim);
    engine.register_fn("starts_with", string::starts_with);
    engine.register_fn("ends_with", string::ends_with);
    engine.register_fn("contains", string::contains);
    engine.register_fn("replace", string::replace);
    engine.register_fn("split", string::split);

    // Logging utilities
    engine.register_fn("log", log::log);
    engine.register_fn("debug", log::debug);
    engine.register_fn("warn", log::warn);

    // Shell utilities (pure - run in current directory or explicit directory)
    engine.register_fn("shell", shell::shell);
    engine.register_fn("shell_in", shell::shell_in);
    engine.register_fn("shell_status", shell::shell_status);
    engine.register_fn("shell_status_in", shell::shell_status_in);
    engine.register_fn("shell_output", shell::shell_output);
    engine.register_fn("shell_output_in", shell::shell_output_in);

    // I/O utilities
    engine.register_fn("read_file", io::read_file);
    engine.register_fn("read_file_or_empty", io::read_file_or_empty);
    engine.register_fn("write_file", io::write_file);
    engine.register_fn("append_file", io::append_file);
    engine.register_fn("glob_list", io::glob_list);

    // Filesystem utilities
    engine.register_fn("exists", filesystem::exists);
    engine.register_fn("file_exists", filesystem::file_exists);
    engine.register_fn("is_file", filesystem::is_file);
    engine.register_fn("dir_exists", filesystem::dir_exists);
    engine.register_fn("is_dir", filesystem::is_dir);
    engine.register_fn("mkdir", filesystem::mkdir);
    engine.register_fn("rm", filesystem::rm_files);
    engine.register_fn("mv", filesystem::move_file);
    engine.register_fn("ln", filesystem::symlink);
    engine.register_fn("chmod", filesystem::chmod_file);

    // Acquire helpers - pure functions with explicit paths
    // download(url, dest) -> path string
    // verify_sha256(path, expected) -> () (throws on mismatch)
    engine.register_fn("download", acquire::download);
    engine.register_fn("verify_sha256", acquire::verify_sha256);
    engine.register_fn("verify_sha512", acquire::verify_sha512);
    engine.register_fn("verify_blake3", acquire::verify_blake3);

    // Build helpers - pure functions
    // extract(archive, dest) -> ()
    // extract_with_format(archive, dest, format) -> ()
    engine.register_fn("extract", build::extract);
    engine.register_fn("extract_with_format", build::extract_with_format);

    // Environment utilities (process-wide, acceptable for scripts)
    engine.register_fn("env", env::get_env);
    engine.register_fn("set_env", env::set_env);

    // HTTP utilities for update checking
    engine.register_fn("http_get", http::http_get);
    engine.register_fn("github_latest_release", http::github_latest_release);
    engine.register_fn("github_latest_tag", http::github_latest_tag);
    engine.register_fn("parse_version", http::parse_version);
    engine.register_fn("github_download_release", http::github_download_release);
    engine.register_fn("extract_from_tarball", http::extract_from_tarball);

    // Disk space utilities
    engine.register_fn(
        "check_disk_space",
        |path: &str, required: i64| -> Result<(), Box<rhai::EvalAltResult>> {
            disk::check_disk_space(std::path::Path::new(path), required as u64)
                .map_err(|e| e.to_string().into())
        },
    );

    // Process execution utilities
    engine.register_fn("exec", process::exec);
    engine.register_fn("exec_output", process::exec_output);

    // Git utilities - pure functions with explicit dest
    // git_clone(url, dest_dir) -> path string
    // git_clone_depth(url, dest_dir, depth) -> path string
    engine.register_fn("git_clone", git::git_clone);
    engine.register_fn("git_clone_depth", git::git_clone_depth);

    // Torrent/download utilities - pure functions with explicit dest
    // torrent(url, dest_dir) -> path string
    // download_with_resume(url, dest_path) -> path string
    engine.register_fn("torrent", torrent::torrent);
    engine.register_fn("download_with_resume", torrent::download_with_resume);

    // LLM utilities (for complex version/URL extraction)
    engine.register_fn("llm_extract", llm::llm_extract);
    engine.register_fn("llm_find_latest_version", llm::llm_find_latest_version);
    engine.register_fn("llm_find_download_url", llm::llm_find_download_url);
}
