# Prepared compiler image

`recipe-image` is an explicit bootstrap producer for the environment consumed by
Recipe's Ginux launcher. It composes already acquired, pinned local artifacts;
it does not download packages, interpret recipes, or run package installation
scripts. Ordinary recipe execution consumes the resulting image.

The manifest records the base image, compiler RPMs, Rust distribution and selected
Rust components. Input paths are relative to `--input-root`. All inputs are copied
and verified against SHA-256 before use. RPM payloads are converted with the host's
`rpm2archive`; their paths are never extracted onto the host. The producer merges
`usr/` additions into the base archive, validates paths/links, normalizes timestamps
and numeric owners, and publishes a fresh directory containing `rootfs.tar` and
`provenance.json`. Existing outputs are never overwritten. The initial bootstrap
uses native Rust tooling and already prepared artifacts so it does not depend on
running Rhai outside Ginux.

From the workspace root, with the manifest's artifacts present under
`.artifacts/ginux-env`:

```sh
cargo build --offline -p levitate-recipe --bin recipe-image
target/debug/recipe-image \
  --manifest tools/recipe/bootstrap/ginux-build-image.json \
  --input-root .artifacts/ginux-env \
  --output .artifacts/ginux-env/prepared-image
```

Select `prepared-image/rootfs.tar` with `GINUX_ROOTFS`. Set `GINUX_ROOT` to
`oci:compiler@sha256:<sha256 from provenance.json>`. The worker binary is staged
separately by Recipe, so updating Recipe does not rebuild the compiler image.
Build and run the Ginux launcher explicitly before executing recipes.

The image includes the Rust compiler, Cargo, standard library, GCC and Make.
The current local Ginux integration admits 32 tasks. Verified offline GNU and
musl checks use two concurrent Cargo clients with one job each, share the locked
target directory, and execute the compiled threaded program. The checks retain
default linker threading. Musl selects the image's Rust linker as shown below.
Other package and distro scenarios need their own verification.

The producer bounds each input/member at 16 GiB, aggregate input/expanded data
at 32 GiB, and archive entries at 500,000. Links cannot traverse above the image
root; addition hardlinks must name a selected regular file in the same archive.
Temporary payloads use private files without keeping one descriptor per entry.
Failures leave no published output directory. Interrupted processes can leave
private temporary directories beside the selected output.


## Acquisition and musl compiler image

[`ginux-acquisition-image.json`](ginux-acquisition-image.json) is the complete
manifest used for managed HTTP/Git, GNU/musl Rust builds and RPM extraction. It
adds Git's HTTPS executable, GNU cpio and the matching Rust musl standard library
to the compiler inputs above. `rust_additions` consumes separately pinned Rust
component archives using the same component selection and path validation; a
component cannot occur in two inputs. The musl component includes static libc,
libunwind and startup objects. The existing compiler supplies `rust-lld`.

Prepare the pinned Rocky base with Ginux's explicit `fetch-rocky-oci.sh` and
`prepare-rocky-oci.sh` producers, using `GINUX_OCI_CACHE_DIR_OVERRIDE` to select
`<input-root>/oci-cache` (run those scripts from the Ginux repository). They
require their documented host tools, including `jq` and `shasum`. The base is
`docker.io/rockylinux/rockylinux:10.2.20260525.0`, linux/amd64, OCI manifest
`sha256:e372170ca8630f0f03e9b70fdd0bf4a3ce3426b0de7cdba615f06337389de176`.
The manifest's base SHA256 identifies Ginux's assembled rootfs tar, rather than
claiming a publisher-supplied rootfs tar digest.

Acquire the exact files below under the manifest's relative `toolchain/` and
`packages/` paths; each expected SHA256 is in the manifest. Rust pins come from
the [official 1.99.0 distribution manifest](https://static.rust-lang.org/dist/channel-rust-1.99.0.toml),
dated 2026-10-01:

- [Rust GNU distribution](https://static.rust-lang.org/dist/2026-10-01/rust-1.99.0-x86_64-unknown-linux-gnu.tar.gz)
- [Matching musl standard library](https://static.rust-lang.org/dist/2026-10-01/rust-std-1.99.0-x86_64-unknown-linux-musl.tar.gz)

All eight RPM pins were checked against the publisher's SHA256 fields in
[Rocky BaseOS primary metadata](https://download.rockylinux.org/pub/rocky/10/BaseOS/x86_64/os/repodata/76f4ee35ec89a1cfdbced3af0f0b24291d362b374e381e802107923509b34d51-primary.xml.gz)
and [Rocky AppStream primary metadata](https://download.rockylinux.org/pub/rocky/10/AppStream/x86_64/os/repodata/56c5886e36a7d3a5536d1b5cea97be8740edb537bd15ff7bd7db2e46aac7e582-primary.xml.gz)
on 2026-11-06. Each metadata filename is its compressed SHA256, verified against
`repomd.xml`; this is repository-metadata verification, not an RPM-signature claim.
Repository updates can remove these versioned URLs; retain the pinned inputs for
reproduction rather than silently substituting a new version.

| Input | Source |
| --- | --- |
| `cpio-2.15-3.el10.x86_64.rpm` | [official RPM](https://download.rockylinux.org/pub/rocky/10/BaseOS/x86_64/os/Packages/c/cpio-2.15-3.el10.x86_64.rpm) |
| `cpp-14.3.1-4.4.el10.x86_64.rpm` | [official RPM](https://download.rockylinux.org/pub/rocky/10/AppStream/x86_64/os/Packages/c/cpp-14.3.1-4.4.el10.x86_64.rpm) |
| `gcc-14.3.1-4.4.el10.x86_64.rpm` | [official RPM](https://download.rockylinux.org/pub/rocky/10/AppStream/x86_64/os/Packages/g/gcc-14.3.1-4.4.el10.x86_64.rpm) |
| `git-core-2.52.0-1.el10.x86_64.rpm` | [official RPM](https://download.rockylinux.org/pub/rocky/10/AppStream/x86_64/os/Packages/g/git-core-2.52.0-1.el10.x86_64.rpm) |
| `glibc-devel-2.39-145.el10_2.x86_64.rpm` | [official RPM](https://download.rockylinux.org/pub/rocky/10/AppStream/x86_64/os/Packages/g/glibc-devel-2.39-145.el10_2.x86_64.rpm) |
| `kernel-headers-6.12.0-211.73.1.el10_2.x86_64.rpm` | [official RPM](https://download.rockylinux.org/pub/rocky/10/AppStream/x86_64/os/Packages/k/kernel-headers-6.12.0-211.73.1.el10_2.x86_64.rpm) |
| `libmpc-1.3.1-7.el10.x86_64.rpm` | [official RPM](https://download.rockylinux.org/pub/rocky/10/AppStream/x86_64/os/Packages/l/libmpc-1.3.1-7.el10.x86_64.rpm) |
| `make-4.4.1-9.el10.x86_64.rpm` | [official RPM](https://download.rockylinux.org/pub/rocky/10/BaseOS/x86_64/os/Packages/m/make-4.4.1-9.el10.x86_64.rpm) |

With the exact inputs present, run from the workspace root:

```sh
cargo build --offline -p levitate-recipe --bin recipe-image
target/debug/recipe-image \
  --manifest tools/recipe/bootstrap/ginux-acquisition-image.json \
  --input-root .artifacts/ginux-env \
  --output .artifacts/ginux-env/prepared-image-acquisition
export GINUX_ROOTFS="$PWD/.artifacts/ginux-env/prepared-image-acquisition/rootfs.tar"
export GINUX_ROOT=oci:recipe-compiler@sha256:c089f95c462582cc0fcc2a8b2ba86c7d2d64c22c82efa499ce20e3704a7fe15e
```

The demonstrated output SHA256 is
`c089f95c462582cc0fcc2a8b2ba86c7d2d64c22c82efa499ce20e3704a7fe15e`;
check `provenance.json` after producing it. A fresh output directory is required.
For musl builds pass `--target x86_64-unknown-linux-musl` to Cargo and set
`RUSTFLAGS="-C linker=/usr/lib/rustlib/x86_64-unknown-linux-gnu/bin/rust-lld -C linker-flavor=ld.lld"`
in the command's environment options. The GNU fixture uses Cargo's default
linker configuration. Managed GNU compilation and musl static linking,
cache reuse, and threaded execution have been verified. C cross-compilation
needs its own target toolchain. Ordinary TLS
uses the image's certificate trust. Test-only CA images and keys are excluded.
