// Test recipe: Install helpers
// Tests: install_bin, install_lib, install_man, install_to_dir, cd

let name = "test-install";
let version = "1.0.0";
let description = "Test recipe for install helper functions";
let installed = false;

fn acquire() {
    // Nothing to acquire
}

fn build() {
    // Create test files for installation
    mkdir(`${BUILD_DIR}/src`);
    mkdir(`${BUILD_DIR}/lib`);
    mkdir(`${BUILD_DIR}/doc`);
    mkdir(`${BUILD_DIR}/share`);

    // Create binaries
    run(`echo '#!/bin/sh\necho "bin1"' > ${BUILD_DIR}/src/mybin1`);
    run(`echo '#!/bin/sh\necho "bin2"' > ${BUILD_DIR}/src/mybin2`);
    chmod(`${BUILD_DIR}/src/mybin1`, 0o755);
    chmod(`${BUILD_DIR}/src/mybin2`, 0o755);

    // Create libraries
    run(`echo "library content 1" > ${BUILD_DIR}/lib/libtest1.so`);
    run(`echo "library content 2" > ${BUILD_DIR}/lib/libtest2.a`);

    // Create man pages
    run(`echo ".TH MYBIN1 1" > ${BUILD_DIR}/doc/mybin1.1`);
    run(`echo ".TH MYBIN2 1" > ${BUILD_DIR}/doc/mybin2.1`);
    run(`echo ".TH CONFIG 5" > ${BUILD_DIR}/doc/myconfig.5`);

    // Create doc files for install_to_dir
    run(`echo "README content" > ${BUILD_DIR}/share/README`);
    run(`echo "LICENSE content" > ${BUILD_DIR}/share/LICENSE`);
}

fn install() {
    // Test cd - change to source directory
    cd(`${BUILD_DIR}/src`);

    // Test install_bin - install executables
    install_bin("mybin1");
    install_bin("mybin*");  // Test glob pattern

    // Test install_lib - install libraries
    cd(`${BUILD_DIR}/lib`);
    install_lib("libtest1.so");
    install_lib("*.a");  // Test glob pattern

    // Test install_man - install man pages (auto-detects section)
    cd(`${BUILD_DIR}/doc`);
    install_man("mybin1.1");
    install_man("mybin2.1");
    install_man("myconfig.5");  // Section 5 man page

    // Test install_to_dir - install to custom directory
    cd(`${BUILD_DIR}/share`);
    install_to_dir("README", "share/doc/test-install");
    install_to_dir("LICENSE", "share/doc/test-install");
}
