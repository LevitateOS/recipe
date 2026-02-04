//! ACQUIRE phase helpers - getting sources
//!
//! Pure functions for downloading, verifying, and fetching source files.
//! These are the first step in the recipe lifecycle: acquire -> build -> install.
//!
//! ## Functions
//!
//! - **download**: Download files from HTTP(S) URLs
//! - **verify_sha256/sha512/blake3**: Verify file integrity with cryptographic hashes
//! - **http_get**: Fetch content from URLs
//! - **github_latest_release/tag**: Query GitHub for latest versions
//! - **git_clone**: Clone git repositories
//! - **torrent**: Download via BitTorrent

pub mod download;
pub mod git;
pub mod http;
pub mod torrent;
pub mod verify;

// Re-export commonly used items
pub use download::download;
pub use git::{git_clone, git_clone_depth};
pub use http::{
    extract_from_tarball, github_download_release, github_latest_release, github_latest_tag,
    http_get, parse_version,
};
pub use torrent::{download_with_resume, torrent};
pub use verify::{FileHashes, compute_hashes, verify_blake3, verify_sha256, verify_sha512};
