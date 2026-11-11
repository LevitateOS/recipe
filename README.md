# Recipe

Recipe executes Rhai package recipes inside a configured Ginux domain. The
coordinator imports explicit immutable inputs, retains matching state/filesystem
checkpoints and publishes the recipe's named outputs. Recipe code and payload
commands do not run on the coordinator host.

## One engine, two consumers

In the LevitateOS workspace, `distro/builder/os-recipes/` holds OS artifact
producers, and the top-level `recipes/` directory is the user-managed software
catalog, starting with ripgrep. The builder selects its own collection
with `--recipes-path DIR` and `--no-activate`; normal CLI lookup uses an explicitly selected
catalog, or the user's XDG recipe directory and then the sources, of which the
image ships the official one in `/usr/share/recipe/sources`.
Both use this engine, the same Rhai API 2 and the same Ginux
execution rules. Directory placement expresses ownership, not a new recipe type
or permission boundary. This repository's `examples/` and `tests/` are engine
examples and fixtures, not the application catalog.

The OS builder assembles published products into images; `recab` handles system
image activation and A/B updates. Apps bundled in a desktop image belong to that
OS lifecycle. For user software, the CLI activates a directory output named
`package` after managed execution and publication succeed. The engine's hooks
still operate on their isolated output tree. `--no-activate` keeps product-only
behavior. `list` lists recipe declarations; `installed` lists active packages.

## System software on immutable A/B installations

Root's install/update/remove commands delegate to recab's prepared inactive
system. `--system` selects this explicitly. They leave the running root unchanged:

```sh
sudo recipe --system install ripgrep
sudo recipe --system installed
sudo recipe --system isinstalled ripgrep --prepared
sudo recab finalize
sudo recab set-next B   # A is running; use A when B is running
sudo reboot
rg --version
```

A compatible recab runtime is required in both slots. Normal package outputs
install under the candidate's `/usr/lib/recipe/profile`, with selection recorded
in `/usr/lib/recipe/system.json`. Explicit `system` directory outputs carry
root-relative intended metadata from `target_finalize`. Dependencies are produced
through the existing Recipe graph. Builds receive isolated inputs and outputs;
recab alone mounts and changes the inactive root. Updates/removals refuse
conflicting ownership and administrator edits. `recab edit PATH [--file FILE]`
applies bounded configuration edits to that same preparation. Finalization and
a successful trial boot make the changes active; package commands do not reboot.

`recipe installed` reports active and prepared receipts; `isinstalled` checks the
active selection unless `--prepared` is given. `--preparation ID --revision N`
guards stale requests. `update` resolves the current catalog first, then falls
back to the prepared package's recorded producer. `--package-root DIR` retains an
independent profile and `--no-activate` retains output-only builder execution.
Other commands, including catalog listing and validation, keep their normal routes.

## Independent user software

On a mutable system, or with an explicit independent package root (use `doas` on AcornOS):

```sh
sudo recipe install ripgrep
rg --version
sudo recipe installed
sudo recipe update ripgrep
sudo recipe remove ripgrep
```

`update` runs the current selected recipe and activates its pinned version. It
does not discover upstream releases or rewrite recipe pins. A reviewed newer
recipe can be placed in the user's override catalog independently of an OS image
update. Explicit `--recipes-path` or `$RECIPE_PATH` selects only that catalog;
otherwise the XDG recipe directory overrides matching shipped recipe filenames.

Packages publish regular `bin/COMMAND` and `share/PATH` files, with a name and
version. The coordinator copies them into an owned profile generation and commits
the files and installed receipt through one atomic symlink switch. File conflicts,
symlinks, special files and setuid/setgid payloads fail before switching. No
activation script executes on the host. Failed acquisition or activation preserves
the previous profile.

Outside the immutable system route, root's package root is `/var/lib/recipe`; an ordinary user's is
`$XDG_DATA_HOME/recipe` (normally `~/.local/share/recipe`). `--package-root PATH`
selects another store. Its `profile/bin` contains active commands; installed login
profiles add the booted slot system path and ordinary user paths to PATH. `remove` uses the installed receipt
without the recipe or runtime. `installed` only reads, so an ordinary user may
also read a store root owns (`recipe installed --package-root /var/lib/recipe`);
changing a store still requires owning it. `isinstalled` checks active files and links. Removed
packages disappear from the profile; old inactive generations and build/publication
caches remain. They are not an installed-package inventory.

For a front end, all read-only and without Ginux: `recipe installed --json`
adds to each package what its recipe was allowed to use when it was installed
(`needs`), when (`installed_on`), the length of its files (`size_bytes`) and
the earlier versions still held (`kept`). `recipe updates` says which installed
packages `recipe update` would change, from the receipts, the collection and
the sources as last fetched, and lists under `changes` every NEEDS field in
which the newer recipe differs from what was recorded (`added` and `raised`
mean it may use more; `null` when the receipt predates the record).
`recipe go-back NAME [--to VERSION]` activates an earlier version again from
the files an inactive generation holds; `recipe storage` reports what the
active and the inactive generations take, and `--clear-earlier` deletes the
inactive ones. `recipe history [--json]` lists what was done to the store,
newest first (install, update, remove, go-back, hold, unhold, clear-earlier),
from `history.jsonl` in the package root: one JSON object per line with `at`
(seconds since 1970), `what`, `package`, `from`, `to` and `source`, null where
one does not apply. It is written under the store's lock after each change; a
failure to write it is reported on stderr and does not undo the change. It keeps
the newest entries (about 256 KiB when it passes 512 KiB). `--json` (and piped
output) prints `{"schema_version": 1, "history": [...]}`.
`recipe hold NAME` and `recipe unhold NAME` set `held` in the receipt (a new
generation, like remove; absent when false). `recipe update NAME` refuses a held
package before it needs Ginux; `recipe updates` still lists it with `"held":
true`, `recipe installed --json` shows `held`, and `recipe install NAME` and
`recipe go-back NAME` keep it as it is. With `--machine-events`, a failed
install carries `"refused": {"endpoint", "declared"}` when Ginux's log of that
run records a denied connection: `endpoint` is the numeric address the audit
line names, `declared` the recipe's NEEDS.network. The history, hold and
unhold commands were run on a hand-made store; `refused` is compiled and not
run, since it needs a failed install under Ginux. These were run on a hand-made store; the record of `needs` at
install has compiled and not run, since an install needs Ginux.

The OS builder packages the public catalog, `/usr/libexec/recipe/ginux`, and
`/usr/share/recipe/runtime/{rootfs.tar,root-reference}`. These are runtime defaults;
explicit `GINUX_*` selections still take precedence. The image is digest-verified
before execution. Existing images must be rebuilt to carry these files; a source
change alone does not update a running installation. Native Ginux still requires
whole-coordinator bootstrap privileges, as described below.

## Shared execution contract

The executable interface is API 2 with a literal `const NEEDS = #{...};`.
Recipes own their typed inputs/outputs, endpoint grants and resource limits.
Callers bind declared inputs and may explicitly narrow authority. A caller cannot
add an input, endpoint or larger limit that the named recipe did not declare.
Restrictions must first be valid lowerings/subsets for the named recipe. They
also cover every dependency: limits take the stricter bound, and endpoint sets
intersect each dependency's own declaration. Limits remain per instance.

The launcher must support configured downstream catalog 1002,
`configured-network-audit=1`, `configured-denial-audit=1`, configured `launch`,
export, control and restore. Recipe uses upstream `[contract]` and
`[contract.*]` sections; the former `[userspace.*]` format is rejected. Its
pinned base preserves the admitted operation set. Explicitly excluded
socket/listener calls return EPERM under the new rules; absent semantic grants
still return ENOSYS. Runtime resource, descriptor, storage and endpoint limits
are unchanged. Recipe validates the complete derived contract before launch;
a generic `config` parse alone does not establish compatibility.

The historical maintained source baseline in [Linux setup](docs/setup-linux.md)
uses the previous schema. The current migrated source pair and its validation
are recorded in the superproject's
`docs/ginux-contributions/upstream-integration-2026-11-09.md`; it is local until
separately authorized for delivery. Do not substitute baseline receipts for
that combination or claim complete product/security acceptance.

## Build and prepare the environment

[Linux setup and bootstrap](docs/setup-linux.md) records the verified host,
input reproduction, package roles, required privileges and output ownership.
Select the recorded source pair and matching worker; final delivery of later
local corrections remains separately tracked.

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

Execution and inspection commands print readable summaries when stdout is a
terminal. Add `--json` for the full result, or `--json-output result.json` to save
it. Redirected or piped execution/inspection output keeps the existing JSON
format. Progress and diagnostics go to stderr; `list`, `info` and `hash` print
text. Each command's `--help` includes examples.

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
recipe install distro/builder/os-recipes/install-tools.rhai --define BIN=recfstab \
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
  readiness checks and explicit remove/cleanup require matching cached dependency
  products and run only dependency installed checks. Missing or changed products
  fail before the main operation; dependencies are never installed implicitly.
- `list` and `info` inspect loader-owned declared metadata without running setup
  or helper code. Dynamic metadata is reported unavailable.
- `list --json` prints one entry per recipe for a front end (`schema_version` 1):
  name, version, description, path, the recipe's `ABOUT` block, NEEDS, dependency
  names and the hooks it defines. It needs no Ginux launcher, and a recipe that
  cannot be read is reported under `problems` instead of failing the list.
- `index [FOLDER]` writes `index.json`, the file a source of recipes publishes
  so its recipes can be listed without downloading them: the folder's
  `repo.toml` (`name`, `title`, `about`, `address`, `keeper`, optional `uses`),
  every `list --json` entry without its path, the SHA-256 and size of each
  recipe file and of the files beside it that share its name, and the commit
  they are at. The folder must be in a Git repository with full history and
  the listed files committed. `--check` fails when the file is out of date.
- Sources add recipes beyond the recipe folders. `source add ADDRESS` adds a
  remote source: a repository on github.com or gitlab.com, or the address its
  files are fetched from with `{rev}` and `{path}` in it. Only its published
  index is kept; `refresh [NAME]` fetches it again over HTTPS. `source add
  --path FOLDER` adds a checkout that is read as it is. `source list` shows
  them and `source remove NAME` removes one. A source is one file,
  `sources/NAME.toml` under the package root (`/var/lib/recipe/sources` for
  root), or `/usr/share/recipe/sources` when the image ships it. An image
  ships a copy of its source beside that file, `NAME/` with the published
  index and its files: it stands in for the index until one is fetched and
  spares downloading a file it holds, so shipped recipes install offline.
- `SOURCE:NAME` names one source's recipe; `local` names the recipe
  folder. A bare name means a recipe folder first, then the one source that
  has it; when several do, Recipe asks for `SOURCE:NAME`. `list` never touches
  the network: it shows a remote source from its kept index, and `list --json`
  adds `sources` and each recipe's `source`. An explicit `--recipes-path` or
  `$RECIPE_PATH` uses no sources.
- Naming a recipe of a remote source downloads it with every recipe it needs
  (`deps`, `build_deps`), each file at the index's commit and checked against
  the index's SHA-256 and size; a file that differs stops before anything
  runs. `install` and `update` fetch the index first and keep the copy in
  `fetched/SOURCE/NAME-KEY/` under the package root; other commands use the
  kept index and the cache. The receipt records `source`, `commit` and the
  recipe file's `sha256`. `update NAME` follows the receipt's source and runs
  nothing when the source has not changed; `remove` deletes the kept copies.
- `install --sha256 HASH` runs only when the recipe file has that checksum:
  what a person looked at in a front end is what runs.

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
the imported input and earlier published generations unchanged. The CLI handles
package activation separately; `--no-activate` selects only this producer lifecycle.

## State, diagnostics and recovery

`--build-dir` selects a stable logical destination and its shared lock. Domain
work remains `/work/build`. The default destination is under the selected state
root, which defaults to `$XDG_STATE_HOME/recipe` (normally `~/.local/state/recipe`).
Use `--state-dir PATH` to choose a different state root.

Identity includes the recipe/module/dependency closure, declared input content,
custom defines, execution options and the root image, by content. Changed inputs
start fresh state. Recipe itself (the coordinator and the worker) and the Ginux
launcher enter it by declared behaviour versions, not by their bytes, so a
rebuilt `recipe` or `GINUX_BIN` that behaves the same reuses saved state. The
versions live in `src/identity.rs`: `ENGINE_BEHAVIOUR` (bumped with any Recipe
change that can alter what a recipe produces: helper semantics, target metadata,
staging, how inputs and outputs are materialized, the rendered Ginux config, a
Rhai or archive-crate upgrade that changes results; not for a pure speed-up),
`GINUX_BEHAVIOUR` (bumped when a Ginux change can alter what a workload observes:
syscall results, VFS behaviour, credentials, clocks, signals, the snapshot or
export format), the coordinator/worker transport version, and the Ginux catalog
and build-base contract digests Recipe pins and checks. `--exact-identity` (or
`RECIPE_EXACT_IDENTITY=1`) also names the worker's and the launcher's bytes, so
only byte-identical tools reuse saved state; release builds use it. Exact and
default identities never share saved state. Checkpoints become recoverable only after both files and separate
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
| `-r, --recipes-path DIR` | Explicit name/module/dependency search directory. Otherwise the user's recipe folder precedes the sources; `$RECIPE_PATH` selects one directory exclusively. |
| `--package-root DIR` | Store for installed receipts, generations and the active `profile` symlink. |
| `--no-activate` | Produce outputs or run hooks only, without installing/removing an active package. |
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
| `--progress-file PATH` | Keep PATH current with the recipe's `progress(done, total, what)` reports: one JSON object (`version`, `invocation`, `recipe`, `phase`, `what`, `done`, `total`, `started`, `updated`, `state` = `running`/`finished`/`failed`), replaced atomically at most once a second. |

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
Scoped HTTPS acquisition retries transient 429/502/503/504 responses at most
twice. Numeric `Retry-After` delays up to five seconds are respected; longer
delays, HTTP-date values and malformed values defer automatic retry instead of
retrying before the origin's requested deadline.
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

Manual `isacquired`, `isbuilt`, `isinstalled`, `remove` and `cleanup` run dependency installed checks
in graph order and require matching cached dependency products. They never run
dependency acquire/build/install hooks to repair missing or changed products.
Use `install` separately to prepare those dependencies. The checked snapshots
retain the parent's installed input identity and each dependency's own authority.
For `remove`/`cleanup`, unavailable matching dependency products fail before the
main hook runs, preserving the prior state and files. Dependency remove/cleanup
hooks are not run by the parent's operation.

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

For a release, set the version in source before verification, and retain
artifact hashes with the matching source, worker, launcher and image provenance.
CI validates the selected commit. Publish the reviewed artifacts separately.

License: MIT OR Apache-2.0.
