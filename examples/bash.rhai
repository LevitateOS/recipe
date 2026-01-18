// GNU Bash - Bourne-Again Shell
// Build from source tarball

let name = "bash";
let version = "5.2.37";
let description = "GNU Bourne-Again Shell";
let installed = false;

fn acquire() {
    download(`https://ftp.gnu.org/gnu/bash/bash-${version}.tar.gz`);
    // verify_sha256("9599b22ecd1d5787ad7d3b7bf0c59f312b3571e6a3a46239e7a3d2db84f7e179");
}

fn build() {
    extract("tar.gz");
    cd(`bash-${version}`);
    run(`./configure --prefix=${PREFIX} --without-bash-malloc`);
    run(`make -j${NPROC}`);
}

fn install() {
    run("make install");
}
