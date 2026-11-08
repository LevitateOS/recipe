# Linux bootstrap and operator setup

The verified host baseline is Rocky Linux 10.2, x86-64, kernel 6.12, glibc 2.39.
The native Ginux coordinator currently needs root privileges. Image production,
source compilation and package generation run as the ordinary operator.

**Runtime delivery remains unavailable.** The public workspace Ginux pin and
published Recipe source do not supply the required configured combination.
The local preparation below has been exercised with unpublished candidate
source; it is not a released-input obtainability claim. Obtain the approved
maintained Ginux/Recipe source revisions before treating this as release setup.
Do not substitute a migration executable, private image cache or test CA.

## Host and source tools

On the verified Rocky host, install normal signed repository tools:

```sh
sudo dnf install gcc gcc-c++ make git curl jq rpm-build rpmdevtools \
  python3 tar xz bzip2 gzip pkgconf-pkg-config file binutils zstd
command -v rpm2archive rpm2cpio
```

Rocky supplies `rpm2cpio` as a command, not a separate package of that name.
Install a supported Rust toolchain (MSRV 1.93); the pinned official Rust 1.99.0
GNU archive in the [bootstrap guide](../bootstrap/README.md) was used on the
fresh host. Verify its digest before running its installer. A clean host needs
normal crates.io access for compilation; `--offline` assumes inputs already
acquired. Do not borrow a developer Cargo target directory or alter TLS trust.

Recipe source retains its standalone Cargo.lock. From an approved source checkout
or extracted source crate:

```sh
cargo build --locked --release --bins
cargo install --locked --path . --bins --root /absolute/path/to/recipe-tools
```

This installs `recipe` (coordinator and worker) and `recipe-image` (bootstrap
producer). The RPM supplies both binaries, the converter dependency, bootstrap
manifests/guides, examples and manuals/licenses. It retains automatic shared-
library requirements. The binary release archive supplies the coordinator/worker;
use the matching RPM or source crate for the image producer.
A locally generated unsigned candidate is not an authenticated release RPM.
Do not disable dependency, metadata, signature or TLS checking to make a package
install. Verify artifact digests and use ordinary publisher trust for dependencies.

## Reproduce images

Use a new input directory. Acquire and verify every declared OCI, Rust and RPM
input using the URLs/pins in the bootstrap guide and manifests. Build the public
upstream OCI assembler with `RUSTFLAGS=''`, use its `GINUX_VFS_INDEX` override,
and run its fetch/prepare producers. This assembler is independently obtainable;
it is separate from the missing configured runtime delivery.

Run the actual Recipe producer for each image into a new output directory:

```sh
recipe-image --manifest bootstrap/ginux-build-image.json \
  --input-root /absolute/path/to/inputs --output /absolute/path/to/compiler-image
recipe-image --manifest bootstrap/ginux-acquisition-image.json \
  --input-root /absolute/path/to/inputs --output /absolute/path/to/acquisition-image
```

The corrected manifests use the public assembler's derived `b93bfd0f…` base.
The historical `1026f8d3…` base pin is not its output. The acquisition manifest
contains twelve RPMs, including ISO tools. Current recorded reproduction hashes:

| Image | SHA-256 |
| --- | --- |
| Compiler | `a8edd8faecd8125a19a70356e8969209f9f78efcf1333c250de6c6afc1807776` |
| Acquisition | `ffe04d8671e45036ac77acc958fb6cfd3c24a939075dc7e6c5437b69d7f05c15` |

Both were reproduced byte-for-byte on the development host and the clean Rocky
host with Rust 1.99.1 and 1.99.0 respectively (both use RPM 4.19.1.1).
Verify `rootfs.tar` against its
`provenance.json`, including every selected input digest. The old eight-RPM
`c089…` receipt does not identify either current manifest's output.

Existing output directories are rejected. Wrong input hashes and missing
conversion tools leave no fresh publication or private temporary directories
and preserve existing valid outputs. Interruptions can leave private temporary
directories; inspect/remove only that failed run's owned temporary data. Treat
any non-success as failure. A late error after atomic publication can leave a
complete fresh directory; never consume it based only on the path's existence.

## Configure the complete coordinator

Select the exact compatible launcher, image and matching worker explicitly:

```sh
export RECIPE_BIN=/absolute/path/to/recipe-tools/bin/recipe
export GINUX_BIN=/absolute/path/to/maintained-ginux
export GINUX_ROOTFS=/absolute/path/to/acquisition-image/rootfs.tar
export GINUX_ROOT="oci:recipe-compiler@sha256:$(jq -er .sha256 /absolute/path/to/acquisition-image/provenance.json)"
export RECIPE_WORKER_BIN="$RECIPE_BIN"
```

These source and artifact selections must be obtainable and verified; the old
workspace pin cannot satisfy them. With `GINUX_ROOT` omitted, the default is the
corrected bare Rocky base, which has no compiler. Explicit prepared images need
their own provenance digest; a mismatched root identity is rejected.

Run the whole coordinator with bootstrap privileges; wrapping only the launcher
in sudo breaks its inherited control channels. Use explicit work/state paths:

```sh
export RECIPE_WORK=/absolute/path/to/new-example-work
mkdir -p "$RECIPE_WORK"
sudo env GINUX_BIN="$GINUX_BIN" GINUX_ROOTFS="$GINUX_ROOTFS" \
  GINUX_ROOT="$GINUX_ROOT" RECIPE_WORKER_BIN="$RECIPE_WORKER_BIN" \
  "$RECIPE_BIN" install examples/hello.rhai --build-dir "$RECIPE_WORK/build" \
  --state-dir "$RECIPE_WORK/state" --json-output "$RECIPE_WORK/hello.json"
```

Run `isinstalled` and another `install` with the same selections, recipe and
work/state paths to verify readiness and a fresh coordinator's state reuse.
[threaded-hello.rhai](../examples/threaded-hello.rhai) builds an actual offline
Cargo program inside Ginux, executes six joined threads, installs the program
and checks that a completed build/install is not repeated. Use distinct work/
state paths for that recipe. Keep Cargo locking and normal linker threading.

## Ownership and exported products

The unprivileged image producer creates operator-owned image outputs. The
privileged coordinator creates root-owned private state/logs/publication
generations. Inspect those with the same privileged operator; ordinary users
are not promised access through private generation directories. For a regular
program, export explicitly into an ordinary-user directory:

```sh
program=$(jq -er .outputs.program "$RECIPE_WORK/hello.json")
sudo "$program"
mkdir -p "$RECIPE_WORK/export"
sudo install -m 0755 -o "$(id -u)" -g "$(id -g)" \
  "$program" "$RECIPE_WORK/export/hello"
"$RECIPE_WORK/export/hello"
```

This is an administrator-controlled copy of a regular executable, not a change
to private state ownership or a target-rootfs deployment operation. Target
metadata/deployment keeps its separate contract. Public-library callers also
run their complete coordinator with the required privileges and select the
matching `RECIPE_WORKER_BIN`; they do not run directly on the host merely because
worker-like environment variables are present.

Preparation additionally verified actual pinned public Rocky HTTPS and GitHub
Git acquisition on the unmodified publisher trust store, plus fresh denied-
network acquisition. Coordinator hostname resolution becomes bounded immutable
IP mappings; Ginux enforces numeric endpoints, not a DNS/HTTP identity policy.
These checks do not close final maintained-source, hosted Linux or security
acceptance. See the superproject PRD14 record for exact source/binary receipts
and the outstanding source-delivery requirement.
