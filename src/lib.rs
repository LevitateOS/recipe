//! Rhai-based package recipe executor for LevitateOS
//!
//! Recipes are Rhai scripts that define how to acquire, build, and install packages.
//! The engine provides helper functions and executes the `acquire()`, `build()`, and
//! `install()` functions defined in each recipe.
//!
//! # Example Recipe
//!
//! ```rhai
//! let name = "bash";
//! let version = "5.2.26";
//! let deps = ["readline", "ncurses"];  // Optional dependencies
//!
//! fn acquire() {
//!     download("https://ftp.gnu.org/gnu/bash/bash-5.2.26.tar.gz");
//!     verify_sha256("abc123...");
//! }
//!
//! fn build() {
//!     extract("tar.gz");
//!     cd("bash-5.2.26");
//!     run(`./configure --prefix=${PREFIX}`);
//!     run(`make -j${NPROC}`);
//! }
//!
//! fn install() {
//!     run("make install");
//! }
//! ```
//!
//! # Dependencies
//!
//! Recipes can declare dependencies using `let deps = ["pkg1", "pkg2"]`.
//! Use `recipe install --deps <package>` to install dependencies automatically.
//! The `recipe deps <package>` command shows dependency information.
//!
//! # Engine-Provided Functions
//!
//! ## Acquire Phase
//! - `download(url)` - Download file from URL
//! - `copy(pattern)` - Copy files matching glob pattern
//! - `verify_sha256(hash)` - Verify last downloaded/copied file
//!
//! ## Build Phase
//! - `extract(format)` - Extract archive (tar.gz, tar.xz, tar.bz2, zip)
//! - `cd(dir)` - Change working directory
//! - `run(cmd)` - Execute shell command
//!
//! ## Install Phase
//! - `install_bin(pattern)` - Install to PREFIX/bin
//! - `install_lib(pattern)` - Install to PREFIX/lib
//! - `install_man(pattern)` - Install to PREFIX/share/man/man{N}
//! - `rpm_install()` - Extract RPM contents to PREFIX
//!
//! # Variables Available in Scripts
//!
//! - `PREFIX` - Installation prefix
//! - `BUILD_DIR` - Temporary build directory
//! - `ARCH` - Target architecture (x86_64, aarch64)
//! - `NPROC` - Number of CPUs
//! - `RPM_PATH` - Path to RPM repository (from environment)

mod engine;

pub use engine::deps;
pub use engine::recipe_state;
pub use engine::util;
pub use engine::RecipeEngine;
