//! Cross-phase utility helpers
//!
//! Pure functions usable across all recipe phases.
//!
//! ## Functions
//!
//! - **paths**: join_path, basename, dirname
//! - **string**: trim, contains, replace, split
//! - **shell**: shell, shell_in, shell_output
//! - **process**: exec, exec_output
//! - **env**: env, set_env
//! - **log**: log, debug, warn

pub mod env;
pub mod log;
pub mod paths;
pub mod process;
pub mod shell;
pub mod string;

// Re-export commonly used items from paths
pub use paths::{basename, dirname, join_path};

// Re-export commonly used items from string
pub use string::{contains, ends_with, replace, split, starts_with, trim};

// Re-export commonly used items from shell
pub use shell::{shell, shell_in, shell_output, shell_output_in, shell_status, shell_status_in};

// Re-export commonly used items from process
pub use process::{exec, exec_output};

// Re-export commonly used items from env
pub use env::{get_env, set_env};

// Re-export commonly used items from log
pub use log::{debug, log, warn};
