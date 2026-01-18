// Autotools helper module
// Import with: import "lib/autotools" as autotools;

/// Standard configure && make && make install workflow
fn configure_make_install() {
    configure_make_install_opts("");
}

/// Configure with custom options, then make && make install
fn configure_make_install_opts(configure_opts) {
    run(`./configure --prefix=${PREFIX} ${configure_opts}`);
    run(`make -j${NPROC}`);
    run("make install");
}

/// Just configure with custom options
fn configure(opts) {
    run(`./configure --prefix=${PREFIX} ${opts}`);
}

/// Just make with optional targets
fn make(targets) {
    if targets == "" {
        run(`make -j${NPROC}`);
    } else {
        run(`make -j${NPROC} ${targets}`);
    }
}
