# CLAUDE.md - recipe

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
