# Moving Recipe off `ginux launch`

Plan of 2026-11-22. Document only: no code was changed, nothing was built or
run, nothing was posted upstream. The drafts in section 5 are for the owner to
read and release; none has been filed.

## 0. Why, and the short version

On 2026-11-22 Greg Kurtzer (gmkurtzer/ginux) answered issue #294:

- the fork's "configured launch" subsystem (owned storage, the VFS redesign,
  `launch_tasks`, hooks, about 39k lines) stays downstream in `LevitateOS/ginux`;
- `launch` is a private runtime protocol, not something downstream callers may
  rely on; `native` is not a replacement either;
- Recipe should plan to move to the public lifecycle workflow
  (`create`/`import`/`start`/`attach`/`stop`, plus explicit result export once
  that lands);
- he wants focused requirements, each filed against the upstream gate or
  workflow it affects.

Findings, in one paragraph. Recipe uses `ginux launch` as a **one-shot,
foreground, caller-owned run** with a request file, a result file and JSON
line records on the worker's stdout (the stdio frame protocol and mid-run
snapshots were removed in Stage 2, section 6), a private disposable root,
memory-owned storage, an export archive and a restore file. Upstream's public workflow is a **named, persistent,
detached instance** on the root-owned `/var/lib/ginux`, with a PTY console, a
host-directory root and no delete, no wait, no exit status in `status` and no
way to read files out. Of Recipe's nineteen needs (section 3), four are
covered today, five are partly covered, seven have no upstream feature, and
three are dropped (N15 by the owner's decision D1 and N10 by D6, both of
2026-11-22, N19 as not needed).
The seven are the reason a migration cannot start with a code change: they
need requirements upstream first (section 5). Until then Recipe stays on the
fork's `launch`, unchanged (section 6).

Two rules of this repository shape the plan. Recipe has no users and keeps no
backwards compatibility (CLAUDE.md), so the cutover is one step with no period
in which both runtimes are supported. Recipe never calls an AI model; nothing
here adds an agent, and the upstream drafts do not mention it.

## 1. Where Recipe uses configured launch

Everything below is in `tools/recipe`, as it was at `a7b378f` and as the
Stage 2 working tree of 2026-11-22 changed it (section 6); places are named by
module and function, not by line. "Fork-only" means the feature is not in upstream `b729915b`
(checked with `git show b729915b:<path>` in `tools/ginux`).

### 1.1 The launch call itself

| Place | What it does | Depends on |
| --- | --- | --- |
| `src/launch/configured.rs`, `run` (the module's one entry point) | Spawns `GINUX_BIN launch CONFIG ROOT --result-file F --export-file E [--restore-file R]` with stdin `/dev/null` and piped stdout and stderr. | `launch` (private upstream: only `CONFIG ROOT --result-file`); `--export-file` and `--restore-file` are **fork-only**. |
| `src/launch/mod.rs` (`run_domain`, destination lock), `configured::run`, `check_agreement`, `check_launcher_exit`, `src/ginux.rs` (`ResultRecord::read`) | Run lock, run directory, the launcher's result file read as the authority, agreement with the worker's result file, launcher exit check. | Result record `version 1` (`src/ginux.rs`). Upstream `launch --result-file` writes the same record; the public lifecycle has no counterpart. |
| `src/launch/worker_io.rs`, `pump_lines` | Reads the worker's line records (diagnostics and progress) from the launcher's stdout until it ends, relays them live, and keeps a line that is not a record as plain log text. Replaces `pump_protocol` (request frame in, checkpoint/ack pairs, result frame out). | The workload's stdout as a live stream. The public console is a PTY that merges the streams and adds echo (see 2.2); a non-record line is plain log text, so it does not break the reader. |
| `src/domain.rs` (`REQUEST_NAME`, `RESULT`), `src/inputs/root.rs` (`prepare_root`), `worker_io::result_from_archive`, `result_from_tree` | The request goes in as the reserved input `.recipe-request.json`; the result is `/work/.recipe-result.json`, read after the run from the export archive (Ginux) or the run directory (host) as untrusted input that must name the invocation. | Staging an input file (N12) and the export (N14) below. |
| `src/launch/configured.rs` (calls `seed_target`) | Restore: `--restore-file` hands the run its kept downloads and the seeded target view (the `target` input and the dependencies' tools). No checkpoint is restored any more: the checkpoint request over `--control-fd` (`GXS1`/`GXA1`) and its snapshot export are gone. Under the files form the restore tar becomes a staged input. | **Fork-only** (`storage_restore.rs`). |
| `src/launch/mod.rs` (`finish_outputs`), `src/publication.rs` | After the worker's run, the supervisor exports the owned `work/…` and `out/…` trees as a tar; Recipe validates and unpacks it as the published output and as the kept result. | **Fork-only** export (`storage_export.rs`). |
| `src/launch/configured.rs`, `run` (the `pre_exec` hook) | `pre_exec`: lease descriptors kept open across exec, `PDEATHSIG=SIGKILL`, own process group, so killing the coordinator tears the run down (Ginux `PTRACE_O_EXITKILL`). | Foreground child whose lifetime is the caller's. |
| `src/launch/configured.rs` (`bind_read_only`), `src/inputs/mod.rs` | `bind_read_only`: in the launcher's own mount namespace, bind host folders read-only under `/inputs` (the `--system` scope, `InputKind::Bind`). | Launcher inherits the namespace; the root is a host directory Recipe built. |
| `src/launch/configured.rs` (`ensure_open_file_limit`) | `ensure_open_file_limit`: raises `RLIMIT_NOFILE` for the fork's one-descriptor-per-storage-object reservation. | **Fork-only** (`launch_storage.rs`). |
| `src/launch/worker_io.rs` (`reap_launcher`, `spawn_stderr_drain`, `join_drain`) | Reaping the launcher; draining its stderr (which carries `ginux:` audit and summary lines) to a capped log. | Launcher stderr stream. |

### 1.2 The instance configuration Recipe writes

`src/inputs/config.rs:52-190` renders the config (limit 4096 bytes), then
`src/inputs/mod.rs:182-196` has Ginux validate it.

| Config line | Meaning for Recipe | Upstream `b729915b` |
| --- | --- | --- |
| `instance.root = "oci:NAME@sha256:D"` | `D` is the SHA-256 of an **assembled rootfs tar** (`GINUX_ROOTFS`), not an OCI manifest (`src/inputs/root.rs`, `src/runtime.rs:23-120`). | `instance.root` is a registry reference with a manifest digest, fetched by `ginux-image`. |
| `init = /inputs/recipe-worker`, 2 args, 8 env entries | The worker is the init. | Same fields exist. |
| `[storage] assignments = ["memory:/work", …]`, `max_objects`, `max_bytes`, `archive_import` | Writable owned storage with limits; nothing writable on the host. | **Fork-only** (`memory:`, limits, archive import). Upstream `storage.assignments` is `root-overlay`. |
| `[network] backend = "mediated"`, `endpoints`, `max_connections`, `max_bytes` | Numeric `IP:port` grants (names are resolved by Recipe, `src/inputs/network.rs`), connection and byte caps. | `mediated` exists as a value; **`endpoints`/`max_connections`/`max_bytes` are fork-only**. Upstream grants are `tcp:PORT`/`udp:PORT` in `[contract.network]`. |
| `[devices] assignments = ["null","random","urandom"]` | Entropy for builds. | Same names exist (`config/instance-gate9.75.toml`). |
| `[contract]` with `catalog = 1002`, `default = "deny"`, derived allow/deny | Recipe's build base. | Upstream catalog is `2`; **1002 is the fork's `CONFIGURED_CATALOG_VERSION`**. |
| `[audit] enabled = true`, `[failure] policy = "stop"` | Audit lines are parsed for reports. | Same fields; format differs (see 2.3). |

### 1.3 The contract, probes and pins

All in `src/needs/contract.rs`.

- `CATALOG = 1002`, `BASE_DIGEST_NETWORK`, `BASE_DIGEST_OFFLINE` (lines 9-40):
  digests of the build base Ginux must report, part of every state identity
  (`src/identity.rs:49-115`). Re-reviewed at every pin move; the e3b71ac0
  review is the last entry in that comment.
- `command` (178), `validate` (312): run `GINUX_BIN instance-validate`
  and require `configured-network-audit=1` and `configured-denial-audit=1`
  (**fork-only** fields) plus the catalog and digest.
- `probe_feature`, `archive_import`, `random_devices` (226-300): ask the
  launcher once per binary whether it accepts `archive_import` or the random
  devices; env `RECIPE_ARCHIVE_IMPORT`, `RECIPE_RANDOM_DEVICES`.
- `src/needs/report.rs:46, 189-215, 225-335`: `recipe inspect`, `validate`,
  `explain` call the launcher (`instance-explain`) and parse `Rule: … source=
  index=`; run reports parse `ginux: audit`, `ginux: denial-summary` and
  `ginux: network-summary` lines from the supervisor log.
- `src/target.rs:1301-1540`: the `GINUX_ARCHIVE_IMPORT` ioctl (`0x40104701`),
  falling back to member-by-member extraction on `ENOTTY`. Optional speed-up,
  **fork-only**.

### 1.4 Resolving and shipping the launcher

- `src/backend.rs:8-30, 579-615`: `GinuxLauncher::from_env` checks that
  `GINUX_BIN` (or the installed `/usr/libexec/recipe/ginux`, `src/runtime.rs:8,
  181`) is an executable file. `src/lib.rs:41-81, 158-202` carries it in the
  library API.
- `src/retained.rs:278-330`, `src/identity.rs`: with exact identity the
  launcher's bytes enter the saved-state key.
- `src/bin/recipe/runtime.rs:111, 349-367, 438`: `recipe runtime stage` copies
  the launcher to `libexec/recipe/ginux` and writes an AppArmor profile for it
  (`data/apparmor-ginux`).
- Tests and scripts that name the launcher: `tests/e2e/*.rs` (wrappers that
  `exec` the real `GINUX_BIN` for `instance-*` verbs),
  `tests/library_backend.rs`, `scripts/managed-integration.py`,
  `scripts/prepare-managed-image.py`.
- Outside `tools/recipe`: `distro/builder/src/recipe/mod.rs` and
  `distro/builder/src/pipeline/sources.rs` build the fork launcher and set
  `GINUX_BIN`. Nothing outside Recipe calls `ginux launch` itself.

### 1.5 What is not affected

Host-mode runs (`src/execution.rs`, `src/host.rs`) never call Ginux and keep
working unchanged; `host.rs` mirrors the Ginux run's shape (same snapshot tar,
same publication path), which makes it the reference for what the lifecycle
adapter must reproduce.

## 2. What upstream's public workflow does today

Read at `tools/ginux` `e3b71ac0`, comparing against upstream `b729915b` (the
merge of #304). The fork added 102 files and about 39k lines on top of
`b729915b`; `git diff --stat b729915b e3b71ac0` shows no change to
`crates/ginux-cli` or `scripts/ginux-host-*`, so the public CLI and the helper
read the same in both, and what follows is upstream's behaviour.

### 2.1 Surface (Gate 25, `docs/cli-command-model.md`)

Public commands, flat: `create`, `import`, `set`, `edit`, `export`, `list`,
`status`, `start [--attach] [--audit]`, `attach`, `stop`, `lock`, `unlock`,
`validate`, `inspect`, `explain`. No `diff`, no `delete`, no `restart`, no
`logs`, no image management; the gate lists these as outside its scope. The
CLI is a clap binary (`crates/ginux-cli`) that calls the privileged helper
`/usr/libexec/ginux/ginux-helper` through `sudo --protocol 1`. `launch` is
documented only as an internal maintainer diagnostic of `ginux-runtime`
(README, "Internal configured native launch diagnostic").

### 2.2 Behaviour (`scripts/ginux-host-instance-helper.sh`)

- **State** is global and root-owned: `/var/lib/ginux/{cache/oci,instances}`,
  transient files under `/run/ginux/instances/NAME` (Gate 26). The helper reads
  `GINUX_STATE_ROOT` and `GINUX_RUN_ROOT` from the environment; I found them
  documented nowhere as operator options.
- **Root.** `import NAME FILE` reads `instance.root` (a digest-qualified
  registry reference) and `ginux-image materialize` expands it from the OCI
  cache into `instances/NAME/root`, a writable host directory chowned to
  `65534`. Gate 26: "Assemble a new instance directly from verified cached
  OCI blobs ... Do not persist a redundant assembled rootfs.tar". Replacing an
  instance cannot change its root. Every `create`/`import` of a new name
  expands the whole image.
- **Start.** `start` is detached by design: the helper `nohup`s a supervisor
  and returns after one second. The workload runs under `script(1)`, so it
  has a **PTY** for stdin/stdout; stdout and stderr both land in
  `console.log`, the audit stream in `audit.log`. Input arrives through the
  `console.in` FIFO (`attach`).
- **Exit.** The supervisor writes the exit status to
  `/run/ginux/instances/NAME/exit.status`; `status` prints only
  `name/state/contract/pid/paths` and `list` only the state. The file is
  deleted on the next `start`. `attach` reads it to print "stopped (status N)".
- **Stop.** Sends `RTMIN+4` to the instance's first process (the systemd
  shutdown signal), waits 60 seconds, then TERM, then KILL.
- **Contract diagnostics.** `validate`, `inspect`, `explain` print text
  (`Contract valid: instance= catalog= digest= operations=`, `Rule: … source=
  index=`). There is no machine-readable form.
- **Hooks** run as managed instance processes before init (`[hooks]`);
  host-context hooks are rejected.
- **Config surface** (`ginux-instance`): `instance`, `identity`, `resources`
  (`vcpus`, `memory_mib`), `execution`, `contract.*`, `storage.assignments`,
  `volumes.assignments` (`/host:/target:ro|rw`), `devices.assignments`,
  `network` (`backend`, `interface`, `source`, ingress/egress lists, port
  forwards), `access`, `hooks`, `lifecycle.stop_timeout_seconds`, `audit`,
  `failure`.
- **Volumes are parsed and validated but I found no consumer.**
  `crates/ginux-init/src/launch.rs:439` rejects a non-empty list, nothing in
  `ginux-init` or the helper references it otherwise, and Gate 31 says "do not
  enable ... external volume behavior". Treat volumes as unimplemented until
  Greg says otherwise.

### 2.3 Gate positions that matter

| Gate | State at `b729915b` | Relevance |
| --- | --- | --- |
| 25 declarative CLI | merged upstream (#273-#289); its record is still in `gates-todo` | owns the command set; a missing command is a new gate or an extension of this workflow |
| 26 Rocky 10 RPM | merged upstream with 25; its record is still in `gates-todo` | owns `/var/lib/ginux` layout, `cache/oci`, `/usr/libexec/ginux`; Rocky 10 x86-64 only, no sudoers rule |
| 29 shared-network destination policy | todo (issue #283) | destination checks for `sendto`/`sendmsg`/`sendmmsg` against the same protocol/port contract as `connect` |
| 31 filesystem mediation foundation | todo (issue #290 umbrella) | "do not enable ... external volume behavior"; filesystem is staged one operation family at a time; reserved area |
| 46 network default deny | todo (issue #298) | empty allow list denies; relevant to how a downstream grant list behaves |
| 11 performance characterization | todo | the 4% budget is measured on public instance modes, which is where Recipe would be after migration |

No upstream gate or issue mentions an export of instance files. "Explicit
result export" is therefore only Greg's statement of direction.

## 3. Gap table

Status: **covered** = upstream does it today; **partial** = a mechanism
exists but not the behaviour Recipe needs; **missing** = nothing upstream.
"Gate" is where the missing piece belongs; "Req" points to section 5.

| # | Recipe need | Today (launch) | Upstream public lifecycle | Status | Gate | Req |
| --- | --- | --- | --- | --- | --- | --- |
| N1 | Run one program (the worker) as init with fixed argv and environment | `init`, `args`, `environment` | same fields; `start` runs init under `native … configured` | covered | – | – |
| N2 | `vcpus` and `memory_mib` limits | config | `resources` | covered | – | – |
| N3 | Syscall contract with default deny, derived allow/deny | catalog 1002 + digests | `[contract]` catalog 2, `validate`/`inspect`/`explain` | partial: digests and catalog differ; pins must be re-derived and the output is text | 25 | U8 |
| N4 | `/dev/null`, `random`, `urandom` | config | `devices.assignments` | covered | – | – |
| N5 | Audit on, failure policy stop | config | `[audit]`, `[failure]` | covered | – | – |
| N6 | Root from an assembled rootfs tar identified by its own digest, offline | `GINUX_ROOTFS` + `GINUX_ROOT`, verified, reflink-cached | registry reference only; `cache/oci` filled by `ginux-image` | **missing** | 26 | U1 |
| N7 | A fresh private root per run, cheap to make and to remove, many in parallel | tar clone into a temp dir, deleted after | full expansion per `create`; no `delete`; names global | **missing** | 25, 26 | U2, U3 |
| N8 | Foreground run whose lifetime is the caller's (kill on cancel, deadline, coordinator death) | child process, `PDEATHSIG`, process group | detached supervisor; `stop` is 60 s of `RTMIN+4`; a dead caller leaves a running instance | **missing** | 25 | U4 |
| N9 | The run's outcome (`exited`+code, `signaled`+signal, `supervisor-error`) as a value | result file, version 1 | `exit.status` in a private run directory, deleted at next start; not in `status` | partial | 25 | U4 |
| N10 | Structured two-way channel to the workload (request, checkpoint acks, result) | stdin/stdout pipes | PTY console (echo, line discipline, merged streams); input by FIFO | **dropped** (D6: request and result become files; the need moves to N7, N12, N14) | – | – (U5 not filed) |
| N11 | Stdout/stderr and supervisor diagnostics kept apart, bounded | separate pipes, capped drain | `console.log` and `audit.log` per instance, unbounded files | partial | 25 | U4 |
| N12 | Read-only inputs, including bound host folders, under `/inputs` | copied into the root; folders bound read-only in the launcher's mount namespace | `volumes` parsed, not served; or write into the instance root directory (private state) | **missing** | 31 | U7 |
| N13 | Writable `/work`, `/out` with object and byte limits, no writable host path | `memory:` storage, `max_objects`, `max_bytes` | `root-overlay` on a host directory; no limit | **missing** | 31 | U9 |
| N14 | Published results taken out through one validated stream | export tar from the supervisor | nothing; reading `instances/NAME/root` is private state owned by `65534` | **missing** | 25, 31 | U6 |
| N15 | Mid-run checkpoint of owned files paired with saved values, and restore | `--control-fd` (gone), `--restore-file` | nothing | **dropped** (D1: `checkpoint` is retired; `--restore-file` remains only to seed kept downloads and the target view, which becomes a staged input under the files form) | – | – (U10 deleted) |
| N16 | Numeric `IP:port` network grants, connection and byte caps, bounded usage and denial summaries | `network.endpoints`, caps, `ginux: network-summary` | `contract.network` protocol/port lists, shared host network | **missing** | 29 | U11 |
| N17 | Stable machine-readable `validate`/`explain` and audit lines for `recipe inspect` and run reports | `instance-validate` fields, `Rule:` line, `ginux: audit …` | text, no schema | partial | 25 | U8 |
| N18 | Run on the hosts Recipe supports (Fedora 44, Ubuntu 24.04 for the standalone release; the OS builders) with a private state root and no sudo wrapper losing descriptors | launcher binary copied next to Recipe, AppArmor profile | Rocky 10 RPM only (Gate 26 non-goals); global `/var/lib/ginux` through `sudo` | partial | 26 | question Q1 |
| N19 | Fast whole-tree unpack (archive import) and per-object descriptor reservation | `archive_import`, `RLIMIT_NOFILE` raise | none | not needed: Recipe already falls back (`ENOTTY`), the reservation disappears with `memory:` | – | – |

Reading the table: N1, N2, N4, N5 are covered and carry over unchanged. N3,
N9, N11, N17, N18 are partial. Seven needs (N6, N7, N8, N12, N13, N14, N16)
have no upstream feature. N15 is dropped by decision D1, N10 by D6 and N19
as not needed. The covered four are lines of configuration, so the missing
seven decide the schedule.

### 3.1 Two alternatives that remove needs without upstream help

These are Recipe-side designs. Each is an owner decision (section 7), not an
assumption.

- **Replace the stdio protocol with files (N10 partly).** The request goes in
  as an input file, the result is a file under `/out`, progress is log lines.
  Recipe owned `src/worker/protocol.rs` (now `src/worker/records.rs`), has no
  users, and could change it.
  This removes the need for a structured live channel everywhere except
  checkpoints (N15). DECIDED 2026-11-22 (D6): files. With `checkpoint` retired
  (D1) no live exchange is left. The request still has to get in (N12, U7 or
  writing it into the instance root, U2/U3) and the result out (N14, U6).
- **Retire mid-run `checkpoint` (N15) — DECIDED 2026-11-22 (D1).** Steps are already separate
  operations (`download`, `build`, `install`, `cleanup`) with kept results
  between them (CLAUDE.md, "Recipe decides what is done from its own
  records"). Only a `checkpoint` inside one `build` needs a live snapshot.
  Without it, N15 and the `--control-fd` code disappear (`--restore-file`
  stays, to seed kept downloads and the target view), and an interrupted build
  restarts that step. This is a change to the recipe
  format. The owner decided to retire it; `save`, `saved`, `reached` have no
  purpose without it and go with it (Stage 2). One recipe uses it for real:
  the kernel build, `distro/builder/os-recipes/linux.rhai` (checkpoints
  `source` and `compiled`), so the hours-long compile has to get its
  resumability another way (decision D5).

## 4. What carries over, what changes

Unchanged by the migration: the recipe format, the engine, the worker's job,
`needs` declarations, retained state and its identity (except the two pins
below), publication validation (`src/publication.rs` already treats the
archive as untrusted), host-mode runs, the offline cache and download store.

Changes, if all requirements land:

- `src/launch/` is split (done in Stage 2, section 6). The lifecycle-independent
  half (`mod.rs`: start decision, `finish_outputs`, `complete`, log recording,
  and `worker_io.rs`: line records, result file, reaping) stays; the
  command-building half, `configured.rs` (`run`, `bind_read_only`,
  `ensure_open_file_limit`), is replaced by a lifecycle adapter with one entry
  point: create instance, stage inputs, start and wait, read outcome, export,
  delete. `pump_protocol` and `capture_checkpoint` no longer exist.
- `src/inputs/config.rs`: no `memory:`, no `endpoints`, no `archive_import`;
  adds `volumes` and the storage limit from U7/U9.
- `src/needs/contract.rs`: `CATALOG` and both digests re-derived for upstream
  catalog 2 and the upstream `validate` output; the two `configured-*-audit`
  checks and both probes (`archive_import`, `random_devices`) are deleted.
  `RECIPE_ARCHIVE_IMPORT` and `RECIPE_RANDOM_DEVICES` go with them.
- `src/ginux.rs` (the result record reader; the `GXS1` snapshot exchange is
  already gone) and `src/target.rs` archive-import ioctl: deleted with the
  launcher.
- `src/backend.rs`, `src/runtime.rs`, `src/bin/recipe/runtime.rs`: the
  resolved thing is the `ginux` CLI (and its helper), not a launcher file;
  `GINUX_BIN` is renamed or removed, `recipe runtime stage` stops copying a
  launcher file; it ships the `ginux` CLI and its helper instead, because
  Recipe ships Ginux (D4, decided).
- `distro/builder` stops building the fork launcher for Recipe.
- `GINUX_BEHAVIOUR` (`src/identity.rs:68`) is bumped at the cutover.

## 5. Requirements to file upstream (drafts)

Each draft is written for upstream: it names a gate or workflow, states the
observed behaviour at `b729915b` and the observable result wanted. It
contains no downstream policy. The fork has tests for most of them
(`scripts/test-launch-*.py`, `tests/fixtures/launch-*-fixture.c`) that can be
offered as source material, as Greg said. None is filed. A suggested order is
in section 6.

### U1. Offline provisioning of the OCI cache — Gate 26 (`cache/oci` layout)

- **Need.** Supply an image to `cache/oci` from a local OCI image-layout
  archive or directory, with digest verification, without a registry.
- **Observed.** `ginux import`/`create` always call `ginux-image materialize
  REFERENCE /var/lib/ginux/cache/oci DEST`; the cache is filled only by
  fetching `instance.root` from its registry (`scripts/ginux-host-instance-helper.sh`
  `materialize_image`). README states the tests run offline once the caches
  are populated, by an explicit fetch target.
- **Wanted.** One explicit, documented operation that adds a verified local
  image to the cache under its manifest digest, after which `instance.root`
  naming that digest resolves with no network. Verification failure leaves the
  cache unchanged.
- **Non-goals.** No change to digest-qualified roots, no mutable tags, no
  signing.
- **Open for upstream.** Whether the operation is a `ginux` command or a
  documented `ginux-image` entry point.

### U2. `delete`, and an instance that costs little to create — Gate 25 follow-up and Gate 26 (layout)

- **Need.** (a) A public way to remove a stopped instance and its state.
  (b) Creating many instances from one cached image without expanding the
  whole image each time.
- **Observed.** Gate 25 lists `delete` as outside its scope; there is no
  command. `import_instance` expands the root with `materialize_image` for
  every new name. Names are global under `/var/lib/ginux/instances`.
- **Wanted.** (a) `ginux delete NAME`: refused while running, atomic, removes
  recipe, deployment state, root, logs; idempotent error codes
  (`not found`). (b) Either a clone (reflink where available) of an already
  expanded base root, or a base-plus-delta layout, chosen by upstream, with
  the same isolation between instances that Gate 17 proved.
- **Non-goals.** No change to the single-writer-per-instance rule.

### U3. Private, per-caller state — Gate 26 (layout) and Gate 25 (privilege)

- **Need.** A supported way for a caller to keep its instances apart from
  other callers' (a separate state root and run root), without root-owned
  global names.
- **Observed.** `GINUX_STATE_ROOT` and `GINUX_RUN_ROOT` exist in the helper
  and the CLI always goes through `sudo`; Gate 26 lists installing a broad
  sudoers rule as a non-goal. A caller that is already root works; a caller that is not cannot
  isolate.
- **Wanted.** Decide and document the supported ways: a documented state-root
  option on the CLI, or an explicit statement that callers must be root and
  use unique instance names. Either answer lets a caller plan.
- **Why separate from U2.** It decides naming and cleanup responsibilities for
  every other requirement.

### U4. Run to completion with an outcome — Gate 25 workflow (`start`, `status`)

- **Need.** Start an instance, wait for it to end, and get how it ended; tie
  the instance's life to the caller.
- **Observed.** `start` detaches and returns; `exit.status` exists only under
  `/run/ginux/instances/NAME` and is removed at the next `start`; `status`
  prints no outcome; if the caller dies the instance keeps running; `stop` is
  graceful `RTMIN+4`, then TERM, KILL (60 s+).
- **Wanted.** (a) `ginux start NAME --wait`: foreground, returns the
  workload's exit code (or `128+signal`, as the runtime does), and tears the
  instance down if its caller dies or is signalled. (b) The outcome
  (`exited`/`signaled`/`supervisor-error`, code, signal) readable after the
  fact from `status`, in a stable form (see U8), until the next start or
  delete. (c) A bounded way to force-stop that does not depend on the
  workload handling `RTMIN+4`.
- **Evidence available.** `launch --result-file` already writes exactly this
  record upstream (`README.md`, "Internal configured native launch
  diagnostic"); the request is to expose it through the lifecycle.

### U5. A non-terminal channel to the workload — Gate 25 workflow (`start`, `attach`)

- **Need.** None (decision D6, 2026-11-22: Recipe uses files; D1 removed the
  checkpoint acknowledgements). Kept for numbering, not filed. What it asked
  for was a bidirectional, binary-safe,
  framed-or-raw byte channel between the caller and the workload, separate
  from the console, plus separate stdout and stderr streams.
- **Observed.** The workload gets a PTY from `script(1)`; stdout and stderr
  merge into `console.log`; a program that writes binary frames over a PTY
  gets echo and line-discipline changes.
- **Wanted.** An option on `start` (for example `--stdio`) that connects the
  workload's descriptors 0-2 to caller-supplied pipes or a per-instance
  endpoint, with no terminal. Reading it needs no new authority for the
  workload.
- **Not filed.** Recipe moves to files (section 3.1, D6) and `checkpoint` is
  retired (D1), so no live exchange is left.

### U6. Explicit result export — Gate 25 workflow (new command) with Gate 31 dependency

- **Need.** Take chosen paths out of a stopped instance as one validated
  stream.
- **Observed.** `ginux export NAME` prints the **recipe**. The instance
  files are `instances/NAME/root`, a private tree owned by `65534`; reading it
  from outside means following paths an adversary controlled, as root.
- **Wanted.** A command (name at upstream's choice) that writes the contents
  of named absolute directories of a **stopped** instance to a tar stream:
  regular files, directories and symbolic links only; no hardlinks across the
  root, no device nodes, special modes cleared or reported; no host path
  followed outside the root (descriptor-relative, no `..`); byte, member-count
  and path-length bounds with a distinct error when exceeded; deterministic
  member order; failure leaves nothing partial in the destination.
- **Why Gate 31 matters.** This is the same trust problem as Gate 31's
  "paths, descriptors and races cannot escape"; upstream may prefer to stage
  it with the filesystem lane. Say so in the issue and let Greg place it.
- **Source material.** The fork's `storage_export.rs` and the archive format
  notes in `docs/archive-import.md`.

### U7. Read-only host volumes — Gate 31 (external volume stage)

- **Need.** Make a host directory visible read-only inside the instance, and
  nothing else of the host.
- **Observed.** `volumes.assignments = ["/srv/x:/data:ro"]` is accepted and
  normalized (`config/instance-gate9.75.toml`), protected virtual-tree targets
  are rejected, but nothing serves it; `launch` rejects a non-empty list; Gate
  31 excludes it.
- **Wanted.** When upstream reaches the volume stage of #290: the `:ro` form
  served through the same mediation as every other filesystem call, no
  host-path disclosure, no write, no `..`, no symlink escape. The `:rw` form
  is not requested.
- **Alternative if upstream declines.** Stage inputs by writing them into the
  instance root before `start` (private state; only workable if U3 and U2
  are answered).

### U8. Machine-readable diagnostics and a documented digest contract — Gate 25 (recipe diagnostics)

- **Need.** `validate`, `inspect`, `explain` and the audit records in a form a
  program can read, and a statement of what makes a contract digest change.
- **Observed.** Text output; no versioned schema; persisted contracts can
  fail with a digest mismatch after a catalog change, with migration reserved.
- **Wanted.** A `--format json` (or an equivalent versioned key=value
  specification) on the three commands with the same fields as today
  (`instance`, `catalog`, `digest`, `operations`, rule `source` and `index`),
  and the same for the bounded denial summary. A paragraph in
  `docs/configurable-contract.md` saying which changes alter the digest.
- **Non-goals.** No change to the catalog or to digests.

### U9. Bounds on writable instance storage — Gate 31 (storage), Gate 26 (layout)

- **Need.** An upper bound, in bytes and in objects, on what an instance may
  write to its filesystem.
- **Observed.** `[storage]` has `assignments` only; a runaway workload fills
  the host filesystem under `/var/lib/ginux/instances/NAME/root`.
- **Wanted.** `storage.max_bytes` and `storage.max_objects` (names at
  upstream's choice) enforced at write, with `ENOSPC`/`EDQUOT`-style errors
  inside the instance and an audit record.
- **Source material.** Fork `launch_storage.rs` limits and
  `scripts/test-launch-storage.py`.

### U10. Snapshot and restore of a running instance — DELETED (decision D1)

The owner retired mid-run `checkpoint` on 2026-11-22, so Recipe no longer needs
a consistent snapshot of a running instance or a start from one. This
requirement is not filed and is kept here only so the numbering of U1-U11
stays stable. Fork `checkpoint_control.rs` has no consumer in Recipe after
Stage 2; `storage_restore.rs` still has one, `--restore-file`, which seeds kept
downloads and the target view into a run.

### U11. Destination-level network grants and bounded usage — Gate 29 / network lane

- **Need.** Grant an instance specific destination addresses (numeric
  `IP:port`) rather than a port for all destinations; cap connections and
  bytes; get a bounded summary of what was attempted.
- **Observed.** `[contract.network]` takes `tcp:PORT`, `udp:PORT`, `*`; Gate
  29 makes every destination-bearing send obey that contract; Gate 46 makes an
  empty list deny. Neither adds a destination dimension. The fork has
  `network.endpoints`, `max_connections`, `max_bytes` and a final
  `network-summary` line.
- **Wanted.** An optional destination constraint on outbound rules, checked
  where Gate 29 already checks the destination, and the two caps. Order of
  work follows the network lane Greg has reserved; file after Gate 29 and 46
  are accepted so it cannot conflict.

### Questions, not requirements

- **Q1 (Gate 26).** Is non-Rocky-10 host support (Fedora, Ubuntu) a thing
  upstream will carry, or is it a downstream packaging matter? Recipe's
  standalone release installs on Fedora 44 and Ubuntu 24.04
  (`docs/recipe-standalone-release.md`). The owner decided (D4) that Recipe
  ships the Ginux binaries it uses, so the answer no longer blocks the plan;
  it only says whether upstream will one day carry that packaging.
- **Q2 (Gate 11).** Is a run through the lifecycle with a non-init worker in
  scope for the 4% budget measurement? The owner wants the budget to hold
  inside Recipe too; today Gate 11 measures sysbench in instance modes only.

## 6. Staged plan

**Stage 2 status (2026-11-22).** Written in the working tree of
`tools/recipe` (and `distro/builder`) and not committed; unverified: nothing
has been built or run, and verification is Stage 4. Done:

- `checkpoint`, `save`, `saved` and `reached` are removed from the format, the
  helpers, the worker and the coordinator, with the `Checkpoint`/`Ack`
  operations, `--control-fd`, the snapshot control channel and `src/core/saved.rs`;
  an interrupted step restarts from its beginning. The kept-state record is
  version 3 and `recipe state` `on_disk` lost `unfinished`.
- The stdio frame protocol (`src/worker/protocol.rs`) is gone: the request is
  the staged input `.recipe-request.json`, the result is the file
  `/work/.recipe-result.json`, diagnostics and progress are JSON lines on the
  worker's stdout (`src/worker/records.rs`, version 14), and a host-mode worker
  learns its root from `RECIPE_HOST_ROOT`. `RECIPE_FRAME_TIMEOUT_SECS` is now
  `RECIPE_OUTPUT_TIMEOUT_SECS`.
- `src/launch.rs` is split into `src/launch/{mod,configured,worker_io}.rs`;
  `configured::run` is the single entry point of the configured launch.
- The pins (catalog and both digests) are one table, `RuntimeKind::pins()` in
  `src/needs/contract.rs`.
- The kernel recipe `linux` in `distro/builder/os-recipes` is split into
  `linux-source`, `linux-compile` and `linux` (D5).
- `--restore-file` was kept: it still seeds kept downloads and the target view
  (the plan above had listed it for removal).

Still open from Stage 2: the image identity of D2 (the image producer writing an
OCI layout and the build identity using its manifest digest), which waits for
U1's answer. Nothing else.

Verification follows the repository rule: write everything first, then
verify all of it together. No build or run is part of any stage until the
cutover is complete.

**Stage 0 — decisions (now).** D1 (retire `checkpoint`; U10 is
deleted), D2 (offline import, U1), D5 (split `linux.rhai`) and D6 (files; U5 is
not filed) are decided. D3 (interim fork commands, Stage 3) and D4 (Recipe ships
Ginux for Fedora and Ubuntu) are decided too. All of section 7 is answered.

**Stage 1 — file the requirements (upstream, one at a time, owner releases
each).** Order, with the reason:

1. U4 and U6, because without a wait with an outcome and an export there is
   no lifecycle run at all; Greg already named export.
2. U2 and U3, because they decide how many instances exist and who cleans up.
3. U1, because it decides how the compiler image gets in (D2 chose offline
   import).
4. U8, small, independent, and it lets the digest pins be re-derived.
5. U7 and U9 when Gate 31's volume stage is reachable; U11 after Gates 29 and
   46 are accepted; U5 and U10 are not filed (D6, D1).

Each is filed as its own issue or gate request against the named workflow, not
as one umbrella, with the fork's tests offered as source material. Posting
needs the owner's go.

**Stage 2 — Recipe-side preparation, independent of upstream.** All of it
changes Recipe only and keeps `ginux launch` working until the cutover:

- (Done, see the status above.) Split `src/launch.rs` as described in section 4 so the part that decides
  and publishes is separate from the part that runs `ginux`. The adapter
  becomes one module with one entry point.
- (Done.) D6 chose files: change `src/worker/protocol.rs` and
  `src/worker/run.rs` from stdio frames to request/result files, and update
  `host.rs` in the same step (it shares the protocol).
- (Done, except as noted.) D1 retires `checkpoint`, one step with no dual form: remove `checkpoint`,
  `save`, `saved` and `reached` from the format (`src/core/saved.rs`,
  `src/helpers/mod.rs`, `docs/recipe-format.md`, `WRITING_RECIPES.md`, the man
  pages, README, CLAUDE.md, HELPERS_AUDIT.md); the checkpoint snapshot parts of
  `retained.rs`, `state.rs`, `launch.rs`, `ginux.rs`, `host.rs`,
  `worker/protocol.rs` (`Checkpoint`/`Ack` ops) and `worker/run.rs`; the
  `--control-fd` code (`--restore-file` stays, see the status above); the callers: `linux.rhai`, split into source, compile and install recipes (D5, decided), and
  the e2e scenarios `failures.rs`, `managed_cancellation.rs`,
  `managed_state.rs`, `managed_storage.rs`, which are rewritten to the step
  boundary form in the same change.
- (Done.) Move the digest and catalog pins into a table keyed by runtime kind so the
  cutover edits one place.

**Stage 3 — interim fork support (D3 allows it, 2026-11-22; written 2026-11-23, see status below).** While upstream
decides, the fork (`LevitateOS/ginux:main`, not upstream) carries additive
public-lifecycle commands for what a lifecycle run needs first: `start --wait`
with an outcome (U4), result export (U6), `delete` and private per-run state
(U2, U3), and offline image import (U1). Each is shaped exactly like its draft,
so upstream's version can replace it without a change in Recipe. U7, U9 and U11
are added only if the cutover needs them before upstream answers. The fork then
runs Recipe on the public lifecycle with downstream copies of the missing
pieces, and each copy is dropped when upstream's lands. Nothing is posted
upstream by this stage (AGENTS.md: no upstream PRs or comments without the
owner's go).

**Stage 3 status.** Written and committed in the fork `tools/ginux` as four
commits on top of `e3b71ac0`: `9f903efc` (`ginux-image export-tree` and
`import-layout`), `002e2e8b` (runtime `native ... configured --result-file`),
`f358cb84` (helper: `start --wait`, `status --format json`, `delete`,
`extract`, `import-image`, private `--state-root`/`--run-root`, per-instance
lock, reflink base roots) and `3e3e81f3` (CLI forms, docs, Make targets, CI).
They sit on a detached HEAD and are NOT pushed. The full design is
`docs/ginux-interim-lifecycle-design.md` (names: `extract`, `import-image`,
`--state-root`, `--run-root`; the outcome record is the version-1
`launch --result-file` shape). Mapping to the drafts: U4 = `start --wait` +
`status --format json` + the runtime result file; U6 = `extract`; U2 = `delete`
and reflink base roots; U3 = `--state-root`/`--run-root`; U1 = `import-image`.
U7, U9 and U11 are not implemented. The open questions Q1-Q12 of the design
were taken at their recommended answers, in particular Q3 (reflink base roots,
which sit in tension with Gate 26's "no redundant assembled rootfs"; the owner
may drop them) and Q4 (a small additive change in upstream's `ginux-init`).
Verified: Rust unit tests (`ginux-cli` 29, `ginux-oci` 32), clippy
`-D warnings`, `cargo fmt --check`, both inventory scripts,
`scripts/test-cli.sh`, syntax checks, and two independent read-only reviews
(security; cross-package consistency) whose confirmed findings were fixed. NOT
run: `scripts/test-public-lifecycle.py` and `scripts/test-native-result.py`
(they need root and a real host), so the helper, the walker under root, the
runtime record and the whole lifecycle are untested end to end. Known open
points: binary stdin/stdout through `sudo` (needed by `extract` and
`import-image`) is unproven; under real sudo a relayed signal to `start --wait`
ends the CLI after 1 s (sudo ignores signals from its own process group) and
the helper tears down through its caller-death path, so callers read the
outcome with `status --format json`; a process that can signal uid 65534 can
stall an export that holds the instance lock; the end-to-end tests rely on
fixture behaviour that was only read, not run.

**Stage 3 addendum: `inject` (D7, 2026-11-23).** Written and committed in the
fork as `aa403109` (image tool `inject-tree`), `4911e52c` (helper verb) and
`8f4d53ba` (CLI `ginux inject NAME --input FILE --into PATH`), on top of
`3e3e81f3`; not pushed. It is the mirror of `extract`. Behaviour: a tar read
with the caller's rights is written below a directory of a STOPPED instance
root, creating the directory and missing parents; only regular files,
directories and symlinks; never overwrites (an existing path is a conflict,
exit 65, except that an existing real directory is accepted and left completely
untouched); every component goes through `openat2` beneath/no-symlinks;
privileges are dropped to uid 65534 first; setuid bits are cleared; member
names must be relative without a leading `./` (so a producer must not use
`tar -C dir .`); bounds on bytes, members, path length, depth and retained name
memory (exit 76); data after the end-of-archive marker must be zero. It is NOT
atomic: on failure the instance is deleted and recreated. The injector holds
the per-instance lock until it exits. Verified like the rest (Rust unit tests:
`ginux-cli` 34, `ginux-oci` 52; clippy; fmt; inventory scripts; `test-cli.sh`;
an independent security review whose findings were fixed). NOT run: the root
end-to-end script `scripts/test-public-lifecycle.py` (it now has inject cases,
including through the CLI). What Stage 4 must do with it: stage the worker
binary, the request file and the inputs as one tar with relative names and
inject it at `/` (or `/inputs`) of a created instance before `start --wait`,
then `extract` the result path.

**A gap Stage 3 did not cover, now covered by `inject` (Stage 4 prerequisite).**
Nothing in the public lifecycle puts files INTO an instance. Recipe's worker
binary, the request file and the staged inputs (`/inputs/...`) have to reach
the instance root, but `volumes` are not served (N12/U7), the state root must
be root-owned (so Recipe, running unprivileged, cannot write into
`instances/NAME/root`), and no helper verb copied files in. This was decision
D7 in section 7, decided 2026-11-23: the interim fork verb `inject` (above).

**Stage 4 — the cutover (one change set).** When every need Recipe depends on
is available through the public lifecycle, whether from upstream or from
Stage 3:

- Replace the adapter; delete everything listed in section 4 as deleted; bump
  `GINUX_BEHAVIOUR`; change `distro/builder` and `scripts/` in the same set.
- Re-derive `CATALOG` and both digests from the upstream `validate` output and
  review them against the catalog as the file's comment already requires.
- Verify all together: the e2e suites (`tests/e2e/*`, which need a real Ginux),
  the recipe catalog installs, one OS part build, and the standalone install
  on Fedora and Ubuntu (D4: Recipe ships Ginux).

**Stage 4 status (written 2026-11-23; unverified, never run).** The cutover is
written as one change set in the working trees of `tools/recipe`, `distro/builder`,
`xtask`, `tools/recipe/scripts` and the fork helper, against the design
[`ginux-cutover-design.md`](ginux-cutover-design.md) (a fixed record; its section 9
names the work packages). Nothing has been built, run or checked beyond
compilation: the first Recipe run on the public lifecycle is also the first real
run of the fork's interim verbs (Stage 3 status above), and `ginux launch` has no
caller left in this repository. The privilege model is the owner's option A (a
sudoers rule for the administrators' group, `recipe runtime stage --sudoers
GROUP`); the old user-namespace path and AppArmor profile are gone. **Hardening
after review (decisions taken on 2026-11-23).** A review found that a rule naming
`ginux-helper` gave every group member passwordless root through any helper verb
(`prepare-root /`). The owner decided two things: (1) the rule names only a
restricted root-owned Recipe guard, `/usr/libexec/recipe/recipe-ginux-guard`
(shipped by `runtime stage --recipe-guard`, installed by the standalone installer,
reported by `runtime status`), which allows only Recipe's instance verbs on
instances named `recipe-<uid>-*`, sanitises the imported instance config
(`--check-config`), clears the environment and then runs the helper; (2) instance
directories are root-private (0700) and the helper streams the console output to
the caller, so the adapter no longer reads files out of the instance. What
remains is a passwordless sandbox for group members, not a passwordless root; the
exposure left on a shared host is in `docs/runtime-limits.md`. Like the rest of
this change set, the guard and the private instances are written, compiled and
unrun; their e2e cases that need no root are in `tests/e2e/guard.rs`.
**Review rounds of 2026-11-23 and the image allowlist.** Independent review
rounds of the guard on 2026-11-23 led to these changes: the guard takes the
caller only from `SUDO_UID`/`SUDO_GID`, reads the imported config with the
caller's identity and re-renders it from validated fields, and for a caller
other than root accepts an image only if its exact reference is in the
root-owned allowlist `/usr/libexec/recipe/runtime-images` (written by `recipe
runtime stage`, `sudo recipe runtime fetch` and the installer); `import-image`
is not available through the guard (root imports with `sudo recipe runtime
fetch`, which the installer runs), so a non-root run without an imported image
stops and says to run it, and the e2e harness runs that command once per test
process when it is not root. The reviews also showed what the guard cannot fix,
because it sits in code the helper and the image tool run as root: **R13**
`apply_whiteout`/`remove_path` in the fork's `ginux-oci` follow symlinks inside
the image (a layer with `h -> /` and a whiteout under `h` deletes `/`); **R14**
the helper's `prepare_root` follows symlinks inside the image; **R15** every
user's workload runs as host uid 65534, so workloads of different users can
signal each other's `inject-tree`/`export-tree` processes (stalling the instance
lock), and a workload that defeats Ginux's mediation could reach other users'
running workloads. R13 and R14 are bugs in upstream's code and are not fixed;
the allowlist keeps untrusted images off the passwordless path, but root callers
and the OS builder still pass images through that code. Next step: fix in the
fork, report upstream. There are also no per-uid instance, disk or cache caps,
and the byte, extract and free-space bounds are Recipe's, enforced in the
client, so a member who calls the guard directly bypasses them. The catalog
(2) and both build-base digests in `RuntimeKind::PublicLifecycle` are
**placeholders** (`PLACEHOLDER-DERIVE-IN-VERIFICATION`): every Ginux validation
fails by design until they are derived. What must happen in verification, in
this order (design section 6, then the single pass):

1. Derive the catalog and the two digests with a built fork binary (`cargo build
   -p ginux-cli --release` at the pin; `recipe inspect --json` on one recipe
   without and one with `NEEDS.network` reports the digest Ginux derived;
   cross-check with `ginux validate`; review with `ginux inspect` and `ginux
   explain` for every selector and for the seven operations catalog 2 lacks;
   write the values and the review entry into the pins row). Never type a digest
   by hand.
2. Run the e2e suites (`tests/e2e/*`, `tests/library_backend.rs`,
   `tests/image_producer.rs`) as root with a real Ginux, the five Ginux files
   installed root-owned and the image layout imported (the preconditions are in
   the header of `tests/e2e.rs`; the tests were adapted, not run).
3. Run a recipe catalog install (for example ripgrep) end to end.
4. Build one OS part (the builder's root-owned Ginux wrapper, `RECIPE_IMAGE[_LAYOUT]`).
5. Install the standalone release on Fedora 44 and on Ubuntu 24.04, with the
   sudoers rule, and run a recipe on each.

Accepted regressions (decided; stated plainly in `docs/runtime-limits.md`, the
README and the release notes): no storage limit inside a run (only the archive
taken out is bounded, plus a free-space watchdog); network grants are per port
on the shared host network (a program can reach any address on a granted port,
with no connection or byte cap; only Recipe's own downloads are held to the
declared `HOST:PORT` and the byte cap); live system roots and the offline cache
are copied, not bound (the cache only by declared pin); no hard links through
the instance; Ginux runs need root or the sudoers rule. Risks of the design
(section 10), one line each: R1 the two safety regressions above ship until
upstream U9/U11 exist; R2 catalog 2 lacks `lstat`, `readv`, `pipe`, `nanosleep`,
`sendfile`, `link`, `rt_sigpending` and builds may break on them; R3 whether
`[contract.filesystem]` and `[access]` are enforced for a non-systemd init is
read from docs, not proven; R4 AcornOS needs GNU coreutils, util-linux, gawk,
procps and sudo for the helper, against "small and strict"; R5 per-run cost on
file systems without reflinks may dominate OS builds; R6 console readability
depends on the fork change F2 (`chmod 0644` on the logs), without it a umask of
077 yields no progress lines; R7 `console.log` and `audit.log` grow unbounded
during a run; R8 offline builds lose the cache for downloads computed at run
time; R9 SELinux on Fedora 44 and `sudo` `use_pty` with binary stdin/stdout are
unproven; R10 `recipe runtime remove` cannot free the imported image or base
roots; R11 the CLI and image tool are not yet known to build as static musl;
R12 every verb of the interim lifecycle is unrun end to end; R13 `apply_whiteout`/`remove_path` follow symlinks in an image as root (a hostile image can delete host files); R14 the helper's `prepare_root` follows symlinks in an image as root; R15 all users' workloads share host uid 65534 (they can signal each other's `inject-tree`/`export-tree` processes, and a workload that defeats Ginux's mediation could reach others' workloads); R13 and R14 are unfixed bugs in upstream's code, next step: fix in the fork, report upstream.

**Stage 5 — what is left on the fork.** After the cutover nothing in this
repository calls `ginux launch`. The fork's configured launch then has no
consumer here and is kept, or dropped, as the owner decides; upstream's
direction (absorbing the abilities "step by step inside its own authorities")
means it only shrinks.

### What stayed on the fork's configured launch until Stage 4

(Historical: Stage 4 above replaced all of it in one change set; the list is
what the cutover deleted from Recipe.)

Everything in section 1: `ginux launch` with `--export-file`,
`--restore-file`, owned `memory:` storage and its limits, archive import,
catalog 1002 with the two digest pins, the `configured-*-audit` fields, the
launcher-copy in `recipe runtime stage` and the builder's launcher build. No
Recipe code changes to those until Stage 4, apart from the Stage 2
refactors, which do not change behaviour. Any new fork feature Recipe needs
before then is added to the fork's configured launch (as done so far), not to
the public lifecycle.

## 7. Decisions for the owner

- **D1. Keep mid-run `checkpoint`, or retire it?** DECIDED 2026-11-22 by the
  owner: retire it. N15, U10 and the control channel are gone; only a long single
  `build` loses, and the one such recipe is `linux.rhai` (D5).
- **D2. Where does the compiler image come from?** DECIDED 2026-11-22 by the
  owner: ask for U1 (offline import); no registry. Recipe's image producer
  writes an OCI image layout and the build identity uses its manifest digest
  (Stage 2, with U1's answer). Options as weighed: publish it as a proper OCI
  image to a registry and use upstream's fetch, or ask for U1 (offline
  import). A registry removes U1 but puts a network fetch and a digest of an
  OCI manifest, not of Recipe's tar, into the identity of every build.
  Recommendation: U1, because builds must run offline once the image is
  present.
- **D3. May the fork carry interim public-lifecycle commands?** DECIDED
  2026-11-22 by the owner: yes (Stage 3). That gives
  Recipe a path before upstream decides, at the cost of fork-only commands
  to retire later. It matches "keep Recipe-required changes available in our
  repository while upstream takes its own direction" (AGENTS.md).
- **D5. How does the kernel build resume without `checkpoint`?** DECIDED
  2026-11-22 by the owner: split `linux.rhai` into dependent recipes (source,
  compile, install); the split lands in Stage 2 with the removal of
  `checkpoint`. (Follows from D1.) `distro/builder/os-recipes/linux.rhai` checkpoints after the source fetch
  and after the compile, so an interrupted kernel build skips what is done.
  Options: split it into dependent recipes (source, compile, install), each a
  step with a retained output, or accept that an interrupted build restarts.
  Recommendation: split, because the compile takes hours and steps are exactly
  the resumability Recipe already has. Stage 2 does it in the same change as
  the removal. Done in the working tree (see the Stage 2 status in section 6):
  `linux-source`, `linux-compile` and `linux`.
- **D6. Replace the stdio frame protocol with files (section 3.1)?** DECIDED
  2026-11-22 by the owner: files. With `checkpoint` gone the protocol is one
  request in and one result out; files remove U5 and the PTY problem.
- **D4. Does Recipe ship Ginux for other distributions (Q1)?** DECIDED
  2026-11-22 by the owner: yes, ship it. Upstream's RPM is Rocky 10 only, so
  downstream packaging of the Ginux CLI and helper for Fedora 44 and Ubuntu
  24.04 is Recipe's own work: `recipe runtime stage` and the standalone
  installer carry them (with the host confinement profile each distribution
  needs, as the AppArmor profile does today), built from the fork while it
  carries the interim commands (Stage 3) and from upstream afterwards. It
  depends on U3 (private per-caller state), since a global root-owned
  `/var/lib/ginux` reached through `sudo` is not what a standalone user
  install should need.
- **D7. How do Recipe's worker, request file and staged inputs get into an
  instance?** DECIDED 2026-11-23 by the owner: option (a), an interim fork verb
  `inject` (the mirror of `extract`), written and committed in the fork (see
  the Stage 3 addendum in section 6). The public lifecycle has no way to put
  files into an instance. Options were: (a) an interim fork verb that injects a
  tar of inputs into a created instance, the mirror of `extract`; (b) wrap the
  inputs as an extra layer of the imported image layout (works with
  `import-image`, but copies the inputs on every run); (c) wait for U7
  (volumes). (a) was the smallest and mirrors `extract`.

## 8. Risks

- U6 and U7 sit in the filesystem lane Greg reserved (#290). He may stage
  them far behind Recipe's schedule. The plan's only hedge is Stage 3.
- Upstream's instance directory is a host directory owned by the unprivileged host
  identity `65534` and expanded per instance; if U2 is answered with "create
  is expensive", Recipe's per-invocation roots become the dominant cost of a
  build part. Measure once, at the end, not per change.
- Digest pins re-derived for catalog 2 will change every saved-state identity,
  as every earlier pin move has.
- The result file now lives in the writable `/work`, so a program the recipe
  starts as the same uid could in principle write a forged result. Mitigations
  in the code: the worker writes the result last, after the recipe's programs
  have finished, replacing whatever is there; the result must name the run's
  invocation id (a result left by an earlier run is refused);
  and the coordinator validates every output it names against the recipe's
  declarations before publishing. The Ginux runtime also exports `work` and
  `out` only after a clean root exit, so a result exists only if the worker
  exited 0.
- Ginux upstream marks the system research software without a production
  claim; the plan does not change what Recipe claims about the sandbox.
