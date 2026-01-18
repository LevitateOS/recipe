// ripgrep - Fast line-oriented search tool
// Pre-built binary from GitHub releases

let name = "ripgrep";
let version = "14.1.1";
let description = "Fast line-oriented search tool that recursively searches directories";

fn acquire() {
    if ARCH == "x86_64" {
        download(`https://github.com/BurntSushi/ripgrep/releases/download/${version}/ripgrep-${version}-x86_64-unknown-linux-musl.tar.gz`);
    } else if ARCH == "aarch64" {
        download(`https://github.com/BurntSushi/ripgrep/releases/download/${version}/ripgrep-${version}-aarch64-unknown-linux-gnu.tar.gz`);
    } else {
        throw `Unsupported architecture: ${ARCH}`;
    }
}

fn build() {
    extract("tar.gz");

    // Find the extracted directory
    if ARCH == "x86_64" {
        cd(`ripgrep-${version}-x86_64-unknown-linux-musl`);
    } else {
        cd(`ripgrep-${version}-aarch64-unknown-linux-gnu`);
    }
}

fn install() {
    install_bin("rg");
    install_man("doc/rg.1");
}
