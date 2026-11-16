# Recipe interface and scope requirements

This is the current implemented interface contract. The [author guide](WRITING_RECIPES.md)
provides executable examples, the [README](README.md) provides operation/recovery,
and the [helper reference](HELPERS_AUDIT.md) owns registered signatures. The
superproject's `docs/ginux-recipe-interfaces.md` records the coordinator/launcher
boundary; its implementation checkpoint records exact tested revisions and gaps.

## Execution ownership

- Every recipe declares `const RECIPE_API = 1;` and a literal `NEEDS`.
  Coordinator discovery, metadata and input staging never evaluate arbitrary
  recipe code or helper code.
- Before anything runs, Recipe reads each recipe's source (and the base it
  extends and the modules it imports) and picks where it runs. If nothing in it
  can call a helper that starts a program or reads the root image, it runs on
  the host as a confined process: files only beneath the run's own directories,
  network only to the `NEEDS.network` endpoints at the addresses Recipe pinned,
  no program helpers, and a seccomp filter that refuses `execve` and new
  processes. Everything else, and anything the analysis cannot read, runs in
  the configured Ginux domain. `--execution auto|ginux|host` selects the rule;
  `recipe info` and the run say which way each recipe went and why. See
  "Execution modes" in the [README](README.md).
- The coordinator selects/verifies the environment image and worker (needed
  only for runs in Ginux), imports explicit inputs, owns destination locking
  and durable records, and publishes named products. Unavailable runtime
  capabilities fail explicitly.
- Recipes own package versions, source URLs/digests, build facts, typed
  input/output declarations, and endpoint authority and limits in `NEEDS`.
  Rust/TOML owns orchestration, path bindings and consumer validation.
- External commands/helpers remain subject to the same assignments and resource
  bounds in either mode. There is no unconfined host execution and no network
  access outside the declared endpoints.
- Recipe never asks an AI and never changes a recipe. A failure is reported;
  what to do about it is the user's.

## Recipe contract

- The format is one: constants and optional functions, specified in
  [the recipe format](docs/recipe-format.md#1-a-recipe-at-a-glance) and taught in
  the [author guide](WRITING_RECIPES.md). No other form is accepted.
- Recipe decides what is done from its own records, never from recipe code; see
  [states](docs/recipe-format.md#62-the-states-of-a-program). A saved value
  never proves that work is done; see
  [saved values](docs/recipe-format.md#7-saved-values-and-resuming).
- Errors and malformed values fail execution. A required step that did not
  produce its result fails clearly.
- Optional `cleanup(reason)` runs at the end of a run and on `recipe cleanup`;
  see [`cleanup(reason)`](docs/recipe-format.md#37-cleanupreason-answer-12).
  Cleanup failures are reported without hiding an existing primary failure.
- Literal modules, dependency declarations and base inheritance are resolved by
  the loader. Dynamic declarations and cycles fail before anything runs.

## Filesystem and products

- Inputs declared as file/directory (optionally `?`) are explicitly bound
  read-only at `/inputs/NAME`. Missing required, undeclared and wrong-kind
  bindings fail preflight. Dependencies receive only their explicit DEP::NAME
  bindings and run under separate declared grants. A host path in a
  define is not an import or filesystem grant.
- The run owns its work folder, `/out` (the selected-target/product tree) and
  scratch space. Recipe code sees them through the engine's constants; see
  [the engine's constants](docs/recipe-format.md#4-the-engines-constants).
- A named `target` input seeds `/out` on fresh state; retained state restores the
  owned target. Compiler-image files cannot satisfy selected-target readiness.
- Input identity includes bytes, modes and links. Confined source links and
  rootfs/target symlink bytes are preserved without following host paths.
  Hardlinks preserve shared owned identity through copy/checkpoint/publication.
- Consumers validate required output names/kinds and use returned publication
  paths, never sibling-path guesses, stale JSON or unrequested host-built
  outputs.
- Publication requires successful managed execution and independent bounded
  archive/path/link validation. Failed/interrupted work cannot replace valid
  earlier publications. Imported inputs and previous generations stay immutable.
- Removal changes owned selected-target files (the producer lifecycle,
  `--no-activate`). Putting a package output into a profile is the CLI's job,
  below.
- After a successful install Recipe deletes the downloads, unpacked sources,
  build material and saved state, unless the user chose `--keep`; see
  [disk rules](docs/recipe-format.md#65-disk-rules).

## Installed software

- A directory output named `package` is activated by `install` and `update`
  after publication, through one atomic profile switch into the selected package
  root: root's `/var/lib/recipe`, a user's `~/.local/share/recipe`, or
  `--package-root`. `remove` works from the receipt, without the recipe or a
  runtime. The store keeps only the active generation: the one a change
  replaces is deleted once the new one is active. `storage` and `history` are
  Recipe's own concept.
- `recipe list --json` carries, for a front end, each recipe's `ABOUT`, `NEEDS`,
  `runs_here` and `not_runnable_reason`, and at the top `host_libc`. A recipe
  whose declared C library this system cannot run is refused at install, unless
  `--any-libc` says otherwise.
- **System scope** (`--system`, or root without `--package-root`) on an A/B
  system (`recab.immutable` on the kernel command line): the booted root is
  locked and Recipe never writes into, remounts or unlocks it. Changes go into
  the unlocked other root, at the `other_mount` that `recab status --json`
  reports, and are active when that root is booted. If the other root is not
  mounted, Recipe stops and says so.
- Recipe keeps `other-root.json` in its package root (`/var/lib/recipe`) to know
  what it wrote there. `recab copy` overwrites the other root; before any
  system-scope command Recipe finds the changes neither root holds and writes
  exactly that system part again, never the whole install and never "for me"
  software. Until it has, that part is reported as not done.
- Without `recab.immutable` the system has one root and no recab: `--system`
  means root's own package root and Recipe does not look for an other root.

## Acquisition

- Payload downloads require recipe-owned full SHA256 pins and checked staging
  before replacement. Resume validates range/response identity. Failed/truncated
  or wrong-digest responses preserve valid previous destinations.
- Git acquisition uses an exact destination and full 40/64-hex commit, validating
  the origin and resulting tree. Imported local repositories remain explicit.
- Network access is off unless NEEDS declares bounded outbound TCP endpoints;
  callers can only narrow that set and the declared limits. Explicit ceilings
  apply to all dependencies via numeric minima and endpoint intersections.
  Hostnames are resolved/pinned before launch; redirects require grants too.
  Domain clients share accounting/readiness/revocation. Endpoint grants do not
  isolate virtual hosts on the same allowed IP and port.
- Extraction/conversion failures propagate. A registered helper is not evidence
  that every external archive/binary is supported by the selected environment.

## Recovery and diagnostics

- Destination locking covers competing recipe/input versions. Checkpoint acks
  require matching durable state/files records; unacknowledged work is discarded.
- Changed recipe/input/image/worker identity starts fresh state. A corrupt current
  record/snapshot fails before startup rather than silently pretending a fresh
  installation. An explicit new state directory starts fresh recovery state.
- State retention protects current/recoverable generations under existing locks.
  Diagnostic/scratch leases protect active invocations. Pruning recognizes only
  owned layouts and never treats published products as expendable snapshots.
- Diagnostics and machine outcomes survive invocation scratch cleanup and are
  bounded. Human logs are never parsed as success/protocol results. Retained declaration/
  contract receipts and bounded audit observations support explanation; missing
  observations stay unknown. Complete endpoint summaries support unused-grant
  warnings without affecting authorization.
- Cancellation and execution/transport deadlines terminate descendants, preserve
  the last complete checkpoint and never publish failed work. Silent execution
  is distinct from a stalled partial protocol frame.

## Metadata

- Metadata comes from the loader's declared/merged AST without running recipe
  code. Literal inherited facts are reported, dynamic facts are unavailable, and
  malformed declarations fail clearly.

## Evidence boundary

Focused real CLI, managed producer/consumer and runtime checks establish the
observed behavior for their actual revisions, image and inputs. Compilation,
fixture downloads and prepared-kernel checks do not establish kernel production,
full distro/VM readiness or arbitrary Linux compatibility. The authoritative
implementation checkpoint keeps outstanding production/runtime work explicit.
