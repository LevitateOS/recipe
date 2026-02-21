mod api;
#[cfg(test)]
mod tests;

pub use api::{
    extract_from_tarball, github_download_release, github_latest_release, github_latest_tag,
    github_release_assets, http_get, parse_version,
};
