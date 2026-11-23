# CLAUDE.md - recipe

Rhai-based recipe executor for LevitateOS. The coordinator (`recipe` CLI /
`RecipeEngine`) stages inputs and starts a worker (the same binary) that runs
the recipe: inside a Ginux instance on Ginux's public lifecycle (`create`,
`inject`, `start --wait`, `status`, `extract`, `delete`, through the `ginux`
CLI; its root-owned helper runs behind Recipe's root-owned guard, the only program `sudo` runs), or, for a recipe that starts no
program, as a confined host process (`src/execution.rs`, `src/host.rs`).

Read these instead of copies here; each fact has one home:

| For | Read |
| --- | --- |
| Running recipes, environment (`RECIPE_GINUX`, `RECIPE_IMAGE`, `RECIPE_IMAGE_LAYOUT`, `RECIPE_GINUX_STATE_ROOT`, `RECIPE_WORKER_BIN`; the old `GINUX_BIN`/`GINUX_ROOT*` are refused), privileges (root, or the `recipe-ginux` sudoers rule, which names only the guard), options, state and logs | `README.md`, `docs/man/recipe.1` |
| What the public lifecycle does not provide (no storage limit in a run, per-port network grants) | `docs/runtime-limits.md` |
| The Ginux cutover: design, status, what is unverified | `docs/ginux-cutover-design.md` (fixed record), `docs/ginux-public-lifecycle-migration.md` section 6 |
| Writing recipes: API 1 declarations, functions, constants, steps and states, interruption and restarting, `deps`/`build_deps`, `extends` | `WRITING_RECIPES.md`, `docs/man/recipe-recipe.5`; the full contract is `docs/recipe-format.md` |
| Helper catalog | `HELPERS_AUDIT.md`, `docs/man/recipe-helpers.7` (registration: `src/helpers/mod.rs`) |
| Compiler images | `bootstrap/README.md`, `src/bin/recipe-image.rs` |
| Why the code is shaped as it is | `UNTANGLE.md` |

## Design rules

- No backwards compatibility (the owner's rule, 2026-11-16). Recipe has never
  been released and has no users. The engine, the recipes and everything that
  reads them change together, in one step. Do not add version switches, keep an
  old form working beside a new one, accept two spellings of a field, or shape
  a change so an older Recipe can still read it. When a format changes, change
  every recipe and every reader and delete the old form. The `RECIPE_API` line
  is `1`: the engine accepts that number and nothing else (the 2/3 split of
  2026-11-15 was such a switch; `ctx`, `RUNTIME` and the hooks are gone). See
  `docs/next-recipe-format.md`, "The API number and the rewrite".

- Recipe never asks an AI for anything (the owner's rule, 2026-11-16, stated
  many times). No feature may call a language model or start an agent, in the
  engine, in a recipe helper, or in anything that runs Recipe: no autofix, no
  "ask an agent", no automatic repair or rewriting of recipes. A failure is
  reported; what the user does with it is theirs. `--autofix`, `src/llm` and
  `src/core/autofix` were removed on 2026-11-16; nothing replaces them
  (`docs/next-recipe-format-plan.md`, section 2, requirement 9; phase A1).

- Recipe decides what is done from its own records, never from recipe code:
  a download by its SHA-256 in the download store, a result by its identity and
  the checksums recorded for its files, an installed program by its receipt
  (`recipe state NAME`). A recipe has declarations and optional functions
  (`build`, `configure`, `verify`, `latest`, `cleanup`); there are no hooks and
  no readiness checks. Every step (download, build, install, cleanup) can be run
  alone, and `install` does the missing ones.
- Recipes never rewrite their own source. An interrupted step (download, build,
  install, cleanup) starts again from its beginning; finished steps keep their
  results. There are no checkpoints and no `save`, `saved` or `reached`
  (removed 2026-11-22); `--restore-file` only seeds a run with kept downloads
  and the target view.
- Disk rules: after a successful install the downloads, unpacked sources, `WORK`
  and kept results are deleted unless the keep option is on (`--keep`, remembered
  per program, `--no-keep` undoes it); files are moved into place, not copied,
  where both places are on one disk; a download two programs name goes only when
  neither needs it; `remove` frees everything at once; nothing is hidden in
  `storage`. There are no earlier versions and no go-back.
- Ginux is reached only through the public lifecycle: `src/ginux.rs` is the one
  module that spawns the `ginux` CLI, and `src/launch/lifecycle.rs` is the one
  entry point that runs a recipe in an instance. Do not call `ginux launch`,
  never name the helper in the sudoers rule (it names only `recipe-ginux-guard`; the
  helper's other verbs are root), add a second runtime or keep an old spelling of an environment variable (the
  old `GINUX_BIN`, `GINUX_ROOT`, `GINUX_ROOTFS`, `GINUX_MINIMAL_ROOT[FS]` stop
  Recipe with the name of their replacement). The limits Ginux's public
  lifecycle does not give (storage inside a run, per-destination network) are
  accepted regressions and are written down in `docs/runtime-limits.md`: never
  claim them in code, error text or docs.
- The domain layout (`/inputs`, `/work`, `/out`) is defined
  once in `src/domain.rs`; the coordinator/worker records (the request file, the
  result file, the diagnostic and progress lines) in `src/worker/records.rs`;
  failure categories in `src/failure.rs`.
- Commands run through `src/process.rs`; human output goes through
  `src/diagnostics.rs` (stdout carries only the final JSON).

## Source map

```
src/
├── bin/recipe.rs, bin/recipe/     CLI (and the private worker entry); system_route.rs is `--system` scope
├── bin/recipe-image.rs            compiler-image producer
├── bin/recipe-ginux-guard.rs      root-owned guard the sudoers rule names: Recipe's instance verbs on `recipe-<uid>-*` only, config check (`--check-config`), clears the environment, then runs the Ginux helper
├── lib.rs                         RecipeEngine (library API)
├── backend.rs, launch/, host.rs   coordinator: invocation → Ginux or host run (launch/lifecycle.rs runs one instance through the guard; worker_io.rs reads the console records the helper streams)
├── ginux.rs                       the `ginux` CLI wrapper: one method per verb, the status record, the outcome
├── execution.rs                   where each recipe runs (host or Ginux)
├── inputs/                        input discovery, staging, instance config, the injected tar (inject_archive)
├── declarations.rs                the literal declarations (API line, ABOUT, PACKAGE, SOURCES, FILES, PACKAGES, ACCOUNTS, UPSTREAM, RERUN, functions, imports)
├── pins.rs                        the `NAME.pins` file beside a recipe with PACKAGES
├── needs.rs, needs/               NEEDS declarations, derived Ginux contract, authority reports
├── target.rs                      target metadata manifest (helpers in helpers/target.rs)
├── retained.rs, invocation_log.rs, temporary.rs, publication.rs   coordinator stores
├── worker/                        records (request, result, lines) + in-domain worker
├── core/                          engine: invocation, lifecycle, deps, events, output
│   └── executor/private/builtin/  the steps Recipe does itself: fetch.rs, place.rs, system.rs
├── helpers/                       Rhai-facing helpers (tree.rs: put, put_link, tree_text, enable_service)
└── domain.rs, failure.rs, diagnostics.rs, process.rs, fsx.rs, image.rs   shared leaves (image.rs: the OCI layout writer and layer validator)
```

## Commands

```bash
cargo build                 # from the LevitateOS workspace; binaries land in ../../target
cargo check
cargo clippy
```

Integration tests need a real Ginux (`RECIPE_GINUX`, the five Ginux files
root-owned at `/usr/libexec/ginux`), the compiler image as an OCI layout
(`RECIPE_IMAGE`, `RECIPE_IMAGE_LAYOUT`) and root or the sudoers rule; the
preconditions of each suite are in its header (`tests/e2e.rs` has the common
ones). See "Reproduction" in the superproject's `docs/recipe-local-validation.md`.
Unit tests are prohibited (see `AGENTS.md`). The cutover is written and has never
been run: the catalog and the two build-base digests in `src/needs/contract.rs`
are placeholders until `docs/ginux-cutover-design.md` section 6 derives them.
