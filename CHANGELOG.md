# Changelog

All notable changes to this project will be documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

- A run's network byte cap has room for more than the payload: it is the declared bytes (and a group's pins) plus 64 KiB per file (TLS handshake, HTTP headers, a redirect) plus a quarter of the payload (retries, record framing), one rule for Ginux runs, which count every byte sent and received, and host runs (`docs/recipe-format.md`, section 2.1). A package group no longer fails near the end of its pinned downloads with "Disk quota exceeded"; `SOURCES` recipes get the same allowance on top of their declared `network_max_bytes`. A transfer stopped by the cap now says that the run's network byte limit was reached, with the cap and the fields that set it.
- The recipe digest of a produced package (receipt `recipe_digest`, "already holds" in a system change) and the state key name the recipe's content and its files' names beside it (recipe, pins, base, modules), not the folder it lies in: the medium's copy and a release's copy of the same group match, so `apply-release` no longer produces every group again. `--system installed --json` reports `from_release` for a package installed from any copy of a recipe the release carries with the same content.
- An update by name of a package with no recipe in any source no longer uses a recipe path the receipt recorded that is gone (such as the install medium's): it says so and how to give the recipe.
- `record-base` no longer records the staged tree's `.recipe-target-metadata.json` in the base entry (it rewrote that file afterwards, so a later `apply-release` saw it as changed); `apply-release` neither writes nor compares it, nor any other path the base never holds, also where an older base entry recorded it.

### Changed

- Downloads (`download`, `download_exact`, the metadata fetches, and the built-in fetch of `SOURCES` and pinned packages) are tried again after a transient network failure: name lookup, refused/reset/timed-out connection, cut-off body, HTTP 408, 425, 429, 500, 502, 503, 504. Up to 4 attempts, after 1, 3 and 9 s, each retry printed with host, path and reason; the final error says how many attempts were made. A 404, a checksum mismatch, a refused host or a size limit still fails at once.
- The stdio frame protocol between coordinator and worker is gone. The coordinator stages the request as the reserved input `.recipe-request.json`; the worker writes its result as the file `/work/.recipe-result.json` (written atomically, naming its version and invocation id); diagnostics and progress are JSON lines on the worker's stdout, and a line that is not such a record is kept as plain log text. A host-mode worker learns its root from `RECIPE_HOST_ROOT`. The records version is 14. `RECIPE_FRAME_TIMEOUT_SECS` is renamed `RECIPE_OUTPUT_TIMEOUT_SECS` (same default 300 s and range): the limit is now how long the worker may go without a complete output line. Internally `src/launch.rs` is `src/launch/` (`configured::run` is the one entry point of the configured `ginux launch`), and `src/needs/contract.rs` holds the runtime pins in one table.
- Recipe runs recipes on Ginux's public lifecycle (`create`, `inject`, `start --wait`, `status`, `extract`, `delete`) with the native configured runtime and contract catalog 2, not on the downstream `ginux launch` (catalog 1002); `GINUX_BEHAVIOUR` is 2, the worker records version is 15 and every saved state changes once. Each run is an instance `recipe-<uid>-<pid>-<random>` that is always deleted, and the next Ginux-using command deletes the leftovers of dead runs. Privileges: every privileged step is the Ginux helper run through `sudo`, so Ginux runs need root or the sudoers rule `recipe runtime stage --sudoers GROUP` writes (`/etc/sudoers.d/recipe-ginux`, `%wheel` or `%sudo`); recipes that start no program still run on the host for everyone. Environment: `RECIPE_GINUX` (the `ginux` CLI), `RECIPE_GINUX_STATE_ROOT` (default `/var/lib/recipe/ginux`), `RECIPE_GINUX_RUN_ROOT`, `RECIPE_IMAGE` (`local/recipe-compiler@sha256:<manifest digest>`) with `RECIPE_IMAGE_LAYOUT`, `RECIPE_MINIMAL_IMAGE[_LAYOUT]`, and `GINUX_INSTANCE_HELPER` passed through; Recipe stops on `GINUX_BIN`, `GINUX_ROOT`, `GINUX_ROOTFS`, `GINUX_MINIMAL_ROOT` and `GINUX_MINIMAL_ROOTFS` and names the replacement. The compiler image is an OCI image layout whose identity is its manifest digest (`recipe-image` writes `oci-layout`, `index.json`, `blobs/sha256/…`, `image.oci.tar` and a `provenance.json` with `manifest_digest` and `layer_sha256`; the runtime asset is `image.oci.tar.zst`), imported once per host. `recipe runtime stage` takes `--ginux-cli`, `--ginux-helper`, `--ginux-image`, `--ginux-runtime`, `--ginux-allocator` and `--sudoers GROUP`; `recipe runtime status` reports the files, the state root, the import and the sudo rule. The catalog and both build-base digests are placeholders until the verification pass derives them. Accepted regressions, stated in `docs/runtime-limits.md`: there is no storage limit inside a run (`storage_max_bytes` and `storage_max_objects` bound the archive taken out, and a free-space watchdog cancels a run on a nearly full disk); network grants are per port on the shared host network (a program a recipe starts can reach any address on a granted port, with no connection or byte cap; only Recipe's own downloads are held to the declared `HOST:PORT` and the byte cap, and `network_usage` is what the worker counted); live system roots are copied, not bound; the offline cache reaches a run only by declared pin; inputs and outputs carry no hard links; each run expands the image where the state root has no reflinks. Design: `docs/ginux-cutover-design.md`; status: `docs/ginux-public-lifecycle-migration.md`, section 6. Unbuilt, never run.
- The sudoers rule `recipe runtime stage --sudoers GROUP` writes names Recipe's guard, `/usr/libexec/recipe/recipe-ginux-guard` (root-owned 0755; `runtime stage` ships it with the new required `--recipe-guard FILE`, `runtime status` reports it and checks the rule with the full form of a call, and the standalone installer installs it with its directory), no longer the Ginux helper. A rule naming the helper gave every group member passwordless root through any helper verb (`prepare-root /`). The guard allows only Recipe's instance verbs on instances named `recipe-<uid>-*`, sanitises the imported instance config (`recipe-ginux-guard --check-config FILE` runs that check without root), clears the environment and then runs the helper; instance directories are root-private (0700) and the helper streams the console output to the caller. The result is a passwordless sandbox for group members, not a passwordless root; `docs/runtime-limits.md` lists the remaining exposure. New e2e cases: `tests/e2e/guard.rs` (no root, no sudo).
- The guard takes the caller only from `SUDO_UID`/`SUDO_GID`, reads the imported config with the caller's identity and re-renders it from validated fields. For a caller other than root the image's exact reference must be listed in the root-owned allowlist `/usr/libexec/recipe/runtime-images`, which `recipe runtime stage`, `sudo recipe runtime fetch` and the standalone installer write (`recipe runtime status` reports it). `import-image` is not available through the guard: images are imported as root by `sudo recipe runtime fetch` (the installer runs it), and a run by anyone else without an imported image stops and says to run it. Instance directories are root-private (0700), `start --wait --stream-console` streams the console to the caller, and on a non-zero outcome the helper appends the last 64 KiB of the audit log to standard error. The e2e harness imports the image once per test process with `sudo recipe runtime fetch` when it does not run as root. Remaining exposure, stated plainly in `docs/runtime-limits.md` and not a guarantee of harmlessness: image unpacking runs as root and follows symlinks in the image (R13 `apply_whiteout`, R14 `prepare_root`, unfixed bugs in upstream's code), every user's workload runs as host uid 65534 (R15), there are no per-uid caps, and the free-space watchdog and the byte and extract bounds are Recipe's, enforced in the client.

### Removed

- Checkpoints: the recipe helpers `save`, `saved`, `checkpoint` and `reached`, the coordinator's snapshot control channel (`--control-fd`) and the checkpoint and acknowledgement operations of the worker protocol. An interrupted step (download, build, install, cleanup) starts again from its beginning; finished steps keep their results as before (`docs/recipe-format.md`, section 7). The kept-state record format is now version 3 (no saved values; earlier records are not read), `recipe state --json` `on_disk` no longer has `unfinished`, and the `unfinished` storage kind now means only published outputs no command took. `--restore-file` stays as the way to seed kept downloads and the target view into a run; it restores no checkpoint. The kernel recipe `linux` (distro/builder) is split into `linux-source`, `linux-compile` and `linux` so a long compile is kept as a finished result.
- The downstream launcher path: `src/launch/configured.rs` and `ginux launch`, the launcher's `GINUX_BIN`, `GINUX_ROOT`, `GINUX_ROOTFS`, `GINUX_MINIMAL_ROOT` and `GINUX_MINIMAL_ROOTFS`, `RECIPE_ARCHIVE_IMPORT` and `RECIPE_RANDOM_DEVICES` with the launcher probes, the one-stream archive import (`[storage] archive_import`), owned `memory:` storage and its limits, the mediated network (endpoints, connection and byte caps in the instance), read-only binds of live roots and of the offline cache, the Recipe-side image unpacking (`image_cache`, `VerifiedImage`, `prepare_root`) and the user-namespace launch with its AppArmor profile (`recipe runtime stage --apparmor`, `data/apparmor-ginux`), the `refused` field of failed `--machine-events` and the `ginux: network-summary` and denial-summary reports.

## [0.2.0] - 2026-11-17

- First release under the package name `recipe-pm`, for any x86_64 Linux, as a tarball, RPM, .deb and APK (plus AUR and Void build files).
- Recipe format API 1 (`docs/recipe-format.md`) replaces API 2/3, `ctx`, `RUNTIME` and the hooks.

### Added

- After every system change to a root (`--system install`, `update`, `remove`,
  `apply-release`, and a part written again after `recab copy`), Recipe rebuilds
  the root's glibc library cache `/etc/ld.so.cache` with the root's own
  `ldconfig -X -i -r ROOT`, with or without `--no-effects`; a musl root is left
  alone. JSON `effects.library_cache`; a failure is reported and keeps the change.
  `etc/ld.so.cache` is a machine file, never part of `base`.
  A part written again after `recab copy` now ends through the same root effects
  as every other change: the root's effects recipe (unless `--no-effects`) and then
  the library cache, once after all such parts; a failure is a warning and the
  parts stay written.
- `ABOUT.libc` (a list from `"glibc"` and `"musl"`) names the C libraries a
  recipe's build works with, and `RUNTIME.host.libc` gives the C library of the
  system the package is installed on, decided from its dynamic loaders (`"unknown"`
  when both or neither are present). `install` and `update` compare them for the
  recipe and its runtime dependencies before acquiring anything, and on a
  difference state the facts and the flag that installs anyway, `--any-libc`
  (JSON failure kind `libc_mismatch`). `ABOUT.libc_carried: true` marks a
  package that brings its own C library and loader; the facts then say where it
  was made and tried. `recipe info` shows both; `list --json` and `index` carry
  them in `about`. `list --json` also has `host_libc` at the top and, per
  recipe, `runs_here` and `not_runnable_reason` (the message an install would give).
- `RUNTIME.paths.prefix`: the folder a package output will live in once
  activated (unit with `--no-activate`); it keys saved state, so a build
  belongs to its store.
- `carry_libraries` rewrites files linked elsewhere (upstream prebuilt
  releases) with a new loadable segment for their loader and `$ORIGIN`
  library folders, takes the C library whole from the folder of its loader,
  and checks that the package holds everything it loads.

### Changed

- API 2 recipes run under coordinator-owned state, checkpointing and
  publication, either in a Ginux domain or, when no code of the recipe can start
  a program, as a confined host process (`--execution auto|ginux|host`); see
  `README.md`, "Execution modes".
- Worker protocol v7: one failure record per result, optional ack generations,
  cleanup reasons only on cleanup. Saved state from earlier versions is not reused.
- Default destinations are per recipe under the state directory.
- The library API returns the final context as JSON.

- `--system` scope: on an A/B system, install, update and remove write into the
  unlocked other root (found with `recab status --json`), keep
  `other-root.json` in the package root, and write again only the system part
  that `recab copy` wiped; `installed` and `isinstalled` say which software is
  in the booted root and which in the other root.

### Removed

- `recipe install --autofix` and its options, `--llm-profile`, `recipe/llm.toml` and the
  LLM providers were removed; Recipe never asks an AI.
- `recipe go-back`, `recipe hold`/`unhold`, `recipe storage --clear-earlier` and the
  `held`/`kept` fields were removed; the package store keeps only the active generation.

### Fixed

- A host run moves its outputs into publication instead of archiving and
  unpacking them, wherever the run's directory and the state root share a disk
  (before, only a system change's producer did): a result is no longer on disk
  three times at the end of a run. With saved state the run's archive is still
  kept as the result until cleanup. Placing `FILES` was already a rename in both
  kinds of run; `docs/recipe-format.md` section 6.5 now says what is still copied
  (a Ginux run's outputs are unpacked from its export archive).
- A recipe that states `PACKAGE.name` no longer fails in the worker with
  "the file is main.rhai": the coordinator sends the worker the recipe's name
  (request `recipe_name`, transport 12), and the worker no longer takes it from
  the staged `main.rhai`. Events and messages of a recipe without
  `PACKAGE.name` name the recipe instead of `main`.
- Tar hardlink members extract relative to the destination.
- Zip symlink members extract as links; archive modes drop setuid/setgid/sticky.
- `git apply` during autofix no longer writes to the CLI's JSON stdout.

## [0.1.4] - 2026-03-13

### Added

- Reproducible Podman test runners for Alpine, Fedora, and Rocky
- Alpine `.apk` packaging script for release artifacts

### Changed

- Added LLM-oriented man pages and improved CLI help coverage
- Added pure file and package helper coverage, including Alpine `apk_*` helpers
- Package-manager helpers now fail fast on missing host tooling and support `doas`

## [0.1.0] - 2026-01-16

- Initial release (an S-expression recipe format later replaced by Rhai recipes;
  see the git history for the original feature list).
