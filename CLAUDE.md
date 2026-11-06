# CLAUDE.md - recipe

Rhai-based recipe executor for LevitateOS. Every executable operation runs in a
Ginux domain: the coordinator (`recipe` CLI / `RecipeEngine`) stages inputs,
launches the Ginux supervisor, and a worker (the same binary) runs the recipe
inside the domain.

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
├── backend.rs, launch.rs          coordinator: invocation → Ginux run
├── inputs/                        input discovery, staging, domain config, root
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
