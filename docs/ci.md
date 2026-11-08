# Managed integration CI

The package declares Rust 1.93 as its MSRV and retains a release Cargo.lock.
Shared CI builds/checks the selected dependencies on stable and MSRV and runs
Clippy/formatting. The standalone-resolution job independently exports source
without Cargo.lock and builds on both stable and 1.93.0. Workspace-lock checks
supplement these disposable unlocked checks.
Shared workflow definitions are pinned to superproject commit
`eea341c64a065d8465727919a72bc592878ea70b`; changing their behavior requires an
explicit source update.

`managed-integration` uses a disposable Ubuntu 24.04 hosted runner. Compilation
runs as the ordinary runner user. Only integration execution uses `sudo`, because
configured launch needs coordinator/ptrace privileges. Use a disposable host.

Provision these Actions variables from the agreed delivery inputs:

| Variable | Required value |
| --- | --- |
| `GINUX_REPOSITORY` | Repository containing the maintained launcher source |
| `GINUX_SOURCE_SHA` | Complete 40-character source commit, built in the job |
| `RECIPE_IMAGE_URL` | HTTPS URL for the produced compiler/acquisition rootfs tar |
| `RECIPE_IMAGE_SHA256` | Complete SHA-256 of that tar |

No fork, moving branch, worktree or developer binary is selected by default.
Missing/invalid inputs fail the job. HTTPS verification and a matching digest
are required before managed launch. Produce and retain the image and provenance
with `recipe-image` and the pinned [manifest](../bootstrap/ginux-acquisition-image.json).
Use the output digest from its provenance; the historical eight-RPM `c089…`
image is not the output of the current twelve-RPM manifest. A changed image
needs its own combination verification. The maintained source remains separate.

The script enumerates the current `e2e`, `library_backend` and `image_producer`
catalogs, rejects ignored cases, runs every selected case with retained logs,
checks results and owned process/scratch cleanup, then verifies artifact/source
hashes again. The worker is the same built `recipe` as the CLI. The public-library
case deliberately sets spoofed worker environment variables. No Recipe unit
tests or unit doctests execute.

The standalone job records exclusions for the `kernel_acquisition`,
`kernel_recipes`, `preseed_recipes` and `target_metadata` product modules. They
need actual distro-builder and superproject recipe inputs; they are not counted
as standalone passes. All other enumerated cases and both library/image catalogs
run. Product verification uses `--full-products`, which excludes nothing and
requires explicit `RECIPE_TEST_BUILDER`, `RECIPE_TEST_BUILDER_RECIPES` and
`RECIPE_TEST_REPO_ROOT` inputs.

Build without running unit harnesses:

```sh
mkdir -p .artifacts/managed-ci
cargo test --release --test e2e --test library_backend --test image_producer \
  --no-run --message-format=json > .artifacts/managed-ci/build.json
sudo python3 scripts/managed-integration.py \
  --build-json .artifacts/managed-ci/build.json \
  --ginux /absolute/path/to/built/ginux --ginux-source "$GINUX_SOURCE_SHA" \
  --image /absolute/path/to/rootfs.tar --image-sha256 "$RECIPE_IMAGE_SHA256" \
  --output .artifacts/managed-ci/results
```

The output directory must be new. CI uploads catalogs, exclusions, command/env
and hash identities, every case's logs/receipts and final integrity/results.
Interrupted, failed and omitted cases are never passes. Fixture trees remain
in local output for investigation.

Pushes and ordinary manual dispatches validate only. Publication requires a
manual dispatch with `publish_release: true` and successful shared and managed
CI; `version_bump` is passed to the release producer. Pushing, hosted dispatches
and publishing still require the applicable user authorization.
