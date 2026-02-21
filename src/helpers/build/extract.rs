#[path = "extract_api.rs"]
mod extract_api;

#[cfg(test)]
#[path = "extract_tests.rs"]
mod extract_tests;

pub use extract_api::{detect_format, extract, extract_with_format};
