//! Recipe helper functions
//!
//! This module contains all the functions available to recipe scripts.
//! These are the public API that recipe authors use.
//!
//! ## Categories
//!
//! - **acquire**: download, copy, verify_sha256, verify_sha512, verify_blake3
//! - **build**: extract, cd, run
//! - **install**: install_bin, install_lib, install_man
//! - **filesystem**: mkdir, rm, mv, ln, chmod, exists, file_exists, dir_exists
//! - **io**: read_file, glob_list
//! - **env**: env, set_env
//! - **command**: run_output, run_status
//! - **http**: http_get, github_latest_release, github_latest_tag, parse_version
//! - **process**: exec, exec_output
//! - **git**: git_clone, git_clone_depth
//! - **torrent**: torrent, download_with_resume

pub mod acquire;
pub mod build;
pub mod command;
pub mod env;
pub mod filesystem;
pub mod git;
pub mod http;
pub mod install;
pub mod io;
pub mod process;
pub mod torrent;

use rhai::Engine;

/// Register all helper functions with the Rhai engine
pub fn register_all(engine: &mut Engine) {
    // Acquire phase helpers
    engine.register_fn("download", acquire::download);
    engine.register_fn("copy", acquire::copy_files);
    engine.register_fn("verify_sha256", acquire::verify_sha256);
    engine.register_fn("verify_sha512", acquire::verify_sha512);
    engine.register_fn("verify_blake3", acquire::verify_blake3);

    // Build phase helpers
    engine.register_fn("extract", build::extract);
    engine.register_fn("cd", build::change_dir);
    engine.register_fn("run", build::run_cmd);
    engine.register_fn("shell", build::run_cmd); // Alias for run, use when recipe defines own run()

    // Install phase helpers
    engine.register_fn("install_bin", install::install_bin);
    engine.register_fn("install_lib", install::install_lib);
    engine.register_fn("install_man", install::install_man);
    engine.register_fn("install_to_dir", install::install_to_dir); // 2-arg version
    engine.register_fn("install_to_dir", install::install_to_dir_i64); // 3-arg version with mode
    engine.register_fn("rpm_install", install::rpm_install);

    // Filesystem utilities
    engine.register_fn("exists", filesystem::exists);
    engine.register_fn("file_exists", filesystem::file_exists);
    engine.register_fn("dir_exists", filesystem::dir_exists);
    engine.register_fn("mkdir", filesystem::mkdir);
    engine.register_fn("rm", filesystem::rm_files);
    engine.register_fn("mv", filesystem::move_file);
    engine.register_fn("ln", filesystem::symlink);
    engine.register_fn("chmod", filesystem::chmod_file);

    // I/O utilities
    engine.register_fn("read_file", io::read_file);
    engine.register_fn("glob_list", io::glob_list);

    // Environment utilities
    engine.register_fn("env", env::get_env);
    engine.register_fn("set_env", env::set_env);

    // Command utilities
    engine.register_fn("run_output", command::run_output);
    engine.register_fn("run_status", command::run_status);

    // HTTP utilities for update checking
    engine.register_fn("http_get", http::http_get);
    engine.register_fn("github_latest_release", http::github_latest_release);
    engine.register_fn("github_latest_tag", http::github_latest_tag);
    engine.register_fn("parse_version", http::parse_version);

    // Execution utilities for run command
    engine.register_fn("exec", process::exec);
    engine.register_fn("exec_output", process::exec_output);

    // Git utilities
    engine.register_fn("git_clone", git::git_clone);
    engine.register_fn("git_clone_depth", git::git_clone_depth);

    // Torrent/download utilities
    engine.register_fn("torrent", torrent::torrent);
    engine.register_fn("download_with_resume", torrent::download_with_resume);
}
