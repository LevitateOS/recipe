// fd - Simple, fast alternative to find
// Pre-built binary from GitHub releases

let name = "fd";
let version = "10.2.0";
let description = "Simple, fast and user-friendly alternative to find";
let installed = false;

fn acquire() {
    if ARCH == "x86_64" {
        download(`https://github.com/sharkdp/fd/releases/download/v${version}/fd-v${version}-x86_64-unknown-linux-musl.tar.gz`);
    } else if ARCH == "aarch64" {
        download(`https://github.com/sharkdp/fd/releases/download/v${version}/fd-v${version}-aarch64-unknown-linux-gnu.tar.gz`);
    } else {
        throw `Unsupported architecture: ${ARCH}`;
    }
}

fn build() {
    extract("tar.gz");

    if ARCH == "x86_64" {
        cd(`fd-v${version}-x86_64-unknown-linux-musl`);
    } else {
        cd(`fd-v${version}-aarch64-unknown-linux-gnu`);
    }
}

fn install() {
    install_bin("fd");
    install_man("fd.1");
}
