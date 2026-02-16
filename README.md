# recipe

Local-first package recipe executor for LevitateOS.

- Recipes are **Rhai scripts**.
- State lives in the recipe file itself as a `ctx` map (`let ctx = #{ ... };`) and is persisted after each phase.
- The CLI is designed to keep **stdout machine-readable** (final `ctx` JSON) and send logs/tool output to **stderr**.

If you are looking for deeper docs:

- Spec and design requirements: `tools/recipe/REQUIREMENTS.md`
- Current helper API surface (authoritative): `tools/recipe/HELPERS_AUDIT.md`
- Lifecycle notes: `tools/recipe/PHASES.md`

## Status

**Alpha.** The implementation is intentionally explicit and conservative.

Implemented:

- Phase executor with `is_*` checks, `acquire/build/install`, ctx persistence
- `//! extends: <base.rhai>` (AST merge: base runs first, child overrides)
- Per-recipe execution lock (`.rhai.lock`)
- Build dependency resolver (`deps` and `build_deps`) that installs tool recipes into `BUILD_DIR/.tools`
- LLM helpers (`llm_extract`, etc) and an opt-in LLM-based repair loop (`--autofix`)

Not implemented yet (still in the spec):

- Sysroot/prefix confinement for safe A/B composition
- Atomic staging/commit and `installed_files` tracking
- Higher-level install helpers (`install_bin`, `install_to_dir`, etc.)
- Update/upgrade lifecycle commands

## Output Discipline (Important)

The `recipe` CLI prints the final `ctx` JSON to stdout (or writes it to `--json-output <file>`).

- All recipe logs, phase banners, helper traces, shell output, and LLM provider output are written to **stderr**.
- If you want a clean JSON pipeline, prefer `--json-output` for long-running installs.

## Recipe Model

Recipes use a `ctx` map for state.

- Check functions (`is_*`) should `throw` when the phase needs to run.
- Phase functions (`acquire/build/install/remove/cleanup`) take `ctx` and return an updated `ctx`.
- `cleanup(ctx, reason)` is **required** for normal installs in this repo. If you do not need cleanup, make it a no-op that returns `ctx`.

Minimal example:

```rhai
let ctx = #{
    name: "ripgrep",
    version: "14.1.0",
    url: "https://example.com/ripgrep.tar.gz",
    sha256: "abc123...",
    archive: "",
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
    // Place files using helpers or shell/exec.
    ctx.installed = true;
    ctx
}

fn cleanup(ctx, reason) {
    // Called automatically on success/failure paths, and via `recipe cleanup`.
    ctx
}
```

### Cleanup Reasons

`cleanup(ctx, reason)` is invoked with:

- `manual` for `recipe cleanup`
- `auto.acquire.success`, `auto.acquire.failure`
- `auto.build.success`, `auto.build.failure`
- `auto.install.success`, `auto.install.failure`

### Extending Recipes

At the top of a recipe, you can declare a base recipe:

```rhai
//! extends: linux-base.rhai
```

Behavior:

- Base recipe is compiled first, then merged with the child AST.
- Child functions with the same name and arity override base functions.
- Top-level statements run base-first, then child.
- Recursive `extends` is rejected.

## Dependency Recipes (`deps` and `build_deps`)

Recipes may declare:

- `let deps = ["foo", "bar"];` for dependencies needed across phases.
- `let build_deps = ["linux-deps"];` for tool dependencies needed only when building.

The resolver:

- Executes dependency recipes from `--recipes-path`.
- Installs tools into `BUILD_DIR/.tools`.
- Prepends `.tools/{usr/bin,usr/sbin,bin,sbin}` to `PATH` for the duration of the phase.
- Exposes `TOOLS_PREFIX` to dependency recipes.

## CLI

```bash
recipe install <name-or-path>
recipe remove <name-or-path>
recipe cleanup <name-or-path>
recipe list
recipe info <name-or-path>
recipe hash <file>
```

Resolution for `<name-or-path>`:

- Absolute paths are used as-is.
- Relative paths are tried as-is, then under `--recipes-path`, then with `.rhai` appended.

### Global Flags

- `-r, --recipes-path <dir>`: where to search for `<name>.rhai` and resolve `//! extends:`
- `-b, --build-dir <dir>`: where downloads/build artifacts go (otherwise a kept temp dir)
- `--define KEY=VALUE`: inject constants into the Rhai scope before execution (repeatable)
- `--json-output <file>`: write the final ctx JSON to a file (stdout stays quiet)
- `--llm-profile <name>`: select a profile from XDG `recipe/llm.toml` (see below)

### Install Flags (Autofix)

`recipe install` also supports:

- `--autofix`: on selected failures, ask the configured provider to return a unified diff, apply it, and retry
- `--autofix-attempts <n>`: maximum patch attempts (default: 2)
- `--autofix-cwd <dir>`: working directory used for LLM invocation and `git apply` (defaults to detected git repo root)
- `--autofix-prompt-file <file>`: append extra instructions to the autofix prompt
- `--autofix-allow-path <path>`: constrain patched files to allowed roots (repeatable)

## LLM Integration

Recipe integrates with external CLIs for two things:

- Rhai helpers: `llm_extract`, `llm_find_latest_version`, `llm_find_download_url`
- The opt-in repair loop: `recipe install --autofix`

Design constraints:

- Codex and Claude have **equal footing**. There is no implicit fallback.
- Recipe does **not** interpret model output. It returns raw text, or in autofix mode it applies a unified diff.

### XDG Config: `recipe/llm.toml`

Config is loaded from:

- `$XDG_CONFIG_DIRS/recipe/llm.toml` (default: `/etc/xdg/recipe/llm.toml`)
- `$XDG_CONFIG_HOME/recipe/llm.toml` (default: `~/.config/recipe/llm.toml`)

Multiple files are merged (user config overrides system config).

Example:

```toml
version = 1
default_provider = "codex" # or "claude" (required)
default_profile = "kernels_nightly" # optional
timeout_secs = 300
max_output_bytes = 10485760
max_input_bytes = 10485760

[providers.codex]
bin = "codex"
args = ["--sandbox", "read-only", "--skip-git-repo-check"]

[providers.claude]
bin = "claude"
args = ["-p", "--output-format", "text", "--no-chrome"]

[profiles.kernels_nightly]
default_provider = "codex"

[profiles.kernels_nightly.providers.codex]
model = "gpt-5.3-codex"
effort = "xhigh" # mapped to Codex config `model_reasoning_effort`
```

Provider keys you can set (globally under `[providers.*]` or per profile under `[profiles.<name>.providers.*]`):

- `bin`: executable name or path
- `args`: extra CLI args
- `model`: provider-specific model id
- `effort`: provider-specific effort control
- `config`: (Codex only) repeated `--config key=value` overrides
- `env`: environment variables to add to the provider process

Notes:

- Codex is invoked via `codex exec` and Recipe disables the Codex `shell_tool` for these calls.
- MCP servers are hard-disabled for Codex runs by overriding `mcp_servers.<name>.enabled=false` at invocation time.

## Autofix Prompts

Autofix prompt sources (in order):

- A built-in base prompt (unified diff only, no prose)
- Optional recipe-supplied block starting with `// AUTOFIX_PROMPT:` in the recipe (or its base recipe)
- Optional `--autofix-prompt-file` contents

Recipe-supplied prompt block format:

```rhai
// AUTOFIX_PROMPT: High-level instruction line.
// More instructions...
// Blank line ends the block.
```

Autofix guardrails:

- Patch output must be a unified diff suitable for `git apply`.
- Patch paths must stay within `--autofix-allow-path` roots.
- Patches that modify `is_installed/is_built/is_acquired` or introduce `|| true` are rejected.

## Default Recipes Path

If `--recipes-path` is not provided, the CLI uses:

- `$RECIPE_PATH` if set
- otherwise `$XDG_DATA_HOME/recipe/recipes` (default: `~/.local/share/recipe/recipes`)

## Building

```bash
cargo build -p levitate-recipe
```

## License

MIT OR Apache-2.0
