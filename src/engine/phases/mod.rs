//! Phase modules - first-class concepts in the recipe lifecycle

pub mod acquire;
pub mod build;
pub mod install;

pub use acquire::{copy_files, download, verify_sha256};
pub use build::{change_dir, extract, run_cmd};
pub use install::{install_bin, install_lib, install_man, rpm_install};
