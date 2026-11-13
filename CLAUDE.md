# CLAUDE.md - recipe

Rhai-based recipe executor for LevitateOS. The coordinator (`recipe` CLI /
`RecipeEngine`) stages inputs and starts a worker (the same binary) that runs
the recipe: inside a Ginux domain, or, for a recipe that starts no program, as
a confined host process (`src/execution.rs`, `src/host.rs`).

Read these instead of copies here; each fact has one home:

| For | Read |
| --- | --- |
| Running recipes, environment (`GINUX_BIN`, `GINUX_ROOTFS`, `GINUX_ROOT`, `RECIPE_WORKER_BIN`), options, state and logs | `README.md`, `docs/man/recipe.1` |
| Writing recipes: API 2, `RUNTIME`/`ctx`, hooks, readiness, cleanup reasons, `deps`/`build_deps`, `extends` | `WRITING_RECIPES.md`, `docs/man/recipe-recipe.5` |
| Helper catalog | `HELPERS_AUDIT.md`, `docs/man/recipe-helpers.7` (registration: `src/helpers/mod.rs`) |
| Compiler images | `bootstrap/README.md`, `src/bin/recipe-image.rs` |
| Why the code is shaped as it is | `UNTANGLE.md` |

## Design rules

- Lifecycle: `is_installed` → `is_built` → `is_acquired` checks decide which of
  `acquire()` → `build()` → `install()` run. Each phase does one thing and is
  re-verified by its check.
- Recipes never rewrite their own source. Resumable state is `ctx.state`,
  checkpointed by the coordinator with a matching filesystem snapshot.
- The domain layout (`/inputs`, `/work/{build,tools,tmp}`, `/out`) is defined
  once in `src/domain.rs`; the coordinator/worker wire contract in
  `src/worker/protocol.rs`; failure categories in `src/failure.rs`.
- Commands run through `src/process.rs`; human output goes through
  `src/diagnostics.rs` (stdout carries only the final JSON).

## Source map

```
src/
├── bin/recipe.rs, bin/recipe/     CLI (and the private worker entry)
├── bin/recipe-image.rs            compiler-image producer
├── lib.rs                         RecipeEngine (library API)
├── backend.rs, launch.rs, host.rs  coordinator: invocation → Ginux or host run
├── execution.rs                   where each recipe runs (host or Ginux)
├── inputs/                        input discovery, staging, domain config, root
├── needs.rs, needs/               NEEDS declarations, derived Ginux contract, authority reports
├── target.rs                      target metadata manifest (helpers in helpers/target.rs)
├── retained.rs, invocation_log.rs, temporary.rs, publication.rs   coordinator stores
├── worker/                        protocol + in-domain worker
├── core/                          engine: invocation, lifecycle, deps, events, output
├── helpers/                       Rhai-facing helpers
├── llm/, core/autofix/            LLM providers and coordinator autofix
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
