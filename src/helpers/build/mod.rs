//! Build helpers - transforming sources
//!
//! Pure functions for extracting and transforming downloaded sources, for
//! use inside `build()` and `configure()`.
//!
//! ## Functions
//!
//! - **extract**: Extract archives (formats: `FORMATS` in `extract.rs`)
//! - **extract_with_format**: Extract with explicit format specification

pub mod extract;

// Re-export commonly used items
pub use extract::{extract, extract_with_format};
