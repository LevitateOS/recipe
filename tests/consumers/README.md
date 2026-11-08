# Actual managed consumers

`run.py` drives the real Recipe CLI, matching worker and configured Ginux
launcher. It does not compile or execute the workload on the host. Select all
three executables and the digest-verified compiler/acquisition image explicitly:

```sh
export RECIPE_TEST_BIN=/absolute/path/to/recipe
export RECIPE_WORKER_BIN="$RECIPE_TEST_BIN"
export GINUX_BIN=/absolute/path/to/ginux
export GINUX_ROOTFS=/absolute/path/to/rootfs.tar
export GINUX_ROOT=oci:recipe-compiler@sha256:IMAGE_DIGEST

# Run the whole coordinator with the launcher's documented bootstrap privileges.
sudo -n --preserve-env=RECIPE_TEST_BIN,RECIPE_WORKER_BIN,GINUX_BIN,GINUX_ROOTFS,GINUX_ROOT \
  python3 tools/recipe/tests/consumers/run.py sqlite --output .artifacts/consumers/sqlite
```

Run from the LevitateOS root (or adjust the script path). Each output directory
must be new; failed evidence is never overwritten. `receipt.json` records exact
commands, binary/image/fixture hashes, results and product validation. Runtime
diagnostics remain in that directory. Required cases:

| Case | Behavior |
| --- | --- |
| `sqlite` | Compile the C client inside Ginux, demonstrate SQLITE_BUSY between two processes, commit both transactions, check sum/count/integrity and publish database/executable with mode 0755. The host only reads the published database to independently check it. |
| `cargo` | Four simultaneous GNU/musl Cargo clients share CARGO_HOME and a target directory, with real package-cache/build-lock waiting and exactly one compilation per target. Both built programs start/join a thread inside Ginux. GNU uses its normal compiler driver; musl uses the image's rust-lld, with ordinary linker threading. The musl ELF must be static. |
| `git-http` | Host Git creates a tiny bare source fixture; a loopback HTTP server serves its actual objects. Guest Git clones/reuses the exact commit and publishes its checked payload. |
| `git-https` | Guest Git clones/reuses public `LevitateOS/recipe` commit `43d9dd4e1296799467f42b2c026aa107c3022d8d`, checks the README digest and publishes it. Uses normal image trust/TLS and declared github.com:443. No fixture CA, proxy or TLS bypass. |
| `rpm --rpm /absolute/path/to/zlib-ng-compat-2.3.3-6.fc45.x86_64.rpm` | Verify the pinned RPM, execute image rpm2cpio→cpio with pipefail, then check all library bytes, size, mode, root UID/GID, SONAME and build-ID links after publication. |
| `apk --apk-lock distro/builder/recipes/sources/acorn-apk-lock.rhai` | Use the producer's current publisher-verified package facts/key. Download pinned apk-tools-static/musl/BusyBox, verify signatures and install with virtual UID/GID 1000, execute installed BusyBox, capture target metadata, and validate published packages and installed bytes against the pinned APK payload. No package scripts or network are allowed during installation. |

The image must provide GNU/musl Rust libraries, GCC/glibc development files,
Git/libcurl/normal CA trust, cpio/rpm2cpio and SQLite. External HTTPS availability
is required for the public Git/APK cases. Host Python 3, Git and readelf prepare
or inspect fixtures/products; they never substitute workload results. The APK
source lock is an explicit input to the runner, not fetched or updated by it.

These receipts bind to the selected artifacts. A local candidate pass does not
establish source availability, final integration, security acceptance or delivery
of a different combination. Rerun affected consumers after corrections and all
required consumers on the actual selected release source/image.
