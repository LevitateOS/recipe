# recipe

Local-first recipe executor for LevitateOS. Recipes are Rhai scripts. State is stored in the recipe file itself (in a `ctx` map) and is persisted after each phase.

If you are looking for:

- the full spec: `tools/recipe/REQUIREMENTS.md`
- the real, current helper API surface: `tools/recipe/HELPERS_AUDIT.md`

## Status

**Alpha.** The implementation currently focuses on executing individual recipes and providing a small, explicit helper surface.

What works today:

- `acquire(ctx) -> ctx` → `build(ctx) -> ctx` (optional) → `install(ctx) -> ctx`
- ctx persistence back into the recipe (`let ctx = #{ ... };`)
- `//! extends: <base.rhai>` (AST merge: base runs first, child overrides)
- Per-recipe execution lock (`.rhai.lock`)
- Downloads (HTTP + resume), git clone, torrents (pure Rust `librqbit`), native archive extraction

Not implemented yet (but required by the spec):

- Sysroot/prefix plumbing + confinement for safe A/B composition
- Atomic staging/commit and `installed_files` tracking
- Higher-level install helpers (`install_bin`, `install_to_dir`, etc.)
- Update/upgrade/refresh lifecycle commands

## Recipe Pattern (Implemented)

Recipes use a `ctx` map for state. Phase functions take `ctx` and return an updated `ctx`. Check functions (`is_*`) should `throw` when the phase needs to run.

```rhai
let ctx = #{
    name: "ripgrep",
    version: "14.1.0",
    url: "https://example.com/ripgrep.tar.gz",
    sha256: "abc123...",
    archive: "",
    prefix: "/usr",
    installed: false,
};

fn is_acquired(ctx) {
    if ctx.archive == "" || !is_file(ctx.archive) { throw "not acquired"; }
    ctx
}

fn acquire(ctx) {
    mkdir(BUILD_DIR);
    let archive = download(ctx.url, join_path(BUILD_DIR, "src.tar.gz"));
    verify_sha256(archive, ctx.sha256);
    ctx.archive = archive;
    ctx
}

fn is_installed(ctx) {
    if !ctx.installed { throw "not installed"; }
    ctx
}

fn install(ctx) {
    // Use filesystem helpers or shell/exec to place files under ctx.prefix (or any destination you decide).
    ctx.installed = true;
    ctx
}
```

## Commands (Implemented)

```bash
recipe install <name-or-path>   # Execute acquire/build/install
recipe remove <name-or-path>    # Execute remove(ctx) (if present)
recipe cleanup <name-or-path>   # Execute cleanup(ctx) (if present)
recipe list                     # List *.rhai in recipes dir
recipe info <name-or-path>      # Show basic metadata from ctx
recipe hash <file>              # Compute sha256/sha512/blake3
```

Global flags:

- `-r, --recipes-path <dir>`: where to search for `<name>.rhai`
- `-b, --build-dir <dir>`: where downloads/build artifacts go (otherwise a kept temp dir)
- `--define KEY=VALUE`: inject constants into the Rhai scope before execution (repeatable)
- `--json-output <file>`: write the final ctx JSON to a file

## Helpers

The authoritative helper inventory is `tools/recipe/HELPERS_AUDIT.md`.

Helpers are registered in `tools/recipe/src/helpers/mod.rs`.

## Script Constants

Always provided:

- `RECIPE_DIR`
- `BUILD_DIR`
- `ARCH`
- `NPROC`

Sometimes provided:

- `BASE_RECIPE_DIR` (only when using `//! extends:`)
- `RPM_PATH` (from environment)
- `TOOLS_PREFIX` (only when executing dependency recipes via the build-deps resolver)

User-provided via `--define`:

- Anything you need (e.g. `PREFIX`, `SYSROOT`, custom URLs/versions, etc.)

## Building

```bash
cargo build -p levitate-recipe
```

## License

MIT OR Apache-2.0
