# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- `RUNTIME.paths.prefix`: the folder a package output will live in once
  activated (unit with `--no-activate`); it keys saved state, so a build
  belongs to its store.
- `carry_libraries` rewrites files linked elsewhere (upstream prebuilt
  releases) with a new loadable segment for their loader and `$ORIGIN`
  library folders, takes the C library whole from the folder of its loader,
  and checks that the package holds everything it loads.

### Changed

- Every executable operation runs in a Ginux domain (API 2 recipes, coordinator-
  owned state, checkpointing and publication); see `README.md`.
- Worker protocol v7: one failure record per result, optional ack generations,
  cleanup reasons only on cleanup. Saved state from earlier versions is not reused.
- Default destinations are per recipe under the state directory.
- The library API returns the final context as JSON.

### Fixed

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
