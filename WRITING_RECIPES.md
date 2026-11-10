# Writing Recipe recipes

A recipe owns its package facts, lifecycle and literal NEEDS authority. Callers
bind declared immutable inputs, may narrow grants/limits, and select the managed
environment. Every executable recipe declares `const RECIPE_API = 2;` and its
own `const NEEDS = #{...};`, including children that extend another recipe.
OS artifact producers and user-managed software use this same format and engine.
In the LevitateOS workspace they live in `distro/builder/os-recipes/` and
`recipes/`, respectively. Their consumer owns deployment: the builder composes
OS images, while the CLI activates user package outputs. The `install` hook below
populates the recipe's owned output tree; activation happens after publication.

Caller restrictions must be valid lowerings/subsets for the named recipe. They
also apply across the dependency graph: each dependency's numeric limits take
the stricter bound, endpoint sets intersect, and no node acquires another's
grants. Resource and byte limits remain per instance.

Read [runtime limits](docs/runtime-limits.md) when choosing grants: memory is
per-process address space, CPU selection is real affinity, network authority is
numeric IP:port, global/vDSO clocks remain visible and archive timestamps have
whole-second precision. The selected image must support the actual consumer;
neither a helper signature nor a granted catalog operation proves that it does.

## Runnable local example

Save this as `hello.rhai`. With a configured Ginux launcher/image from the
[README](README.md), `recipe install ./hello.rhai --build-dir ./hello-work`
produces a named program without downloading or compiling anything on the host.

```rhai
const RECIPE_API = 2;
const NEEDS = #{ outputs: #{ program: "file" } };
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

## A package for the active profile

Use the existing directory output contract to opt into CLI activation:

```rhai
const RECIPE_API = 2;
const NEEDS = #{ outputs: #{ "package": "directory" } };
// ctx must also declare a name and version.
// install(ctx) writes package_root/bin/COMMAND and/or package_root/share/PATH.
// is_installed(ctx) checks those files before publishing the same directory.
// Both return the updated ctx with:
// ctx.outputs["package"] = package_root;
```

`package` is a Rhai keyword, so quote the output key. The files must be regular
files; commands need executable permission. The initial activation surface is
`bin/COMMAND` and `share/PATH`; symlinks, special files and setuid/setgid permissions
are rejected. There are no host installation hooks. A package may also hold
`lib/`, which stays in the package and is linked nowhere: it is where a program
carries the shared libraries it loads (`carry_libraries`, a trial of the
self-contained packages design in the LevitateOS superproject). See the workspace's
`recipes/ripgrep.rhai` for a complete pinned package producer.

`recipe install` activates this output atomically unless `--no-activate` is set.
`recipe update NAME` consumes the selected recipe's current pin. `recipe remove
NAME` deactivates a recorded package without running its recipe; its `remove(ctx)`
hook remains available for producer cleanup with `--no-activate`. The library
continues to publish outputs only. OS builders pass `--no-activate` explicitly.

## Runtime, package facts and resumable state

Setup runs once per invocation inside Ginux. It can use immutable `RUNTIME` to
initialize recipe fields. Hooks receive the same engine-owned `ctx.runtime`.
Never save runtime paths or package versions in `ctx.state` to override current
setup on a later invocation.

| Field | Contract |
| --- | --- |
| `runtime.paths.recipe_dir` | Staged directory of the selected recipe. |
| `runtime.paths.base_recipe_dir` | Staged base directory when extending a recipe; empty otherwise. |
| `runtime.paths.inputs` | `/inputs`, containing explicitly imported inputs. |
| `runtime.paths.build` | `/work/build`, owned build/acquisition work. |
| `runtime.paths.tools` | `/work/tools`, a private copy of declared dependency tools. |
| `runtime.paths.dependencies` | Immutable direct dependency products at `/inputs/recipe-dependencies/DEP/OUTPUT`. |
| `runtime.paths.temp` | `/work/tmp`, owned scratch. |
| `runtime.paths.output` | `/out`, selectable product files/directories. |
| `runtime.paths.target` | `/out`, the owned installed-target view. |
| `runtime.target.os`, `.arch` | Explicit supported execution target. |
| `runtime.host.os`, `.arch` | The worker's own platform. |
| `runtime.operation` | The running operation (`install`, `remove`, `cleanup`, `is_installed`, `is_built`, `is_acquired`). |
| `runtime.options.jobs` | Requested job count. |
| `runtime.options.force_acquire`, `.force_build`, `.force_install` | Current force selection for the main recipe. |
| `runtime.options.persist_state` | Whether recovery is enabled. |

After worker completion, the coordinator adds `runtime.contract` (catalog,
digest and derived rules) and `runtime.network_usage` to the returned context.
These are completion metadata in the engine-owned namespace; ordinary recipe
facts named `contract` or `network_usage` stay intact. Each dependency has its own
retained invocation receipt. Dependency products obey the same import bounds,
including the `DEP/OUTPUT` path prefix. Dependency aliases must identify one
recipe when binding inputs and cannot descend into another dependency's product
namespace.

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
and return nonzero when it reports work needed. With dependencies, they first
require matching cached dependency products and run only their installed checks,
in dependency order. They never run dependency acquire/build/install hooks to
repair a cache miss. Explicit `remove`/`cleanup` uses the same cached-dependency
rule to recover the parent's exact saved state/files. A missing or changed cache
fails before the main hook; dependencies are never installed or removed by the
parent's operation. See [the runtime limits](docs/runtime-limits.md).

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
contains absolute host publication paths; hooks keep using domain paths. Products
retain coordinator ownership/private export-directory permissions. Privileged
producers require matching consumer privileges or explicit product installation;
a returned path or selection symlink grants no additional access.

`NEEDS.outputs` declares the exact names and file/directory kinds. Recipe checks
the completed installation and cache reuse against that declaration before
publication. Intermediate phases may expose a subset; every present output is
validated before checkpointing/publication and must identify its declared kind. Consumers also
validate the products they use. Never guess a sibling
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
origin and selected tree. Git URLs must identify the direct repository; Git
redirects are disabled. Cached repositories with includes, URL rewrites,
TLS/proxy overrides, filters or duplicate origins are rejected without changing
the cache. Hooks, fsmonitor, replacement objects and submodule recursion are
disabled. Local repositories must be explicit imported inputs; remote transport
uses the invocation's mediated network grants.

Declare grants as literal data, without helper calls, variables or computed maps:

```rhai
const NEEDS = #{
    inputs: #{ source: "directory", config: "file?" },
    outputs: #{ program: "file", rootfs: "directory" },
    network: ["publisher.example:443"],
    memory_mib: 4096,
    storage_max_objects: 32768,
    timeout_secs: 3600,
    network_max_bytes: 8589934592,
};
```

An absent field means no inputs/outputs/network, or the existing default limit.
Optional input kinds end in `?`; absence is allowed, a supplied path must still
have that kind. Every endpoint, including HTTP redirect destinations, belongs
in NEEDS. `--network-host` selects a subset and resource flags select lower
limits; widening fails with the declaration field to edit. Network byte bounds
and teardown apply to helpers and subprocess clients alike. The configured
contract pins configured downstream catalog 1002 (separate from upstream Gate 11
catalog 2), grants outbound connect only for nonempty effective
network, grants no listener and denies direct MMIO/DMA/raw packets. It still
relies on Ginux's path, descriptor, resource and numeric endpoint owners.

Dependencies declare their own NEEDS and run in separate instances. Bind their
inputs explicitly as `DEP::NAME`; no parent's input or grant is inherited. A
`tools: "directory"` output contributes to the consumer's private tools prefix.
Other declared products are available at `RUNTIME.paths.dependencies/DEP/OUTPUT`.
Metadata inspection does not execute setup; invocation explanation uses retained
authority rather than today's source. Unused endpoint warnings rely on a complete
bounded network summary, never on absent syscall-log lines.

## Commands and extraction

`exec_checked(program, args[, options])` throws on nonzero exit;
`exec_output(...)` returns checked stdout; `exec(...)` returns a status that the
recipe handles deliberately. Arguments are an array. An optional map supports
child-only `cwd`, `env`, and integer `uid`/`gid` options, without mutating the worker's global environment:

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

Each dependency runs in its own Ginux instance under its own NEEDS and matching
state/filesystem identity. Caller restrictions narrow every instance across the
graph; the parent never lends a grant. Validated direct dependency products are
snapshotted into the consumer at `runtime.paths.dependencies`. A directory output
named `tools` also populates that consumer's private `runtime.paths.tools`, whose
executables are prepended to worker PATH. Custom dependency readiness remains
active. Do not depend on host build-directory conventions or paths in another
instance.

A child can start with `//! extends: base.rhai`. The loader compiles/merges the
base first; child functions of the same name/arity override it. Setup statements
run base-first. Only one level is supported: a base that itself extends fails.
Literal imports preserve module paths. Cycles and dynamic imports/dependency
declarations fail preparation. Pure facts modules need no
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

A recipe may also say what a front end should show about it, in a literal
`const ABOUT = #{...};` beside NEEDS: `kind`, `category`, `details`, `homepage`,
`license`, `adds`, per-hook `steps` notes and `keeps` (the superproject's
`docs/recipe-front-end-format.md` has the fields). Like NEEDS it holds no helper
calls or variables. `recipe list --json` passes it through as written; it is the
author's claim and is not enforced. `recipes/ripgrep.rhai` has one.

Compile syntax before runtime, then exercise the real CLI with representative
inputs, reuse, changed input, remove and failure paths. A successful compile is
source evidence; a managed run proves only its observed image/recipe/workload.
Do not rebuild kernels just to validate caller or prepared-bundle migration.

Kernel producers default to the LevitateOS pin; builders select Acorn explicitly
with `KERNEL_TARGET=acorn`. Source kernel builds require the selected compiler
image to contain their build tools. The historical linux-deps DNF fallback has
no declared mirror grants and is unsupported in this release; a parent kernel
publisher grant is never borrowed for it. Kernel rebuilds are excluded from
release acceptance, as recorded by PRD19. This limitation is separate from the
prebuilt kernel bundle path.
