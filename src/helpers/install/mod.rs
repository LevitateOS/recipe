//! INSTALL phase helpers - placing files
//!
//! Pure functions for installing files to the system.
//! These are the third step in the recipe lifecycle: acquire -> build -> install.
//!
//! ## Functions
//!
//! - **filesystem**: exists, mkdir, rm, mv, ln, chmod, copy helpers
//! - **io**: read_file, write_file, append_file, glob_list, text replacement
//! - **disk**: check_disk_space

pub mod disk;
pub mod filesystem;
pub mod io;

// Re-export commonly used items from filesystem
pub use filesystem::{
    chmod_file, copy_file, copy_file_reflink, copy_first_existing, copy_into_dir,
    copy_tree_contents, dir_exists, exists, file_exists, glob_exists, is_dir, is_file, mkdir,
    move_file, rm_files, symlink, symlink_force,
};

// Re-export commonly used items from io
pub use io::{
    append_file, append_line_if_missing, glob_list, read_file, read_file_or_empty, replace_in_file,
    write_file,
};

// Re-export disk utilities
pub use disk::check_disk_space;
