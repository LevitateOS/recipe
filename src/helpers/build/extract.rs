#[path = "extract_api.rs"]
mod extract_api;

pub use extract_api::{detect_format, extract, extract_with_format};
