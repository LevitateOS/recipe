# Recipe

Recipe executes Rhai package recipes inside a configured Ginux domain. The
coordinator imports explicit immutable inputs, retains matching state/filesystem
checkpoints and publishes the recipe's named outputs. Recipe code and payload
commands do not run on the coordinator host.

The current contract is `const RECIPE_API = 2;`. Recipes define package facts,
custom readiness and acquire/build/install behavior. Callers select inputs,
resource limits, endpoint permissions and the outputs they consume.

## Build and prepare the environment

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

`GINUX_ROOTFS` must match the digest in `GINUX_ROOT`. If omitted, the image tar is
looked up at `$XDG_CACHE_HOME/recipe/images/<digest>.tar`. The worker is staged
separately; changing Recipe does not require rebuilding an unchanged image.
The current native launcher requires Linux x86_64 and privileges for its chroot
bootstrap. Run the configured coordinator/launcher with those privileges; a
plain `sudo` wrapper around only the launcher closes required inherited channels.
Unavailable launch capabilities fail explicitly.

Only install commands required by a recipe in its compiler image. For example,
a Git recipe needs Git there. An explicit Rust musl target needs its matching
standard library/linker support. GNU and musl outputs are distinct products.
The demonstrated native GNU Cargo invocation uses one job,
`LD_LIBRARY_PATH=/usr/lib`, and `RUSTFLAGS=-C link-arg=-Wl,--threads=1`.

## Run a recipe

```sh
recipe install ./producer.rhai \
  --build-dir .artifacts/work/example \
  --state-dir .artifacts/state/example \
  --input source=/absolute/path/to/offline-source \
  --json-output .artifacts/example-result.json
recipe isinstalled ./producer.rhai --build-dir .artifacts/work/example \
  --state-dir .artifacts/state/example --input source=/absolute/path/to/offline-source
```

Pass the same recipe, inputs and destination for retained lifecycle operations.
Use the returned `ctx.outputs` paths; they identify published host files or
directories. Paths in recipe hooks refer to the domain filesystem instead.
Logs and tool output go to stderr; stdout or `--json-output` contains final JSON.

For the repository's actual offline live-tool producer:

```sh
distro-builder artifact prepare-tool-source recfstab .artifacts/recfstab-source
recipe install distro-builder/recipes/recfstab.rhai \
  --input source=.artifacts/recfstab-source \
  --build-dir .artifacts/work/recfstab --json-output .artifacts/recfstab.json
```

Source preparation vendors only the selected production closure, offline, with
versions from the workspace lock. The managed recipe performs compilation.
Canonical product preparation calls this same producer. `just tools-install`
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
  required for publication. Failed work cannot replace earlier publications.
- Dependencies use the same lifecycle and shared `/work/tools`. They are not
  separate host installers.
- `list` and `info` inspect loader-owned declared metadata without running setup
  or helper code. Dynamic metadata is reported unavailable.

## Inputs and selected targets

`--input NAME=PATH` imports a file or directory read-only at `/inputs/NAME`.
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
A corrupt current record/snapshot fails before workload startup; retain it for
diagnosis and choose a new state directory to start explicitly fresh.

`--no-persist-state` disables state/files recovery; it does not disable managed
execution or publication. Retention keeps the current and three recent input
identities per destination/recipe scope, with current and previous complete
filesystem generations. Pruning runs under the destination lock. Published
products are not garbage-collected as state snapshots.

Diagnostics live in `<state-root>/logs/<invocation-id>/`, outside invocation
scratch: `worker.log`, `supervisor-stderr.log` and `result.json`. Logs are bounded;
active leases protect running invocations, and up to 32 recent completed
invocations are retained. The coordinator prints the diagnostic directory.
Private leased scratch is reclaimed after abandoned invocations; arbitrary
workspace directories are never cleanup candidates.

SIGINT/SIGTERM cancel execution and await descendant teardown. Cancellation and
deadline failure preserve the last complete checkpoint and do not publish failed
work. `--timeout-secs` bounds the whole execution, including legitimate silent
commands (default 3600, range 1–86400). Transport partial-frame waits are bounded
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
| `--memory-mib N` | Domain memory bound, default 2048 MiB. |
| `--storage-max-objects N` | Writable object bound, default 16384. |
| `--storage-max-bytes N` | Explicit writable data cap within the memory bound. |
| `--network-host HOST:PORT` | Explicit outbound TCP authority, repeatable, at most eight. Networking otherwise stays off. |
| `--network-max-bytes N` | Cumulative network send/receive cap, default 8 GiB. |
| `--json-output FILE` | Final context result, separate from stderr diagnostics. |

Storage accounts hardlink data once. Inputs, restored snapshots and publications
are bounded and validated; increasing a bound does not add a missing syscall or
compiler capability. Network grants resolve/pin numeric TCP endpoints before
launch, install the corresponding hostname map and mediate readiness/teardown.
They do not isolate virtual hosts sharing an allowed IP and port. There is no
UDP, proxy, listening service or native host-network fallback.

`download(url, dest, sha256[, resume])` verifies staged bytes before replacing a
valid destination. `git_checkout(url, exact_dest, full_commit)` requires a full
40/64-hex commit. Redirect destinations also require explicit endpoint grants.
Source recipes own version/URL/digest facts; Rust/TOML declares permissions.

## Optional coordinator repair

Normal execution needs no AI provider. `recipe install --autofix` explicitly
opts into bounded coordinator-side patch/retry after an eligible build/install
failure. Provider configuration comes from XDG `recipe/llm.toml`; select a profile
with `--llm-profile`. The provider process is outside the worker domain.

Use `--autofix-attempts` (1–8), `--autofix-cwd`, `--autofix-prompt-file` and repeated
`--autofix-allow-path` to bound repair. Patches cannot alter readiness checks,
change disallowed paths or hide command failures. Changed recipe bytes create a
new invocation identity; old state is not restored across a patch. Recipe-side
LLM helpers are unavailable inside the worker.

## Validation and limits

Run focused real CLI/integration cases with the actual launcher and prepared
image. Build a worker snapshot before a run and keep it unchanged for that run:

```sh
cargo test --offline -p levitate-recipe --test e2e --no-run
RECIPE_TEST_BIN=/absolute/path/to/recipe cargo test --offline \
  -p levitate-recipe --test e2e managed_target:: -- --test-threads=1
```

The checkout's workspace and configured launch privileges are required. See the
superproject implementation checkpoint for exact tested revisions and remaining
production/VM evidence. Native tool lifecycle, custom-rootfs, selected-target,
checkpoint/link preservation and mediated acquisition are separately exercised;
none implies general Linux syscall or every distro package support.

License: MIT OR Apache-2.0.
