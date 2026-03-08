# CLAUDE.md - recipe

## Core Design Philosophy - READ THIS FIRST

### Phase Separation is Sacred

Recipe uses a 3-phase lifecycle: `acquire()` → `build()` → `install()`

Each phase:
1. **Checks if already done** - returns early if work exists
2. **Does ONE thing** - acquire OR build OR install, never combined
3. **Enables caching** - user can re-run any phase independently

### WHY This Matters

| Without separation | With separation |
|-------------------|-----------------|
| Change one thing → redo everything | Change one thing → redo only that phase |
| No caching possible | Each phase cached independently |
| Monolithic, fragile | Granular, robust |

### Anti-Patterns (DO NOT DO)

❌ **Monolithic functions** - putting acquire+build+install in one function
❌ **Skipping phases** - calling build() from acquire()
❌ **Combining phases** - downloading AND extracting in acquire()

### The sys.exit(0) Test

Before implementing, ask: "Am I making the test pass, or actually solving the problem?"

If your solution bypasses phase separation to "get it working" - STOP.
That's reward hacking. The easy path and correct path must be the same.

Reference: https://www.anthropic.com/research/emergent-misalignment-reward-hacking

---

## What is recipe?

Rhai-based package manager for LevitateOS. Recipes are executable scripts, not static configs. State lives in recipe files (`installed = true`), not a database.

If you are writing or reviewing recipes, start with `WRITING_RECIPES.md`, then
use `HELPERS_AUDIT.md` to confirm the helper surface that actually exists today.

## Bootstrap Reality

`REQUIREMENTS.md` is broader than the current implementation. For the current binary:

- There is no `--sysroot` or `--prefix` CLI yet.
- Current script constants are `RECIPE_DIR`, `BUILD_DIR`, `ARCH`, `NPROC`, and `RPM_PATH`.
- `BASE_RECIPE_DIR` is only present when a recipe extends a base recipe.
- `TOOLS_PREFIX` is only present while resolving dependency recipes.
- `cleanup(ctx, reason)` is required by this repo's install flow.

Fresh Fedora minimal bootstrap:

```bash
sudo dnf install -y rust cargo gcc git pkgconf-pkg-config
cd tools/recipe
cargo build
./target/debug/recipe --help
```

Optional external commands used by the current implementation:

- `sh` for `shell*` helpers
- `git` for `git_clone*`, autofix patch application, and git repo root detection
- `tar` for `extract_from_tarball()` only
- `df` for `check_disk_space()` only
- `codex` / `claude` for LLM helpers and `--autofix` only

## What Belongs Here

- Recipe execution engine
- Rhai helpers (download, extract, filesystem, shell, etc.)
- Dependency resolution
- CLI (`recipe install`, `recipe list`, etc.)

## What Does NOT Belong Here

| Don't put here | Put it in |
|----------------|-----------|
| System extraction | `tools/recstrap/` |
| Fstab generation | `tools/recfstab/` |
| Chroot setup | `tools/recchroot/` |

## Commands

```bash
cargo build
cargo test
cargo clippy
cargo run -- install ripgrep
cargo run -- list
cargo install --path .
```

`cargo test` expects the surrounding LevitateOS checkout because the crate has a path dev-dependency on `../../testing/cheat-test`.

## Code Structure

```
src/
├── bin/recipe.rs         # CLI entry point
├── lib.rs                # Public API, RecipeEngine
├── core/                 # Execution engine, output, locks, dependency resolution
├── helpers/              # Recipe-facing helper modules
│   ├── acquire/          # download, verify, http, git, torrent
│   ├── build/            # native archive extraction
│   ├── install/          # low-level filesystem, file I/O, disk helpers
│   ├── util/             # shell/process/path/env/string helpers
│   └── llm.rs            # recipe-facing LLM helpers
└── llm/                  # provider config and invocation plumbing
```

## Key Concepts

1. **Recipes are code** - Rhai scripts that execute, not YAML/TOML
2. **No database** - State written directly to .rhai files
3. **Minimal executor** - Engine provides helpers, recipes do the work
