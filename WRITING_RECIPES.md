# Writing Recipes

This is the author-facing guide for the `recipe` implementation that exists
today.

If this guide, `REQUIREMENTS.md`, and the code disagree, trust:

1. current source code
2. `HELPERS_AUDIT.md`
3. this guide
4. `REQUIREMENTS.md`

`REQUIREMENTS.md` is still the broader target spec. This guide is about what
you can actually write and run now.

## Core Model

Recipes are Rhai scripts.

They work by:

- declaring a top-level `ctx` map
- using `is_*` checks that throw when work is needed
- mutating and returning `ctx` from phase functions
- persisting `ctx` back into the recipe file after successful phases

Normal install flow is:

1. `is_installed(ctx)`
2. `is_built(ctx)`
3. `is_acquired(ctx)`
4. `acquire(ctx)` if needed
5. `build(ctx)` if needed and defined
6. `install(ctx)`
7. `cleanup(ctx, reason)` automatically on phase success/failure

Important: `cleanup(ctx, reason)` is effectively mandatory for normal installs
in this repo. If you do not need cleanup, define a no-op hook.

## Minimal Template

This is the smallest useful install recipe shape for the current executor:

```rhai
let ctx = #{
    name: "jq",
    version: "1.7.1",
    url: "https://github.com/jqlang/jq/releases/download/jq-1.7.1/jq-linux-amd64",
    sha256: "replace-me",
    downloaded: "",
    bin_path: "",
};

fn is_installed(ctx) {
    let bin_dir = join_path(env("HOME"), ".local/bin");
    let bin_path = join_path(bin_dir, "jq");
    if !is_file(bin_path) { throw "not installed"; }
    ctx.bin_path = bin_path;
    ctx
}

fn is_acquired(ctx) {
    if ctx.downloaded == "" || !is_file(ctx.downloaded) { throw "not acquired"; }
    ctx
}

fn acquire(ctx) {
    mkdir(BUILD_DIR);
    let downloaded = download(ctx.url, join_path(BUILD_DIR, "jq"));
    verify_sha256(downloaded, ctx.sha256);
    ctx.downloaded = downloaded;
    ctx
}

fn install(ctx) {
    let bin_dir = join_path(env("HOME"), ".local/bin");
    mkdir(bin_dir);

    let dst = join_path(bin_dir, "jq");
    mv(ctx.downloaded, dst);
    chmod(dst, 0o755);

    ctx.bin_path = dst;
    ctx
}

fn cleanup(ctx, reason) {
    ctx
}
```

Notes:

- `build(ctx)` is optional. Omit it for prebuilt binaries.
- `is_built(ctx)` is also optional. Omit it if there is no build phase.
- `remove(ctx)` is optional, but needed if you want `recipe remove` to work.

## Required and Optional Functions

### Required for Normal `recipe install`

- `let ctx = #{ ... };`
- `install(ctx)`
- `cleanup(ctx, reason)`

In practice, most real recipes should also define:

- `is_installed(ctx)`
- `acquire(ctx)`

### Optional

- `is_acquired(ctx)`
- `is_built(ctx)`
- `build(ctx)`
- `remove(ctx)`

Behavior of missing checks:

- missing `is_installed` means install is needed
- missing `is_built` means build is needed
- missing `is_acquired` means acquire is needed

Behavior of missing phases:

- missing `build(ctx)` is fine if your recipe does not need a build phase
- missing `remove(ctx)` means `recipe remove` will fail for that recipe

## The `ctx` Contract

### The Exact `ctx` Literal Matters

`ctx` persistence looks for the exact byte sequence:

```rhai
let ctx = #{
```

That means these are not equivalent for persistence purposes:

- `let ctx = #{` works
- `let ctx=#{` does not
- `const ctx = #{` does not

If the exact form is missing, the recipe can still execute, but persistence will
not find the block and install/remove/cleanup operations will fail when they try
to write state back.

### Keep `ctx` Values Simple

Current persistence round-trips these types cleanly:

- strings
- integers
- booleans
- `()`

Other Rhai values are serialized by stringifying them and then writing the
result as a quoted string. That means arrays, maps, timestamps, and other
non-scalar values are not safe persistent `ctx` state today.

Rule of thumb: keep persistent `ctx` fields scalar and reconstruct richer values
inside checks/phases.

### `ctx` Is Persisted Incrementally

After a successful phase, `ctx` is written back to the recipe source file.

That means:

- a successful `acquire(ctx)` persists even if `install(ctx)` later fails
- rerunning the recipe resumes from the persisted state
- file permissions are preserved where possible
- keys are written back in sorted order, not original source order

This is intentional. It is how resume behavior works.

## Writing Checks Correctly

Checks are not boolean predicates. They are throw-based gates.

- return updated `ctx` when the phase is already satisfied
- `throw` when the phase still needs to run

Example:

```rhai
fn is_acquired(ctx) {
    let src_dir = join_path(BUILD_DIR, "ripgrep-" + ctx.version);
    if !is_dir(src_dir) { throw "source tree missing"; }
    ctx.src_dir = src_dir;
    ctx
}
```

This matters because the executor carries the returned `ctx` forward even when
the phase is skipped.

That lets checks do useful discovery work:

- infer source directories
- rediscover output paths
- refresh cached derived fields

### Actual Flow Rules

Current install flow is:

1. run `is_installed(ctx)`
2. if it passes, stop
3. run `is_built(ctx)`
4. if it passes, skip both `build(ctx)` and `is_acquired(ctx)`
5. if it throws, run `is_acquired(ctx)`
6. run `acquire(ctx)` only if `is_acquired(ctx)` threw or was missing
7. if `build(ctx)` exists, rerun `is_built(ctx)` after acquire
8. run `build(ctx)` only if build is still needed and the function exists
9. run `install(ctx)`

Two practical consequences:

- if `is_built(ctx)` passes, acquire is not checked
- if acquire makes `is_built(ctx)` pass on the second check, build and
  `build_deps` are skipped
- if you define `is_built(ctx)`, it should actually mean "everything needed to
  skip build is already ready"

## Phase Responsibilities

Keep the phases cleanly separated.

### `acquire(ctx)`

Use this for:

- downloads
- checksum verification
- cloning repos

Typical outputs:

- archive path
- source checkout path
- downloaded binary path

### `build(ctx)`

Use this for:

- compilation
- patching
- code generation
- transforming acquired inputs into installable artifacts

Typical outputs:

- build directory
- compiled binary path
- staged artifact path

### `install(ctx)`

Use this for:

- final file placement
- chmod/symlink steps
- installer invocations
- marking installation state

Current implementation note: there is no sysroot/prefix confinement yet. If
your install code points at host paths, it will mutate the host.

### `cleanup(ctx, reason)`

Always define it.

Even if you do nothing:

```rhai
fn cleanup(ctx, reason) { ctx }
```

Automatic reasons used by the executor:

- `auto.acquire.success`
- `auto.acquire.failure`
- `auto.build.success`
- `auto.build.failure`
- `auto.install.success`
- `auto.install.failure`

Manual cleanup uses:

- `manual`

Good uses for cleanup:

- remove temporary files inside `BUILD_DIR`
- normalize `ctx` after a failed phase
- delete half-written intermediate artifacts

## Available Constants

These constants are pushed into the Rhai scope by the executor.

### Always Present for Normal Installs

- `RECIPE_DIR`: parent directory of the recipe file
- `BUILD_DIR`: build/work directory for this run
- `ARCH`: host architecture from Rust `std::env::consts::ARCH`
- `NPROC`: CPU count
- `RPM_PATH`: current `RPM_PATH` environment value, or empty string

### Present Only in Some Contexts

- `BASE_RECIPE_DIR`: parent directory of the base recipe when using
  `//! extends: ...`
- `TOOLS_PREFIX`: only when executing a dependency recipe

### User Defines

`--define KEY=VALUE` pushes a string constant into scope.

Example:

```bash
recipe install foo --define PREFIX=/usr/local
```

Then in Rhai:

```rhai
let dst = join_path(PREFIX, "bin");
```

Values from `--define` are strings.

## Helper Surface

The authoritative helper reference is:

- `HELPERS_AUDIT.md`

Do not rely on the helper list in `REQUIREMENTS.md` alone. It includes helpers
that do not exist yet.

### Helper Categories You Actually Have Today

- path helpers: `join_path`, `basename`, `dirname`
- string helpers: `trim`, `starts_with`, `ends_with`, `contains`, `replace`,
  `split`
- logging helpers: `log`, `debug`, `warn`
- shell helpers: `shell`, `shell_in`, `shell_status`, `shell_status_in`,
  `shell_output`, `shell_output_in`
- process helpers: `exec`, `exec_output`
- file I/O helpers: `read_file`, `read_file_or_empty`, `write_file`,
  `append_file`, `glob_list`
- filesystem helpers: `exists`, `file_exists`, `is_file`, `dir_exists`,
  `is_dir`, `mkdir`, `rm`, `mv`, `ln`, `chmod`
- network/acquire helpers: `download`, `verify_sha256`, `verify_sha512`,
  `verify_blake3`, `fetch_sha256`, `http_get`, `git_clone`,
  `git_clone_depth`, `torrent`, `download_with_resume`
- GitHub helpers: `github_latest_release`, `github_latest_tag`,
  `github_download_release`, `extract_from_tarball`, `parse_version`
- build helpers: `extract`, `extract_with_format`
- env helpers: `env`, `set_env`
- LLM helpers: `llm_extract`, `llm_find_latest_version`,
  `llm_find_download_url`

### `shell*` vs `exec*`

Use `shell*` when you need shell syntax:

- pipes
- redirects
- glob expansion
- variable interpolation in a shell string

Use `exec*` when you already have a command and argument list:

```rhai
exec("strip", ["--strip-unneeded", binary_path]);
```

That avoids shell quoting issues.

### Helper Footguns Worth Knowing

- `git_clone(url, dest_dir)` clones into `dest_dir/<repo-name>`, not exactly to
  `dest_dir`
- `git_clone_depth(url, dest_dir, depth)` behaves the same way
- `extract()` is native Rust and does not depend on host `tar`
- `extract_from_tarball()` does rely on host `tar`
- `rm(pattern)` can recursively remove matched directories; treat it as
  destructive

## Dependencies: `deps` and `build_deps`

Recipes can declare top-level dependency arrays:

```rhai
let deps = ["pkg-config-runtime"];
let build_deps = ["meson", "ninja"];
```

### How They Behave

- `deps` are resolved before the main phase flow and stay on `PATH` for the main
  recipe execution
- `build_deps` are resolved only if build is still needed after checks
- `build_deps` are added to `PATH` only for the actual build portion
- dependency recipes are found by name as `<name>.rhai` under `--recipes-path`
- tools are installed into `BUILD_DIR/.tools`
- tool paths are prepended as `.tools/{usr/bin,usr/sbin,bin,sbin}`

That means your main recipe normally just calls the tool by name:

```rhai
fn build(ctx) {
    shell_in(ctx.src_dir, "meson setup build");
    shell_in(ctx.src_dir, "ninja -C build");
    ctx
}
```

### How Dependency Recipes Run

Dependency recipes are executed differently from normal recipes:

- their `BUILD_DIR` becomes `BUILD_DIR/.deps/<dep-name>`
- they get `TOOLS_PREFIX`
- they do not use normal per-recipe ctx persistence
- they do not take the normal recipe lock
- they use a simplified flow: `is_installed` -> optional `is_acquired` /
  `acquire` -> `install`

If you are authoring a tool recipe meant for `build_deps`, still define
`cleanup(ctx, reason)` if you want cleanup behavior, but the normal top-level
install strictness around cleanup is looser there.

### Environment Fixups for Tool Recipes

When dependency tools are installed, the resolver may also export:

- `BISON_PKGDATADIR`
- `M4`
- `LIBRARY_PATH`
- `C_INCLUDE_PATH`
- `CPLUS_INCLUDE_PATH`
- `PKG_CONFIG_PATH`

Those are only set if the corresponding path exists under `.tools/usr`.

## `extends` Behavior

Recipes can inherit from a base recipe with a leading comment:

```rhai
//! extends: linux-base.rhai
```

Rules:

- it must appear in the leading comment block before the first non-comment line
- base recipe is compiled first
- child AST is merged on top
- child functions with the same name and arity override base functions
- top-level statements run base first, then child
- recursive extends are rejected

### Important Persistence Caveat

If the child recipe does not declare its own `ctx` block, ctx persistence falls
back to the base recipe's `ctx`.

That is useful for shared state, but it can also be a trap:

- running child installs may mutate the shared base recipe file
- multiple child recipes can end up sharing one persisted state source

If you want per-child state, define a `ctx` block in the child recipe.

## Authoring Patterns That Work Well

### Pattern 1: Let Checks Rediscover State

Instead of persisting every derived path forever, let checks reconstruct them.

Good:

```rhai
fn is_built(ctx) {
    let out = join_path(BUILD_DIR, "out/mytool");
    if !is_file(out) { throw "not built"; }
    ctx.out = out;
    ctx
}
```

This keeps `ctx` simple and resilient.

### Pattern 2: Persist Stable Facts, Not Rich Objects

Good `ctx` fields:

- `version`
- `sha256`
- `downloaded`
- `src_dir`
- `bin_path`
- `installed`

Bad current `ctx` fields:

- arrays of file lists
- nested maps
- opaque Rhai objects

### Pattern 3: Prefer Explicit Paths

Use helpers that make file flow obvious:

```rhai
let archive = download(ctx.url, join_path(BUILD_DIR, "src.tar.gz"));
verify_sha256(archive, ctx.sha256);
extract(archive, BUILD_DIR);
```

That matches the implementation style and avoids hidden state.

## Debugging Recipes

### Use the Check Commands

Useful commands:

```bash
recipe isinstalled mypkg
recipe isbuilt mypkg
recipe isacquired mypkg
```

Those run the check directly and return updated `ctx` JSON on success.

### Keep Stdout Clean

The CLI prints final `ctx` JSON to stdout.

Everything else goes to stderr:

- logs
- phase banners
- helper traces
- child process output

If you need reliable machine-readable output, use:

```bash
recipe install mypkg --json-output result.json
```

### Trace Helpers

Set:

```bash
RECIPE_TRACE_HELPERS=1
```

This emits helper-level trace lines.

### Dry-ish Runs

Use:

```bash
recipe install mypkg --no-persist-ctx
```

That disables source mutation while still executing the recipe.

## Common Failure Modes

### Missing `cleanup(ctx, reason)`

Normal install flow will fail before doing work if the recipe lacks a 2-arg
cleanup hook.

Fix:

```rhai
fn cleanup(ctx, reason) { ctx }
```

### `ctx` Persistence Broke After Refactoring

If persistence suddenly stops working, check whether the recipe still uses:

```rhai
let ctx = #{
```

The executor is looking for that exact text.

### Recipe Writes to the Real Host

That is possible today. There is no implemented sysroot/prefix confinement yet.

If you need safe destination routing, pass explicit destinations with
`--define`, keep installs under a controlled prefix, and audit every path.

## Practical Checklist

Before calling a recipe "done", verify:

- `ctx` uses the exact `let ctx = #{` form
- persistent `ctx` fields are scalar
- `is_installed(ctx)` exists and is truthful
- `cleanup(ctx, reason)` exists
- `build(ctx)` is only present if the recipe really has a build phase
- `is_built(ctx)` only passes when build can truly be skipped
- destination paths are explicit and safe
- helper usage matches `HELPERS_AUDIT.md`, not wishful spec helpers

## Related Documents

- `README.md`: current CLI, bootstrap, and runtime reality
- `HELPERS_AUDIT.md`: authoritative current helper list
- `PHASES.md`: lifecycle overview
- `REQUIREMENTS.md`: target design, broader than current implementation
