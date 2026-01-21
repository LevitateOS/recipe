# CLAUDE.md - Recipe Package Manager

## ⛔ STOP. READ. THEN ACT.

Every time you think you know where something goes - **stop. Read first.**

Every time you think something is worthless and should be deleted - **stop. Read it first.**

Every time you're about to write code - **stop. Read what already exists first.**

The five minutes you spend reading will save hours of cleanup.

---

## What is recipe?

A Rhai-based package manager where recipes are executable code, not static configs. State lives in the recipe files themselves - no database.

## Key Concepts

1. **Recipes are code** - Rhai scripts that run, not YAML/TOML configs
2. **No database** - `installed = true` is written directly to the .rhai file
3. **Minimal executor** - Engine provides helpers, recipes do the work

## Development

```bash
# Build
cargo build

# Run tests
cargo test

# Check with clippy
cargo clippy

# Run the CLI
cargo run -- install ripgrep
cargo run -- list
```

## Code Structure

```
src/
├── bin/recipe.rs       # CLI entry point
├── lib.rs              # Public API, RecipeEngine
├── core/               # Infrastructure
│   ├── lifecycle.rs    # execute, remove, update, upgrade
│   ├── context.rs      # Thread-local execution state
│   ├── recipe_state.rs # Persistent variables
│   ├── deps.rs         # Dependency resolution
│   └── output.rs       # Terminal formatting
└── helpers/            # Recipe-facing functions
    ├── acquire.rs      # download, copy, verify_sha256
    ├── build.rs        # extract, cd, run
    ├── install.rs      # install_bin, install_lib, install_man
    └── ...             # Other helper modules
```

## Common Mistakes

1. **Adding state to the engine** - State belongs in recipe files, not engine
2. **Making recipes too smart** - Keep recipes simple, let engine provide utilities
3. **Breaking recipe validation** - Required vars/functions must always be checked

## Testing

All helper functions should be tested. Test recipes live in `tests/` and `examples/`.
