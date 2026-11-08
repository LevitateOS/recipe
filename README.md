# Recipe

Recipe executes Rhai package recipes inside a configured Ginux domain. The
coordinator imports explicit immutable inputs, retains matching state/filesystem
checkpoints and publishes the recipe's named outputs. Recipe code and payload
commands do not run on the coordinator host.

The executable interface is API 2 with a literal `const NEEDS = #{...};`.
Recipes own their typed inputs/outputs, endpoint grants and resource limits.
Callers bind declared inputs and may explicitly narrow authority. A caller cannot
add an input, endpoint or larger limit that the named recipe did not declare.
Restrictions must first be valid lowerings/subsets for the named recipe. They
also cover every dependency: limits take the stricter bound, and endpoint sets
intersect each dependency's own declaration. Limits remain per instance.

The launcher must support configured downstream catalog 1002, `configured-network-audit=1`, `configured-denial-audit=1`,
configured `launch`, export, control and restore. Recipe derives a self-contained
`[userspace.contract]` and validates it before launch; a generic `config` parse
alone does not establish compatibility. The LevitateOS workspace's old
`tools/ginux` pin lacks this runtime. Maintained runtime delivery remains open in
PRD01; local NEEDS verification uses the exact candidates recorded in the
superproject's `docs/ginux-contributions/prd18-implementation.md`.

## Build and prepare the environment

[Linux setup and bootstrap](docs/setup-linux.md) records the verified host,
input reproduction, package roles, required privileges and output ownership.
Maintained runtime source delivery remains an explicit prerequisite.

From the LevitateOS workspace:

```sh
cargo build --offline -p levitate-recipe --bin recipe --bin recipe-image
```

Prepare a compiler image explicitly from pinned, already acquired inputs. The
[bootstrap guide](bootstrap/README.md) describes the manifest and provenance:

```sh
target/debug/recipe-image \
  --manifest tools/recipe/bootstrap/ginux-build-image.json \
  --input-root .artifacts/ginux-env \
  --output .artifacts/ginux-env/prepared-image
```

The output directory must be new. Configure the actual built Ginux launcher and
verified image before executing recipes:

```sh
export GINUX_BIN=/absolute/path/to/ginux
export GINUX_ROOTFS=/absolute/path/to/prepared-image/rootfs.tar
export GINUX_ROOT=oci:compiler@sha256:IMAGE_SHA256_FROM_PROVENANCE
export RECIPE_WORKER_BIN=/absolute/path/to/recipe
```

`GINUX_ROOTFS` must match the digest in `GINUX_ROOT`. Without `GINUX_ROOT`, the
pinned bare Rocky base (no compiler) is selected. If `GINUX_ROOTFS` is omitted, the image tar is
looked up at `$XDG_CACHE_HOME/recipe/images/<digest>.tar`. The worker is staged
separately; changing Recipe does not require rebuilding an unchanged image.
The current native launcher requires Linux x86_64 and privileges for its chroot
bootstrap. Run the configured coordinator/launcher with those privileges; a
plain `sudo` wrapper around only the launcher closes required inherited channels.
Unavailable launch capabilities fail explicitly.
The invocation examples below assume those privileges are available. When using
sudo, pass the declared image/worker selections to the whole coordinator command:

```sh
sudo env GINUX_BIN="$GINUX_BIN" GINUX_ROOTFS="$GINUX_ROOTFS" \
  GINUX_ROOT="$GINUX_ROOT" RECIPE_WORKER_BIN="$RECIPE_WORKER_BIN" \
  "$RECIPE_WORKER_BIN" install tools/recipe/examples/hello.rhai --build-dir ./hello-work
```


Only install commands required by a recipe in its compiler image. For example,
a Git recipe needs Git there. An explicit Rust musl target needs its matching
standard library/linker support. GNU and musl outputs are distinct products.
The verified GNU and musl Cargo fixtures use one job per client and the local
Ginux integration's 32-task bound. GNU uses its default linker configuration;
musl selects the compiler image's `rust-lld` as described in
[the bootstrap instructions](bootstrap/README.md). Both fixtures compile once
across two cache clients and execute the resulting threaded program.

## Run a recipe

```sh
recipe install tools/recipe/examples/hello.rhai \
  --build-dir .artifacts/work/example \
  --state-dir .artifacts/state/example \
  --json-output .artifacts/example-result.json
recipe isinstalled tools/recipe/examples/hello.rhai --build-dir .artifacts/work/example \
  --state-dir .artifacts/state/example
```

Pass the same recipe, inputs and destination for retained lifecycle operations.
Use the returned `ctx.outputs` paths; they identify published host files or
directories. Paths in recipe hooks refer to the domain filesystem instead.
Logs and tool output go to stderr; stdout or `--json-output` contains final JSON.
Published paths and retained state keep the coordinator's ownership and private
export-directory permissions. A privileged producer needs an appropriately
privileged consumer, or explicit installation of selected product files with
the desired ownership. Selecting a publication with a symlink does not grant
another user access. No automatic privilege bridge is provided.


For the repository's actual offline live-tool producer:

```sh
cargo run --locked -p distro-builder --bin distro-builder -- \
  --repo-root "$PWD" artifact prepare-tool-source recstrap .artifacts/recstrap-source
recipe install distro/builder/recipes/install-tools.rhai --define BIN=recfstab \
  --input source=.artifacts/recstrap-source \
  --build-dir .artifacts/work/recfstab --json-output .artifacts/recfstab.json
```

Source preparation vendors only the selected production closure, offline, with
versions from the workspace lock. The managed recipe performs compilation.
`recfstab` is a binary of the `recstrap` package, so source preparation selects
`recstrap` while `BIN` selects `recfstab`.
Canonical product preparation calls this same producer. `cargo xtask setup`
explicitly prepares the separate QEMU/OVMF product; scenario commands consume
existing prepared artifacts.

## Recipe interface

[WRITING_RECIPES.md](WRITING_RECIPES.md) contains a runnable recipe and lifecycle
details. [HELPERS_AUDIT.md](HELPERS_AUDIT.md) lists registered helpers.

- Setup reads immutable `RUNTIME`; hooks receive the same data as `ctx.runtime`.
- Checks return `ready(ctx)` or `needs_work(ctx, "concrete reason")`. Check errors
  stop execution. Phase hooks return an updated context map.
- Package/build facts remain recipe fields. Only `ctx.state` is restored between
  invocations; setup runs again and current facts remain current.
- `ctx.outputs` names files/directories under owned `/out` or `/work`. Successful
  execution, validated archive data and acknowledged matching checkpoints are
  required for publication. Install/installed reuse require the complete declared
  set; intermediate phases allow typed subsets checked before checkpointing.
  Failed work cannot replace earlier publications.
- Dependencies run that lifecycle in separate Ginux instances with their own
  NEEDS. Declared products are snapshotted into the consumer; a directory output
  named `tools` contributes to its `/work/tools`. No parent grant is lent. Manual
  checks, remove and cleanup operate on the named recipe without preparing deps.
- `list` and `info` inspect loader-owned declared metadata without running setup
  or helper code. Dynamic metadata is reported unavailable.

## Inputs and selected targets

`NEEDS.inputs` maps names to `file`, `directory`, `file?` or `directory?`.
`--input NAME=PATH` binds one declared input read-only at `/inputs/NAME`; missing
required, undeclared, duplicate or wrong-kind inputs fail before setup. Use
`--input DEP::NAME=PATH` for a dependency: inputs are never implicitly inherited.
A host path in a define grants no filesystem access. Root `.git` and `target`
directories are omitted from ordinary source imports. Input bytes, modes, link
identity and link targets participate in state identity. Confined relative links
and hardlinks are preserved. Named `rootfs`/`target` trees may contain absolute
rootfs symlink bytes; they never authorize following host paths.

`--input target=PATH` seeds the owned target `/out` for a fresh identity. With
retained state, Recipe restores that target instead. Hooks inspect
`ctx.runtime.paths.target`; compiler-image files under `/usr` cannot establish
that a selected product is installed. Removal changes the owned target, leaving
the imported input and earlier published generations unchanged. There is no
live-system activation command.

## State, diagnostics and recovery

`--build-dir` selects a stable logical destination and its shared lock. Domain
work remains `/work/build`. The default destination is under the selected state
root, which defaults to `$XDG_STATE_HOME/recipe` (normally `~/.local/state/recipe`).
Use `--state-dir PATH` to choose a different state root.

Identity includes the recipe/module/dependency closure, declared input content,
custom defines, execution options, image and worker. Changed inputs start fresh
state. Checkpoints become recoverable only after both files and separate
`ctx.state` records are durable. Interrupted, unacknowledged work is discarded.
A successful failure-cleanup hook is also checkpointed with its returned state
and matching files, while the producer operation still fails and publishes
nothing. Cleanup/checkpoint diagnostics preserve the original producer failure.
A corrupt current record/snapshot fails before workload startup; retain it for
diagnosis and choose a new state directory to start explicitly fresh.

`--no-persist-state` disables state/files recovery; it does not disable managed
execution or publication. Retention keeps the current and three recent input
identities per destination/recipe scope, with current and previous complete
filesystem generations. Pruning runs under the destination lock. Published
products are not garbage-collected as state snapshots.

Diagnostics live in `<state-root>/logs/<invocation-id>/`, outside invocation
scratch: `worker.log`, `supervisor-stderr.log`, `authority.json` and `result.json`.
The authority receipt saves the actual declaration, effective constraints, pinned
endpoints, rendered config and catalog/digest used for this instance. Logs are bounded;
active leases protect running invocations, and up to 32 recent completed
invocations are retained. The coordinator prints the diagnostic directory.
Private leased scratch is reclaimed after abandoned invocations; arbitrary
workspace directories are never cleanup candidates.

SIGINT/SIGTERM cancel execution and await descendant teardown. Cancellation and
deadline failure preserve the last complete checkpoint and do not publish failed
work. `NEEDS.timeout_secs` bounds each recipe instance, including legitimate silent
commands (default 3600, range 1–86400); `--timeout-secs` can only lower it. Transport partial-frame waits are bounded
separately by `RECIPE_FRAME_TIMEOUT_SECS` (default 300, range 1–86400).

## Options

Run `recipe --help` and `recipe install --help` for the complete current syntax.

| Option | Meaning |
| --- | --- |
| `-r, --recipes-path DIR` | Name/module/dependency search directory; defaults to `$RECIPE_PATH` or the per-user recipe data directory. |
| `-b, --build-dir DIR` | Logical destination identity and lock. |
| `--input NAME=PATH` | Explicit immutable file/tree input. |
| `--define KEY=VALUE` | Recipe-specific constant; not a runtime path substitute. |
| `--target-os linux --target-arch x86_64` | Currently supported native execution platform. |
| `-j, --jobs N` | Requested jobs, 1–16, default 1; Ginux task limits still apply. |
| `--force-install` | Rerun installation for the main recipe. |
| `--force-build` | Rerun build and installation for the main recipe. |
| `--force-acquire` | Rerun acquisition, build and installation for the main recipe. |
| `--memory-mib N` | Lower `NEEDS.memory_mib` (default declaration 2048 MiB). |
| `--storage-max-objects N` | Lower `NEEDS.storage_max_objects` (default declaration 16384). |
| `--storage-max-bytes N` | Lower the declared writable-data cap within effective memory. |
| `--network-host HOST:PORT` | Select a subset of `NEEDS.network`, repeatable; `--no-network` selects none. |
| `--network-max-bytes N` | Lower `NEEDS.network_max_bytes` (default declaration 8 GiB). |
| `--json-output FILE` | Final context result, separate from stderr diagnostics. |

Storage accounts hardlink data once. Inputs, restored snapshots and publications
are bounded and validated; increasing a bound does not add a missing syscall or
compiler capability. Network grants resolve/pin numeric TCP endpoints before
launch, install the corresponding hostname map and mediate readiness/teardown.
They do not isolate virtual hosts sharing an allowed IP and port. There is no
UDP, proxy, listening service or native host-network fallback.

`download(url, dest, sha256[, resume])` verifies staged bytes before replacing a
valid destination. `git_checkout(url, exact_dest, full_commit)` requires a full
40/64-hex commit and a direct repository URL: Git redirects are disabled, and
cached repository settings that override transport or execute hooks/filters are
rejected. HTTP download redirect destinations need explicit endpoint grants.
Recipes own version/URL/digest facts and literal NEEDS authority. Variant TOML
keeps recipe selection and input path bindings. The Fedora rootfs/preseed declare
8192 MiB; Alpine declares 4096 MiB. Fedora rootfs declares 196608 objects and
Alpine rootfs declares 131072; use `inspect` on the selected recipe for its limits.
`memory_mib` bounds each process's virtual address space, not aggregate tree RAM;
storage also has its own byte/object accounting. Jobs select eligible real CPUs,
not a virtual topology. Numeric IP/port grants are not DNS/HTTP identities.
Configured raw clocks expose host realtime and monotonic time; vDSO clocks remain
observable. Configured `sysinfo` returns zeros, not a memory-usage account.
Bounded `poll`/`ppoll` is readiness support, without general signal interruption
or restart semantics. Archive timestamps have whole-second precision. See
[supported runtime limits](docs/runtime-limits.md) before claiming isolation or
Linux compatibility beyond these boundaries.

Manual `isacquired`, `isbuilt` and `isinstalled` run dependency installed checks
in graph order and require matching cached dependency products. They never run
dependency acquire/build/install hooks to repair missing or changed products.
Use `install` separately to prepare those dependencies. The checked snapshots
retain the parent's installed input identity and each dependency's own authority.
Explicit `remove`/`cleanup` on dependency graphs remains unresolved; this
readiness correction does not repair those operations.

## Read authority without executing setup

```sh
recipe validate tools/recipe/examples/hello.rhai
recipe inspect tools/recipe/examples/hello.rhai
recipe explain tools/recipe/examples/hello.rhai network.connect
recipe explain /absolute/state/logs/INVOCATION network.connect
recipe diff tools/recipe/examples/hello.rhai tools/recipe/examples/threaded-hello.rhai
```

These commands never run recipe setup or launch a workload. Validation also
checks dependency declarations, their input bindings and compiled contracts.
Inspection lists resolved authority and pinned contract digests; it cannot know
which endpoints a future execution will use. Diff shows added/removed/changed
typed paths, endpoints, raised/lowered limits and contract digests. Invocation
explain reads the retained receipt and available audit observations, so later
source edits do not rewrite the explanation. A missing/truncated observation is
reported as unknown. Completed contexts include `runtime.contract` and `runtime.network_usage`;
a complete bounded endpoint summary produces warnings for unused grants, while
an absent summary never labels unobserved connections unused. Diagnostics do
not become success/protocol evidence or grant additional authority.

## Optional coordinator repair

Normal execution needs no AI provider. `recipe install --autofix` explicitly
opts into bounded coordinator-side patch/retry after an eligible build/install
failure. Provider configuration comes from XDG `recipe/llm.toml`; select a profile
with `--llm-profile`. The provider process is outside the worker domain. A minimal explicit provider
configuration is:

```toml
version = 1
default_provider = "codex"

[providers.codex]
bin = "codex"
```

Only the `default_provider`'s block is required.

This describes configuration syntax, not verified external-provider support.
Release acceptance includes controlled patch/retry mechanics only; actual Codex,
Claude or other provider interoperability is outside the agreed release scope.

Use the chosen provider's installed CLI/configuration; there is no implicit
provider fallback. A named profile can override provider settings.

Use `--autofix-attempts` (1–8, default 2), `--autofix-cwd`, `--autofix-prompt-file` and repeated
`--autofix-allow-path` to bound repair. Patches cannot alter NEEDS/API/dependency authority or readiness checks,
change disallowed paths or hide command failures. Changed recipe bytes create a
new invocation identity; old state is not restored across a patch. Recipes have
no LLM helpers; the provider never runs inside the worker.

## Validation and limits

[Managed integration CI](docs/ci.md) describes launcher/image pins, runtime
privileges, catalog coverage and explicitly opted-in publication.

Run focused real CLI/integration cases with the actual launcher and prepared
image. Build a worker snapshot before a run and keep it unchanged for that run:

```sh
cargo test --offline -p levitate-recipe --test e2e --no-run
RECIPE_TEST_BIN=/absolute/path/to/recipe cargo test --offline \
  -p levitate-recipe --test e2e managed_target:: -- --test-threads=1
```

The checkout's workspace and configured launch privileges are required. See the
superproject implementation checkpoint for exact tested revisions and remaining
production/VM evidence. Real tool lifecycle, custom-rootfs, selected-target,
checkpoint/link preservation and mediated acquisition are separately exercised;
none implies general Linux syscall or every distro package support.

License: MIT OR Apache-2.0.
