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

## What Belongs Here

- Recipe execution engine
- Rhai helpers (download, extract, install_bin, etc.)
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
```

## Code Structure

```
src/
├── bin/recipe.rs       # CLI entry point
├── lib.rs              # Public API, RecipeEngine
├── core/               # Infrastructure
│   ├── lifecycle.rs    # execute, remove, update
│   ├── context.rs      # Thread-local execution state
│   ├── recipe_state.rs # Persistent variables
│   └── deps.rs         # Dependency resolution
└── helpers/            # Recipe-facing functions
    ├── acquire.rs      # download, copy, verify_sha256
    ├── build.rs        # extract, cd, run
    └── install.rs      # install_bin, install_lib
```

## Key Concepts

1. **Recipes are code** - Rhai scripts that execute, not YAML/TOML
2. **No database** - State written directly to .rhai files
3. **Minimal executor** - Engine provides helpers, recipes do the work
