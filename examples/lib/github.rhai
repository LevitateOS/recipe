// GitHub helper module
// Import with: import "lib/github" as github;

/// Download a release asset from GitHub
fn github_release(repo, version, asset) {
    let url = `https://github.com/${repo}/releases/download/${version}/${asset}`;
    download(url);
}

/// Download a source tarball from GitHub
fn github_tarball(repo, version) {
    let url = `https://github.com/${repo}/archive/refs/tags/${version}.tar.gz`;
    download(url);
}
