# Linux bootstrap and operator setup

The verified host baseline is Rocky Linux 10.2, x86-64, kernel 6.12, glibc 2.39.

**Status (2026-11-23): the public-lifecycle cutover is written and has never
been run.** Recipe no longer uses the downstream `ginux launch`; it runs
recipes through Ginux's public lifecycle ([cutover design](ginux-cutover-design.md),
[migration plan](ginux-public-lifecycle-migration.md), section 6). Sections below
that still describe the old runtime (the historical source pair, the image
hashes of `rootfs.tar`, `GINUX_BIN`/`GINUX_ROOTFS`/`GINUX_ROOT`) are history,
kept until the verification pass replaces them with a recorded tuple. The
current setup is in "Configure the complete coordinator". The Ginux coordinator
now needs root or a sudoers rule: every privileged step is the Ginux helper run
as root behind Recipe's guard, which `sudo` runs ([README, Privileges](../README.md#privileges)); the user-namespace
path and its AppArmor profile are gone. Image production, source compilation and
package generation run as the ordinary operator.

The maintained sources are [LevitateOS/recipe](https://github.com/LevitateOS/recipe)
and [LevitateOS/ginux](https://github.com/LevitateOS/ginux). Upstream Ginux leads;
our Ginux history includes upstream main and retains the changes Recipe needs.
Reconcile new upstream behavior and replace our changes when upstream covers it.
The pinned version also includes upstream Gate 11 PR #249 at
`9759daaec8589a7e2d2cc9e1c9ec1f398b6e958f`, ahead of its merge into main.
Gate 11 remains partial against its performance budget. Recipe selects explicit
downstream catalog 1002, separate from Gate 11 upstream catalog 2 (historical:
Recipe now selects catalog 2).
This source delivery does not claim distro boot/install or complete security
acceptance. Do not substitute a migration binary, private image cache or test CA.
The reproducible baseline below is Recipe `e7bfc166` with Ginux `28f4368f`.
Later local manual-readiness and TUI compiler corrections are still pending
final delivery and product/security acceptance in the superproject's
`docs/ginux-contributions/prd15-product-contract.md`. The baseline commands do
not reproduce those later corrections or establish their release acceptance.

The 2026-11-09 upstream integration migrates configuration to `[contract.*]`.
Its Ginux source is `ef1b538f940d8ae99b5e798c19cada9976381d86`, and Recipe's
functional migration is `2a786826c73b8aa9111b1a6620a30fa1e65ef6b5`. The tracked
CI source pin selects that matching Ginux. These new commits are local pending
publication permission; the historical clone commands below deliberately retain
their original pair. Do not pair the new Recipe with the old launcher or assume
the new commits are publicly obtainable. The superproject's
`docs/ginux-contributions/upstream-integration-2026-11-09.md` owns the current
source, validation and delivery status.

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

Recipe source retains its standalone Cargo.lock. Obtain both repositories over
HTTPS and retain both exact revisions with your artifacts. Moving branch names
do not identify a reproducible pair. These commands reproduce the delivered
baseline; use the recorded final release pair when that is available:

```sh
git clone --branch master https://github.com/LevitateOS/recipe.git recipe-source
git clone --branch main https://github.com/LevitateOS/ginux.git maintained-ginux
git -C recipe-source checkout --detach e7bfc166343164989794c917e67f187d0ac49ba8
git -C maintained-ginux checkout --detach 28f4368f6fd70d0e485386892a08d6734b5988dd
git -C recipe-source rev-parse HEAD
(cd maintained-ginux && cargo build -p ginux-init --bin ginux --release --target x86_64-unknown-linux-gnu)
cd recipe-source
cargo build --locked --release --bins
cargo install --locked --path . --bins --root /absolute/path/to/recipe-tools
```

Historical build (old runtime): `GINUX_BIN` was
`maintained-ginux/target/x86_64-unknown-linux-gnu/release/ginux`. The public
lifecycle needs the CLI, helper, image tool, runtime and allocator instead:
`cargo build --release -p ginux-cli -p ginux-oci -p ginux-allocator` and
`cargo build --release -p ginux-init --bin ginux --target x86_64-unknown-linux-gnu`
in the maintained checkout, with the helper script
`scripts/ginux-host-instance-helper.sh`; the four files are installed
root-owned (`recipe runtime stage` does it into a root tree, with the
sudoers rule). The worker is the same `recipe` executable as the coordinator. An extracted source crate also supports
the two Cargo commands above; it includes the release lock and image producer.

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
input using the URLs/pins in the bootstrap guide and manifests. Run the maintained Ginux fetch/prepare producers from the pinned checkout.
Its OCI assembler uses ordinary host linker flags. The bootstrap guide also
records an equivalent public upstream assembler and explicit override.

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
Verify the layout against its
`provenance.json`, including every selected input digest (the tar digests of
this table belong to the old `rootfs.tar` form; the identity is now the manifest
digest and the hashes will be re-recorded in verification). The old eight-RPM
`c089…` receipt does not identify either current manifest's output.

Existing output directories are rejected. Wrong input hashes and missing
conversion tools leave no fresh publication or private temporary directories
and preserve existing valid outputs. Interruptions can leave private temporary
directories; inspect/remove only that failed run's owned temporary data. Treat
any non-success as failure. A late error after atomic publication can leave a
complete fresh directory; never consume it based only on the path's existence.

## Configure the complete coordinator

Select the CLI, the image (an OCI layout and the reference of its manifest
digest) and the matching worker explicitly. `recipe-image` writes the layout and
`provenance.json` (`manifest_digest`, `layer_sha256`):

```sh
export RECIPE_BIN=/absolute/path/to/recipe-tools/bin/recipe
export RECIPE_GINUX=/absolute/path/to/ginux
export RECIPE_IMAGE_LAYOUT=/absolute/path/to/acquisition-image/image.oci.tar
export RECIPE_IMAGE="local/recipe-compiler@sha256:$(jq -er .manifest_digest /absolute/path/to/acquisition-image/provenance.json)"
export RECIPE_WORKER_BIN="$RECIPE_BIN"
```

Recipe's guard (`recipe-ginux-guard`, `/usr/libexec/recipe/`), the Ginux helper,
image tool, runtime and allocator must be installed root-owned at
`/usr/libexec/recipe/`, `/usr/libexec/ginux/` and `/usr/lib64/ginux/` (the helper
may be reached through a root-owned wrapper named by `GINUX_INSTANCE_HELPER`),
and the host needs `sudo`.
`GINUX_BIN`, `GINUX_ROOT`, `GINUX_ROOTFS`, `GINUX_MINIMAL_ROOT` and
`GINUX_MINIMAL_ROOTFS` are refused by Recipe: a stale shell must not select a
runtime.

If `RECIPE_IMAGE_LAYOUT` is not set, Recipe looks for the layout its install ships,
then in `$XDG_CACHE_HOME/recipe/images/<manifest digest>.oci.tar`, and downloads
it there when it is absent (`recipe runtime fetch`; `RECIPE_RUNTIME_URL` names
another address). Ginux imports it once per host, as root: the guard allows no
`import-image`, so run `sudo recipe runtime fetch` (the installer does) before
anyone but root runs a recipe in Ginux. It also lists the image in the guard's
root-owned allowlist `/usr/libexec/recipe/runtime-images`, without which the
guard refuses the image for every caller except root.

A recipe with `NEEDS.runtime: "minimal"` (a root's effects recipe) runs in the minimal
runtime image (`bootstrap/README.md`, "Minimal runtime image"). Select it the same way,
or leave it out and such a recipe runs in the image above:

```sh
export RECIPE_MINIMAL_IMAGE_LAYOUT=/absolute/path/to/prepared-image-minimal/image.oci.tar
export RECIPE_MINIMAL_IMAGE="local/recipe-minimal@sha256:$(jq -er .manifest_digest /absolute/path/to/prepared-image-minimal/provenance.json)"
```

Every run is an instance of Ginux with a private root under the state root
(`/var/lib/recipe/ginux`, root-owned, created by the helper). The first import
copies the image there (about 1.3 GB); each run then needs a private root
(copy-on-write clones on XFS or Btrfs, a full expansion elsewhere, about 1.3 GB
per run, about 8 MB in the minimal image) and is deleted when the run ends.
`RECIPE_GINUX_STATE_ROOT` and `RECIPE_GINUX_RUN_ROOT` select other roots (every
level root-owned). The coordinator's own scratch pool (`~/.cache/recipe/scratch`;
`RECIPE_SCRATCH_DIR`, or `TMPDIR`) holds the run directory (the instance
configuration and the tars that go in and out). Published outputs go to
`<state-dir>/exports`, not into the current directory.

Verify these source and artifact selections against the recorded release tuple
(none is recorded yet for the public lifecycle). Without `RECIPE_IMAGE` and an
installed `root-reference` there is no default image.

Run the whole coordinator as root, or as an administrator after
`recipe runtime stage --sudoers GROUP` installed the rule (the rule is a
passwordless sandbox for Recipe-shaped instances from trusted images, not an
unconditional guarantee: `runtime-limits.md`, "What a shared host still
exposes") and `sudo recipe runtime fetch` imported the image. Use explicit
work/state paths:

```sh
export RECIPE_WORK=/absolute/path/to/new-example-work
mkdir -p "$RECIPE_WORK"
sudo env RECIPE_GINUX="$RECIPE_GINUX" RECIPE_IMAGE="$RECIPE_IMAGE" \
  RECIPE_IMAGE_LAYOUT="$RECIPE_IMAGE_LAYOUT" RECIPE_WORKER_BIN="$RECIPE_WORKER_BIN" \
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
privileged coordinator creates root-owned private state/logs/exports/publication
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

The [runtime limits](runtime-limits.md) also apply to privileged operators.
Running as root does not add undeclared inputs/network or lift configured
limits. Building product tools and checking their versions does not establish
their guest installer interaction or boot/install/A/B behavior. Those require
the separate included-product checks in the superproject contribution queue.

Preparation additionally verified actual pinned public Rocky HTTPS and GitHub
Git acquisition on the unmodified publisher trust store, plus fresh denied-
network acquisition. Coordinator hostname resolution becomes bounded immutable
IP mappings; Ginux enforces numeric endpoints, not a DNS/HTTP identity policy.
These checks do not close final maintained-source, hosted Linux or security
acceptance. See the superproject PRD14 record for exact source/binary receipts
and the outstanding source-delivery requirement.
