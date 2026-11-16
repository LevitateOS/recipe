# CLAUDE.md - recipe

Rhai-based recipe executor for LevitateOS. The coordinator (`recipe` CLI /
`RecipeEngine`) stages inputs and starts a worker (the same binary) that runs
the recipe: inside a Ginux domain, or, for a recipe that starts no program, as
a confined host process (`src/execution.rs`, `src/host.rs`).

Read these instead of copies here; each fact has one home:

| For | Read |
| --- | --- |
| Running recipes, environment (`GINUX_BIN`, `GINUX_ROOTFS`, `GINUX_ROOT`, `RECIPE_WORKER_BIN`), options, state and logs | `README.md`, `docs/man/recipe.1` |
| Writing recipes: API 1 declarations, functions, constants, steps and states, saved values, `deps`/`build_deps`, `extends` | `WRITING_RECIPES.md`, `docs/man/recipe-recipe.5`; the full contract is `docs/recipe-format.md` |
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
- Recipes never rewrite their own source. A value that must survive an
  interruption is `save`d and kept with a `checkpoint` (coordinator snapshot of
  the files and the values together); `saved` reads it, `reached` says where a
  resumed `build` stopped. A saved value never proves work is done; one
  checkpoint at most, deleted when the install succeeds (`src/core/saved.rs`).
- Disk rules: after a successful install the downloads, unpacked sources, `WORK`
  and saved state are deleted unless the keep option is on (`--keep`, remembered
  per program, `--no-keep` undoes it); files are moved into place, not copied,
  where both places are on one disk; a download two programs name goes only when
  neither needs it; `remove` frees everything at once; nothing is hidden in
  `storage`. There are no earlier versions and no go-back.
- The domain layout (`/inputs`, `/work`, `/out`) is defined
  once in `src/domain.rs`; the coordinator/worker wire contract in
  `src/worker/protocol.rs`; failure categories in `src/failure.rs`.
- Commands run through `src/process.rs`; human output goes through
  `src/diagnostics.rs` (stdout carries only the final JSON).

## Source map

```
src/
├── bin/recipe.rs, bin/recipe/     CLI (and the private worker entry); system_route.rs is `--system` scope
├── bin/recipe-image.rs            compiler-image producer
├── lib.rs                         RecipeEngine (library API)
├── backend.rs, launch.rs, host.rs  coordinator: invocation → Ginux or host run
├── execution.rs                   where each recipe runs (host or Ginux)
├── inputs/                        input discovery, staging, domain config, root
├── declarations.rs                the literal declarations (API line, ABOUT, PACKAGE, SOURCES, FILES, PACKAGES, ACCOUNTS, UPSTREAM, RERUN, functions, imports)
├── pins.rs                        the `NAME.pins` file beside a recipe with PACKAGES
├── needs.rs, needs/               NEEDS declarations, derived Ginux contract, authority reports
├── target.rs                      target metadata manifest (helpers in helpers/target.rs)
├── retained.rs, invocation_log.rs, temporary.rs, publication.rs   coordinator stores
├── worker/                        protocol + in-domain worker
├── core/                          engine: invocation, lifecycle, deps, events, output
│   ├── saved.rs                   save, saved, checkpoint, reached
│   └── executor/private/builtin/  the steps Recipe does itself: fetch.rs, place.rs, system.rs
├── helpers/                       Rhai-facing helpers (tree.rs: put, put_link, tree_text, enable_service)
└── domain.rs, failure.rs, diagnostics.rs, process.rs, fsx.rs, image.rs   shared leaves
```

## Commands

```bash
cargo build                 # from the LevitateOS workspace; binaries land in ../../target
cargo check
cargo clippy
```

Integration tests need a real Ginux launcher, a prepared compiler image and
privileges; see "Reproduction" in the superproject's
`docs/recipe-local-validation.md`. Unit tests are prohibited (see `AGENTS.md`).
