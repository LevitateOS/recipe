//! INSTALL phase helpers - placing files
//!
//! Pure functions for installing files to the system.
//! These are the third step in the recipe lifecycle: acquire -> build -> install.
//!
//! ## Functions
//!
//! - **filesystem**: exists, mkdir, rm, mv, ln, chmod
//! - **io**: read_file, write_file, append_file, glob_list
//! - **disk**: check_disk_space

pub mod disk;
pub mod filesystem;
pub mod io;

// Re-export commonly used items from filesystem
pub use filesystem::{
    chmod_file, dir_exists, exists, file_exists, is_dir, is_file, mkdir, move_file, rm_files,
    symlink,
};

// Re-export commonly used items from io
pub use io::{append_file, glob_list, read_file, read_file_or_empty, write_file};

// Re-export disk utilities
pub use disk::check_disk_space;
