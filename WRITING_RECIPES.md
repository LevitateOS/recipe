# Writing Recipe recipes

A recipe owns its package facts and lifecycle. Callers select immutable inputs,
endpoint permissions and a managed execution environment. Every executable
recipe declares `const RECIPE_API = 2;`.

## Runnable local example

Save this as `hello.rhai`. With a configured Ginux launcher/image from the
[README](README.md), `recipe install ./hello.rhai --build-dir ./hello-work`
produces a named program without downloading or compiling anything on the host.

```rhai
const RECIPE_API = 2;
let ctx = #{
    name: "hello",
    version: "1.0",
    program: join_path(RUNTIME.paths.output, "bin/hello"),
    payload: "#!/bin/sh\nprintf 'hello\\n'\n",
};

fn is_acquired(ctx) { ready(ctx) }
fn acquire(ctx) { ctx }
fn is_built(ctx) { ready(ctx) }
fn build(ctx) { ctx }

fn is_installed(ctx) {
    if !is_file(ctx.program) { return needs_work(ctx, "hello output is missing"); }
    if read_file(ctx.program) != ctx.payload {
        return needs_work(ctx, "hello output has different contents");
    }
    ctx.outputs.program = ctx.program;
    ready(ctx)
}

fn install(ctx) {
    mkdir(dirname(ctx.program));
    write_file(ctx.program, ctx.payload);
    chmod(ctx.program, 0o755);
    ctx.outputs.program = ctx.program;
    ctx
}

fn remove(ctx) {
    if is_file(ctx.program) { rm(ctx.program); }
    ctx.outputs = #{};
    ctx
}
```

The result's `outputs.program` is a published host path. `/out/bin/hello` is its
path inside hooks. Use the same recipe/destination with `isinstalled` or `remove`.
A successful remove does not delete an earlier published generation.

## Runtime, package facts and resumable state

Setup runs once per invocation inside Ginux. It can use immutable `RUNTIME` to
initialize recipe fields. Hooks receive the same engine-owned `ctx.runtime`.
Never save runtime paths or package versions in `ctx.state` to override current
setup on a later invocation.

| Field | Contract |
| --- | --- |
| `runtime.paths.recipe_dir` | Staged directory of the selected recipe. |
| `runtime.paths.base_recipe_dir` | Staged base directory when extending a recipe. |
| `runtime.paths.inputs` | `/inputs`, containing explicitly imported inputs. |
| `runtime.paths.build` | `/work/build`, owned build/acquisition work. |
| `runtime.paths.tools` | `/work/tools`, shared dependency products. |
| `runtime.paths.temp` | `/work/tmp`, owned scratch. |
| `runtime.paths.output` | `/out`, selectable product files/directories. |
| `runtime.paths.target` | `/out`, the owned installed-target view. |
| `runtime.target.os`, `.arch` | Explicit supported execution target. |
| `runtime.options.jobs` | Requested job count. |
| `runtime.options.force_acquire`, `.force_build`, `.force_install` | Current force selection for the main recipe. |
| `runtime.options.persist_state` | Whether recovery is enabled. |

Top-level `ctx.name`, `ctx.version`, source URLs/digests, target ABI choices and
other package facts are initialized from the current recipe. Put only genuinely
resumable recipe data in `ctx.state`, for example a verified internal progress
marker. Readiness must still inspect the real files.

Only `ctx.state` is serialized/restored. JSON-compatible maps, arrays, strings,
booleans, null and finite numbers are accepted. Unsupported values fail with a
field path. A matching state record and filesystem snapshot form one checkpoint;
state alone cannot establish completed work.

## Lifecycle and readiness

Install checks `is_installed` first, then determines whether build/acquisition
are needed from `is_built` and `is_acquired`. Dependencies and necessary phases
run in dependency order. Each defined check returns exactly one of:

```rhai
ready(ctx);
needs_work(ctx, "a concrete reason");
```

A check exception is an error. Do not use exceptions, booleans or ordinary context
maps to request work. Custom checks should verify relevant bytes, release, ABI,
configuration or companion products, not just a saved boolean. After successful
work the executor rechecks readiness. Rediscover `ctx.outputs` when reusing an
installation.

`acquire(ctx)`, `build(ctx)`, `install(ctx)` and `remove(ctx)` return the updated
context. Prebuilt recipes may omit build work. Missing checks indicate work is
needed; a missing required action fails instead of inventing an implementation.
Manual `isacquired`, `isbuilt` and `isinstalled` execute their actual custom check
and return nonzero when it reports work needed.

`--force-install`, `--force-build` and `--force-acquire` select progressively more
work for the main recipe. Dependencies retain their own readiness. Current force
options are available under `ctx.runtime.options` if a recipe needs to refresh
its acquisition cache explicitly.

## Immutable inputs and target trees

```sh
recipe install ./tool.rhai --input source=./offline-source
recipe isinstalled ./kernel.rhai --input target=./existing-bundle \
  --input kconfig=./kconfig --define KERNEL_TARGET=acorn
```

Inputs are read-only at `/inputs/<name>`. Use named inputs for source trees,
archives, configuration and executable inputs. Defines are genuine recipe
choices such as a target ABI; they do not expose a host path. Changes to imported
bytes/modes/links change retained-state identity.

The named `target` input seeds owned `/out` on fresh execution; retained state
restores the same owned tree. Inspect `ctx.runtime.paths.target` for installation
and removal. A compiler-image binary under `/usr` is a build tool, not evidence
that it is installed in the selected product.

Rootfs/target absolute symlink bytes are preserved without host traversal.
Ordinary source trees require confined relative links. Hardlinks, directory/file
modes and empty directories survive input/checkpoint/publication. Mutating an
owned alias affects its owned peers, not the imported input.

## Named output contracts

```rhai
ctx.outputs.program = join_path(ctx.runtime.paths.output, "bin/tool");
ctx.outputs.rootfs = join_path(ctx.runtime.paths.output, "rootfs");
```

Select actual regular files or directories under assigned `/out` or `/work`.
Choose a complete bundle once when that is the consumer contract; overlapping
selections can account duplicate published bytes. A successful phase alone is
not publication: the coordinator validates the complete archive, bounds, links
and selected paths before publishing the final generation. The returned context
contains absolute host publication paths; hooks keep using domain paths.

Consumers must validate required output names and kinds. Never guess a sibling
build path, accept stale JSON from another invocation or search for an old
host-built executable. Deleting a published product does not modify retained
owned files; a later successful invocation can publish a fresh generation.

## Verified acquisition

Source package facts belong in the recipe or an imported facts module:

```rhai
fn acquire(ctx) {
    let archive = join_path(ctx.runtime.paths.build, "source.tar.xz");
    download(ctx.url, archive, ctx.sha256);
    ctx
}
```

Use `download(url, exact_dest, sha256[, resume_bool])`. A full 64-hex SHA256 is
required. The helper stages bytes, validates status/length/hash and replaces the
destination only after success. Resume validates response identity/ranges;
truncated, malformed or wrong-digest transfers cannot replace valid bytes.
`verify_sha256`, `verify_sha512` and `verify_blake3` validate existing files.

Use `git_checkout(url, exact_dest, full_commit)` for pinned Git. The destination
is exact, the commit is a full 40/64-hex object id, and acquisition validates the
origin and selected tree. Local repositories must be explicit imported inputs;
remote transport uses the invocation's mediated network grants.

Callers grant endpoint authorities with repeated `--network-host HOST:PORT` or
`RecipeEngine::with_network_hosts`. Recipes cannot add grants. Networking stays
off by default. Every redirect destination needs a grant; choose a direct
publisher endpoint where possible. Network byte/connection bounds and teardown
apply to helpers and subprocess clients alike. Metadata helpers do not make
unverified package payloads trustworthy.

## Commands and extraction

`exec_checked(program, args[, options])` throws on nonzero exit;
`exec_output(...)` returns checked stdout; `exec(...)` returns a status that the
recipe handles deliberately. Arguments are an array. An optional map supports
child-only `cwd` and `env`, without mutating the worker's global environment:

```rhai
exec_checked("cargo", ["build", "--release", "--locked", "--offline"], #{
    cwd: "/inputs/source",
    env: #{CARGO_HOME: join_path(ctx.runtime.paths.build, "cargo")},
});
```

Commands run inside Ginux and need binaries/libraries in the selected image or
shared tools. Use `shell` only when shell syntax is necessary and quote data.
For pipelines, use a checked `bash -o pipefail -c ...` or separately checked
producer/consumer steps. Do not discard extraction failures.

`extract(archive, dest)` and `extract_with_format(archive, dest, format)` use the
archive helper. Archive extraction remains subject to domain storage ownership,
path validation and supported runtime operations. A helper existing in the
registry is not evidence that every external package's executable works.

## Dependencies and inheritance

Literal declarations are discovered before launch without evaluating Rhai:

```rhai
let deps = ["runtime-tools"];
let build_deps = ["compiler-tools"];
```

Dependency recipes use the same contract and shared `runtime.paths.tools`.
Their executables are prepended to worker PATH. Their own state is paired with
the shared filesystem generation; custom dependency readiness remains active.
Do not depend on host build-directory conventions or per-dependency publication.

A child can start with `//! extends: base.rhai`. The loader compiles/merges the
base first; child functions of the same name/arity override it. Setup statements
run base-first. Literal imports preserve module paths. Cycles and dynamic
imports/dependency declarations fail preparation. Pure facts modules need no
executable lifecycle.

## Cleanup and failure

Omit `cleanup` when there is no recipe-specific cleanup. A defined hook accepts
`cleanup(ctx, reason)` and returns updated context. Reasons are `manual` and
`auto.<phase>.<success|failure>`. Manual `recipe cleanup --reason TEXT` passes the
explicit reason. Guard cleanup of useful cache/product files by the reason;
automatic success cleanup must not erase a product needed by readiness.

A cleanup error after success fails the command. Failure cleanup errors are
reported alongside the original failure. A completed failure-cleanup hook has its
returned `ctx.state` and matching files checkpointed, so restart restores that
last complete cleanup. The producer operation still fails and publishes nothing;
cleanup/checkpoint diagnostics preserve its primary failure. Source inputs and previous publications
remain immutable; failed work is not published. `remove` modifies the owned
selected target, not the live host system.

See the [README recovery section](README.md#state-diagnostics-and-recovery) for
checkpoint/log paths, retention, cancellation and timeouts. Optional coordinator
repair is explicit and cannot rewrite readiness checks to obtain success.

## Metadata and verification

`recipe list` and `recipe info` use the loader's merged declared metadata.
Literal inherited name/version/description are available without running setup.
Dynamic expressions are unavailable metadata; malformed literals fail clearly.
No coordinator-side Rhai evaluation is used for discovery.

Compile syntax before runtime, then exercise the real CLI with representative
inputs, reuse, changed input, remove and failure paths. A successful compile is
source evidence; a managed run proves only its observed image/recipe/workload.
Do not rebuild kernels just to validate caller or prepared-bundle migration.
