# Recipe interface and scope requirements

This is the current implemented interface contract. The [author guide](WRITING_RECIPES.md)
provides executable examples, the [README](README.md) provides operation/recovery,
and the [helper reference](HELPERS_AUDIT.md) owns registered signatures. The
superproject's `docs/ginux-recipe-interfaces.md` records the coordinator/launcher
boundary; its implementation checkpoint records exact tested revisions and gaps.

## Execution ownership

- Executable recipes declare `const RECIPE_API = 2;` and run inside the configured
  Ginux domain. Coordinator discovery, metadata and input preparation never
  evaluate arbitrary recipe setup or helper code.
- The coordinator selects/verifies the environment image and worker, imports
  explicit inputs, owns destination locking and durable records, and publishes
  named products. Unavailable runtime capabilities fail explicitly.
- Recipes own package versions, source URLs/digests, build facts and lifecycle
  behavior. Rust/TOML owns orchestration, permissions and consumer contracts.
- External commands/helpers remain subject to the same domain assignments and
  resource bounds. There is no native host execution/network fallback.

## Recipe contract

- Setup runs once per invocation with immutable `RUNTIME`; hooks receive the same
  `ctx.runtime`. Current facts are initialized from current recipe bytes.
- Checks return `ready(ctx)` or `needs_work(ctx, nonempty_reason)`. Errors and
  malformed values fail execution. Custom checks inspect actual relevant
  products/configuration; saved booleans do not establish installation.
- Acquire/build/install/remove hooks return updated context. Successful work is
  checked against custom readiness. Required missing work fails clearly.
- Only `ctx.state` is restored. It contains bounded JSON-compatible values and
  never replaces current package facts, runtime paths or output declarations.
- Optional cleanup receives `manual` or `auto.<phase>.<success|failure>`. Cleanup
  failures are reported without hiding an existing primary failure.
- Literal modules, dependency declarations and base inheritance are resolved by
  the existing loader. Dynamic declarations and cycles fail preparation.

## Filesystem and products

- Inputs are explicitly imported read-only at `/inputs/NAME`. A host path in a
  define is not an import or filesystem grant.
- `/work/build`, `/work/tools` and `/work/tmp` are owned work/tools/scratch;
  `/out` is the owned selected-target/product tree. Runtime exposes these paths.
- A named `target` input seeds `/out` on fresh state; retained state restores the
  owned target. Compiler-image files cannot satisfy selected-target readiness.
- Input identity includes bytes, modes and links. Confined source links and
  rootfs/target symlink bytes are preserved without following host paths.
  Hardlinks preserve shared owned identity through copy/checkpoint/publication.
- Recipes select actual files/directories in `ctx.outputs`. Consumers validate
  required names/kinds and use returned publication paths, never sibling-path
  guesses, stale JSON or unrequested host-built outputs.
- Publication requires successful managed execution and independent bounded
  archive/path/link validation. Failed/interrupted work cannot replace valid
  earlier publications. Imported inputs and previous generations stay immutable.
- Removal changes owned selected-target files. Live-system activation is outside
  this interface.

## Acquisition

- Payload downloads require recipe-owned full SHA256 pins and checked staging
  before replacement. Resume validates range/response identity. Failed/truncated
  or wrong-digest responses preserve valid previous destinations.
- Git acquisition uses an exact destination and full 40/64-hex commit, validating
  the origin and resulting tree. Imported local repositories remain explicit.
- Network access is off unless the caller grants bounded outbound TCP endpoints.
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
  bounded. Human logs are never parsed as success/protocol results.
- Cancellation and execution/transport deadlines terminate descendants, preserve
  the last complete checkpoint and never publish failed work. Silent execution
  is distinct from a stalled partial protocol frame.

## Optional repair and metadata

- Normal execution requires no AI credentials. Explicit optional repair runs in
  the coordinator, bounds attempts/paths/provider work and protects readiness.
  Changed source bytes receive a new identity before retry.
- Metadata comes from the loader's declared/merged AST without executing setup.
  Literal inherited facts are reported, dynamic facts are unavailable, and
  malformed declarations fail clearly.

## Evidence boundary

Focused real CLI, managed producer/consumer and runtime checks establish the
observed behavior for their actual revisions, image and inputs. Compilation,
fixture downloads and prepared-kernel checks do not establish kernel production,
full distro/VM readiness or arbitrary Linux compatibility. The authoritative
implementation checkpoint keeps outstanding production/runtime work explicit.
