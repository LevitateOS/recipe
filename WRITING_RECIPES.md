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

## Required execution environment

Operations that evaluate recipes use Ginux on Linux x86_64. Select the launcher
with `GINUX_BIN`. Recipe invokes `ginux launch <config> <prepared-rootfs>
--result-file <fresh-path>` and reports unavailable capabilities as failures.

`GINUX_ROOTFS` selects an assembled, uncompressed rootfs tar. Its SHA-256 must
match `GINUX_ROOT`, whose format is `oci:NAME@sha256:CONTENT_DIGEST` (the digest
of the assembled tar, rather than an OCI manifest digest). With no override,
Recipe uses its pinned Rocky image identity and looks for the tar at
`$XDG_CACHE_HOME/recipe/images/<digest>.tar`, or the platform cache directory
when `XDG_CACHE_HOME` is unset. Acquisition of the environment is explicit;
ordinary execution does not download it.

The coordinator snapshots and verifies the tar, then builds a fresh invocation
root containing `/inputs/recipe-worker`, the staged inputs, `/work/build`,
`/work/tools`, `/work/tmp`, and `/out`. Inputs and image files are read-only.
Image entries cannot replace these invocation paths or write through symlinks.
The coordinator copies files into the invocation root; it does not share mutable
hardlinks with an image cache. The root is removed after the launcher is reaped.

The `recipe` CLI stages its own executable as the worker. Library callers use an
installed `recipe` executable on PATH or explicitly select `RECIPE_WORKER_BIN`;
the caller's unrelated executable is never treated as a Recipe worker. These
host selections are coordinator inputs, not worker environment variables.
The public library defaults to Ginux as well. `RecipeEngine::backend()` returns
`Result<Backend>` because resolving the required launcher can fail. Worker
bootstrap environment variables never select host execution for public CLI or
library operations; worker dispatch uses a separate private command.

Managed readiness queries and file-producing lifecycle hooks run through the
configured supervisor, including custom outcomes, discovered context, script
failures, and v1 checkpoint acknowledgements. Recipe declares ephemeral memory
storage at `/work`, `/work/build`, `/work/tools`, `/work/tmp`, and `/out`.
A real acquire/build/install transforms file contents, and cleanup/removal can
read and delete domain files. No writable host directory is assigned. Recipe also explicitly assigns
Ginux's virtual `/dev/null`. Reads return EOF, writes discard bytes, and metadata
identifies it as a character device. The supervisor uses its existing device
registry; no host device node is mounted or copied. Unknown image entries under
`/dev` stay hidden.

The current backend is limited to 32 objects (including directories), 64 KiB
per file and 64 open files. It supports regular-file IO and metadata, mkdir,
file unlink and chmod; directory enumeration/removal, rename, links and
execution of generated files remain unsupported. It cannot yet
build ordinary compiler projects. Anonymous pipes now use supervisor-owned
buffers, with tested byte flow, aliases and EOF. Files, devices, pipes and stdio
now share small public descriptors; aliases share file offsets and handle
lifetimes. The real descriptor limit is capped at 256 while preserving a lower
operator limit. The configured launcher now supervises up to 16 processes,
registering children before they run. Fork/vfork and the supported libc clone
forms inherit descriptor tables; owned handles survive parent/child closes,
blocking pipes wake for peer IO, and waits use virtual child IDs and observed
exit statuses. Root exit or supervisor failure kills and reaps descendants.
Image execve/execveat now preserve task identity, retire CLOEXEC descriptors only
after successful replacement, and retain every descriptor after failed exec.
The configured supervisor now tracks the supported pthread shape with shared
descriptor tables, virtual TIDs, independent syscall state and clear-TID joins.
Blocked pipe IO keeps its original handle across a peer close/reuse. Threads can
exit without closing peer handles; final process exit reaps the whole group.
A real Bash child now produces verified domain bytes and returns zero through
the existing helper. Reader startup failures remain ordinary command errors.
Child stdout/stderr, Rhai print/debug/log/warn and phase narration now reach CLI
stderr through bounded diagnostic messages. Machine JSON remains on stdout,
including when child output contains non-UTF-8 bytes. The invocation budget is
16 MiB or 32768 data messages; one notice reports truncation and excess output
continues draining so the command can finish. Supervisor audit remains separate.
Streaming helpers now work for the demonstrated Bash lifecycle. Captured-output
helpers still need owned-pipe nonblocking ioctl/readiness support in Ginux; a
real exec_output probe currently fails there.
Broader signal/robust-futex behavior, threaded exec transitions, and polling of
owned or remapped descriptors remain unfinished. Exec preparation interrupted
by signals fails explicitly; generated-file execution still needs storage.
File effects disappear after the invocation.
Acknowledgements still use placeholder generation 0 and are not durable-state
evidence. Child commands, retained writable storage, durable state, complete
dependency closure staging, target installation views and output publication
remain unfinished. A successful ephemeral install does not publish a host
artifact or provide resumable state.

## Core Model

Recipes are Rhai scripts.

They work by:

- declaring a top-level `ctx` map
- using independent `is_*` checks (explicit outcomes in API 2; deliberate throws in legacy recipes)
- mutating and returning `ctx` from phase functions
- persisting `ctx` back into the recipe file after successful phases

Normal install flow is:

1. `is_installed(ctx)`
2. `is_built(ctx)`
3. `is_acquired(ctx)`
4. `acquire(ctx)` if needed
5. `build(ctx)` if needed and defined
6. `install(ctx)`
7. `cleanup(ctx, reason)` automatically on phase success/failure, when defined

Cleanup is optional. Omit the hook when the recipe has nothing to clean. A
present hook must accept `(ctx, reason)` and return updated `ctx`.

## API 2 initialization and readiness

Opt in with a top-level `const RECIPE_API = 2;`. This declaration selects the
semantics of the entire recipe, including an inherited base. Unsupported API
versions fail before setup executes.

API 2 evaluates top-level setup once per invocation. Imports (including dynamic
import expressions), initialized globals, global constants, and inherited
functions remain available to hooks. A new worker invocation refreshes setup and
imported facts. Put observations that must refresh between phases inside checks
or explicit functions called by
hooks. Check scopes are isolated; return discovered values through `ctx`.

All three checks remain independent Rhai functions with package-specific rules:

```rhai
const RECIPE_API = 2;
let ctx = #{name: "example", product: join_path(RUNTIME.paths.output, "example.txt")};
fn is_installed(ctx) {
    if !is_file(ctx.product) { return needs_work(ctx, "product is missing"); }
    ready(ctx)
}
fn is_built(ctx) { ready(ctx) }
fn install(ctx) { mkdir(dirname(ctx.product)); write_file(ctx.product, "example"); ctx }
```

`ready(ctx)` reports readiness; `needs_work(ctx, reason)` requests the relevant
phase and carries updated context. The reason must be a nonempty string. Any
thrown error or malformed result fails execution. Phase hooks still return plain
`ctx`. Existing custom checks run again after their producer succeeds, and the
installation check also runs after success cleanup. A check that still requests
work fails that producer instead of declaring success. Missing checks request
work conservatively; a missing build hook still skips building.

Unversioned recipes retain top-level reevaluation before hooks and the legacy
`ctx`/deliberate-`throw` readiness convention. Interpreter errors and failed native
helpers fail execution. Native helper errors remain catchable with Rhai
`try/catch`, allowing an author to handle a known condition explicitly.

Source-based ctx persistence remains pending migration. Dependency lifecycles now
share the install executor; managed execution and per-dependency durable state
still require integration, as described in the dependency section below.

### Engine-owned invocation inputs

API 2 supplies `RUNTIME` before top-level setup, and attaches the same values as
`ctx.runtime` before checks/hooks. A stale `runtime` field in source is replaced
with current inputs. Hooks must preserve this namespace; returned contexts are
validated. Use the runtime paths when declaring products instead of deriving an
identity from build-directory names.

| Field | Current value |
| --- | --- |
| `runtime.host.os`, `runtime.host.arch` | Linux worker platform |
| `runtime.target.os`, `runtime.target.arch` | Native Linux default, currently equal to host |
| `runtime.paths.recipe_dir`, `base_recipe_dir` | Prepared recipe/base directories; absent base is `()` |
| `runtime.paths.build`, `tools` | Explicit directories supplied to the current operation/dependency |
| `runtime.paths.inputs`, `output` | `/inputs` and `/out` domain paths |
| `runtime.paths.temp` | Worker temporary directory |
| `runtime.operation` | `install`, `remove`, `cleanup`, or `is_*` |
| `runtime.options.jobs` | Resolved processor count |
| `runtime.options.persist_state` | Requested persistence policy; false for manual checks and current tool dependencies |

API 2 rejects conflicting defines for engine-owned paths/platform globals and
reserves the name `RUNTIME`. Unversioned define behavior is unchanged. The
coordinated R07 transport work still needs to supply explicitly selected targets,
prepared input identities, force policy, and the final domain/tool layout. This
slice exposes the native default and current explicit directories; it does not
claim target-selection transport is complete.

`ctx.state` and `ctx.outputs` default to empty maps. Recipe-defined map entries
are preserved, and checks/phases/cleanup can return updated maps. Wrong namespace
types fail with a field-specific error. `state` is for recipe runtime data and
`outputs` is for named products; fresh package facts may stay at the ctx root.
Separate durable state import/storage, strict nested-value persistence, output
contracts, and publication remain pending. The persistence option alone does not
establish resumable storage.

### Named product paths

Declare products in `ctx.outputs`, including on a successful readiness check when
installation is reused. For path products, use a nonempty absolute path string:

```rhai
ctx.outputs.rootfs = rootfs;
```

Rust consumers select required names and validate file/directory types. A missing
name is an error; consumers do not infer filenames or substitute an old build
directory. These contract checks supplement the recipe's custom readiness rules.

Worker paths such as `/out/rootfs` identify domain artifacts. The coordinator
must export selected products and return their published host paths before a
host consumer uses them. Declaring a path does not publish it or authorize access
to that host path. This publication bridge remains R08 work.

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

```

Notes:

- `build(ctx)` is optional. Omit it for prebuilt binaries.
- `is_built(ctx)` is also optional. Omit it if there is no build phase.
- `remove(ctx)` is optional, but needed if you want `recipe remove` to work.

## Required and Optional Functions

### Required for Normal `recipe install`

- `let ctx = #{ ... };`
- `install(ctx)`

In practice, most real recipes should also define:

- `is_installed(ctx)`
- `acquire(ctx)`

### Optional

- `is_acquired(ctx)`
- `is_built(ctx)`
- `build(ctx)`
- `remove(ctx)`
- `cleanup(ctx, reason)`

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

Omit this hook when the recipe needs no custom cleanup. A manual `recipe cleanup`
then returns initialized `ctx` without removing products or writing a cleanup
checkpoint. This applies to legacy recipes and API 2.

When present, the hook must accept `(ctx, reason)` and return `ctx`. Invalid
signatures fail before install or cleanup setup executes. A cleanup error after
a successful phase fails the operation. If the phase already failed, a secondary
cleanup error is reported without replacing the primary failure. Dependencies use
the same cleanup behavior.

For example, remove only a known intermediate owned by this recipe:

```rhai
fn cleanup(ctx, reason) {
    if reason == "auto.build.failure" { rm(join_path(BUILD_DIR, "build.tmp")); }
    ctx
}
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

### Present for CLI and RecipeEngine Operations

Install, manual checks, cleanup, and removal use the same compatibility globals.
Dependency recipes use these globals with their own work directory and the
parent's shared tools destination.

- `RECIPE_DIR`: parent directory of the recipe file
- `BUILD_DIR`: build/work directory for this run
- `ARCH`: host architecture from Rust `std::env::consts::ARCH`
- `NPROC`: CPU count
- `RPM_PATH`: current `RPM_PATH` environment value, or empty string
- `TOOLS_PREFIX`: the prepared `/work/tools` tree, shared by the main recipe
  and its dependencies

### Present Only in Some Contexts

- `BASE_RECIPE_DIR`: parent directory of the base recipe when using
  `//! extends: ...`

Built-in `print` and `debug` write to stderr, including when called by imported
modules. They do not prefix machine-readable JSON on stdout.

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
- process helpers: `exec_checked`, `exec`, `exec_output`
- file I/O helpers: `read_file`, `read_file_or_empty`, `write_file`,
  `append_file`, `glob_list`
- filesystem helpers: `exists`, `file_exists`, `is_file`, `dir_exists`,
  `is_dir`, `mkdir`, `rm`, `mv`, `ln`, `chmod`
- network/acquire helpers: `download`, `verify_sha256`, `verify_sha512`,
  `verify_blake3`, `fetch_sha256`, `http_get`, `git_clone`,
  `git_clone_depth`, `git_checkout`, `torrent`, `download_with_resume`
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
exec_checked("strip", ["--strip-unneeded", binary_path]);
exec_checked("make", ["-j", NPROC], #{
    cwd: ctx.source_path,
    env: #{PREFIX: ctx.prefix}
});
```

Arguments are passed directly without shell expansion. `exec_checked` throws
on a nonzero exit; `exec` returns the exit code for explicit handling.
`exec_output` returns stdout and throws on failure. All three accept an optional
third argument with `cwd` (a nonempty string) and `env` (a map of string values).
Environment overrides apply only to that child command.

Streaming command output and captured-command stderr go to Recipe stderr so
machine output stays intact. Child stdin is closed; it cannot consume worker
control input. Use `shell*` for pipelines or redirects; shell strings still
require appropriate quoting.

### Helper Footguns Worth Knowing

- Prefer `download(url, dest, ctx.sha256)` when the recipe has a pinned SHA256.
  The optional digest is checked before replacement, so corrupt responses cannot
  replace a valid cached file. `download_with_resume(url, dest, ctx.sha256)` and
  `github_download_release(repo, pattern, dest_dir, ctx.sha256)` support the same
  verification. Calls without the digest still require separate verification.
- `download_with_resume(url, dest)` treats an existing file as a prefix and
  requires an exact HTTP Range response. It stages a complete replacement;
  failures preserve the existing file and discard the attempt's new bytes.
  Verify the result's declared digest even if the server reports a matching size.
- `git_clone(url, dest_dir)` clones into `dest_dir/<repo-name>`, not exactly to
  `dest_dir`
- `git_clone_depth(url, dest_dir, depth)` behaves the same way
- `git_checkout(url, dest, commit)` uses the exact destination and a full
  40- or 64-character commit ID. It checks origin, rejects tracked/untracked
  local changes, and selects detached HEAD. A missing commit is fetched from
  the declared origin; an already matching checkout can be reused offline.
  The clone helpers also check origin, and never delete a damaged destination.
  They preserve the existing revision and local edits when reusing a clone.
  HTTP(S), SSH, and explicit `file://` origins are supported. Fresh clones are
  staged beside the destination and published after successful acquisition.
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
- dependency recipes are found as `<name>.rhai`, beside the requesting recipe
  first and then under `--recipes-path`; names cannot escape through `..`
- all transitive dependencies share the invocation's `/work/tools` tree
- tool paths are prepended as `/work/tools/{usr/bin,usr/sbin,bin,sbin}`

That means your main recipe normally just calls the tool by name:

```rhai
fn build(ctx) {
    shell_in(ctx.src_dir, "meson setup build");
    shell_in(ctx.src_dir, "ninja -C build");
    ctx
}
```

### How Dependency Recipes Run

Dependency recipes use the same install executor, readiness rules, build hook,
cleanup handling, and retry policy as their parent. They can declare their own
`deps` and `build_deps`. Successful checks carry discovered context forward.
Installed tools are checked again when another graph branch needs them; the
resolver does not replace custom readiness with a remembered success flag.

Each dependency gets a separate work directory under the invocation's
`BUILD_DIR/.deps/<recipe-stem>-<path-hash>`. This hash disambiguates source paths;
it is not durable build identity. `TOOLS_PREFIX` and API 2's runtime tools path
refer to the shared tools destination, including for transitive dependencies.
Cycles are checked by canonical source path before taking another lock or
evaluating that recipe. Dependency depth is bounded to 64 active recipes.

The current v1 worker protocol cannot identify separate dependency snapshots.
Use `--no-persist-ctx` for explicitly non-resumable dependency execution. Persistent
dependency execution fails before dependency setup until the R07 state contract
is integrated. Dependency sources are never rewritten, and dependency contexts
are never sent as the parent's v1 checkpoint. In non-persistent mode, parent
checkpoints still report normally; dependency phases have no durable state/ack
association yet. This common executor code is compiled, but managed dependency
execution, closure staging, and durable recovery still require Ginux integration.

### Environment Fixups for Tool Recipes

When dependency tools are installed, the resolver may also export:

- `BISON_PKGDATADIR`
- `M4`
- `LIBRARY_PATH`
- `C_INCLUDE_PATH`
- `CPLUS_INCLUDE_PATH`
- `PKG_CONFIG_PATH`

Those are only set if the corresponding path exists under `/work/tools/usr`.

## `extends` Behavior

Recipes can inherit from a base recipe with a leading comment:

```rhai
//! extends: base.rhai
```

Rules:

- it must appear in the leading comment block before the first non-comment line
- base recipe is compiled first
- child AST is merged on top
- child functions with the same name and arity override base functions
- top-level statements run base first, then child
- recursive extends are rejected

The repository's source-kernel lifecycle lives in
`distro-builder/recipes/linux.rhai`; its shared metadata and path/release helpers
live in `recipes/kernel/metadata.rhai`. `linux-base.rhai` is a deprecated entry
point that inherits the canonical lifecycle and artifact layout. Custom kernel
recipes should extend `linux.rhai` directly, since extending the alias would
require recursive inheritance. The unused `leviso/deps/linux.rhai` overlay was
removed; its Rust caller already delegates to the canonical recipe.

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

### Malformed cleanup hook

Cleanup may be omitted. If present, its signature must be
`fn cleanup(ctx, reason) { ...; ctx }`. A one-argument cleanup hook is an error;
it is not silently treated as an absent hook.

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
- any defined cleanup hook accepts `(ctx, reason)` and returns `ctx`
- `build(ctx)` is only present if the recipe really has a build phase
- `is_built(ctx)` only passes when build can truly be skipped
- destination paths are explicit and safe
- helper usage matches `HELPERS_AUDIT.md`, not wishful spec helpers

## Related Documents

- `README.md`: current CLI, bootstrap, and runtime reality
- `HELPERS_AUDIT.md`: authoritative current helper list
- `PHASES.md`: lifecycle overview
- `REQUIREMENTS.md`: target design, broader than current implementation
