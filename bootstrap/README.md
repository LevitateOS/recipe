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
Rust commands currently need `LD_LIBRARY_PATH=/usr/lib`. The demonstrated offline
Cargo workload uses explicit vendored source inputs, one build job, and
`-C link-arg=-Wl,--threads=1` for LLD to stay within Ginux's current task bound.
These are demonstrated compiler requirements, not evidence that every package
or distro recipe works yet.

The producer bounds each input/member at 16 GiB, aggregate input/expanded data
at 32 GiB, and archive entries at 500,000. Links cannot traverse above the image
root; addition hardlinks must name a selected regular file in the same archive.
Temporary payloads use private files without keeping one descriptor per entry.
Failures leave no published output directory. Interrupted processes can leave
private temporary directories beside the selected output.
