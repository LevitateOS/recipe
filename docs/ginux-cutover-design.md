# Stage 4 design: Recipe on the public Ginux lifecycle (the cutover)

Read-only design of 2026-11-23. Nothing was edited, built or run. Code read at
`tools/recipe` HEAD `902790e` plus the Stage 2 working tree, `tools/ginux` HEAD
`8f4d53ba`, and the superproject's `distro/builder`, `xtask`, `.github`.
"Helper" = `tools/ginux/scripts/ginux-host-instance-helper.sh`; "CLI" =
`tools/ginux/crates/ginux-cli/src/lib.rs`; "migration doc" =
`tools/recipe/docs/ginux-public-lifecycle-migration.md`; "interim design" =
`tools/recipe/docs/ginux-interim-lifecycle-design.md`.

Rules carried in: one change set, no dual runtime, no backwards compatibility,
no unit tests, no build between changes, Recipe never calls a model. Items
marked **OWNER** are decisions; **NEEDS BINARY** is the one step that needs a
built fork binary (section 6).

---

## 0. Findings that change the plan (read first)

1. **`create`/`import` refuses an init that is not yet in the root.** The
   helper checks `root_path_is_executable "$staging/root" "$configured_init"`
   right after expanding the image (helper `import_instance`, line 807) and
   exits 65. Recipe's init is `/inputs/recipe-worker` (`src/domain.rs`
   `WORKER`, `src/inputs/config.rs` `for_invocation`), which only `inject`
   can put there, after `create`. As written, every Recipe instance fails at
   create. The migration doc's Stage 4 text ("inject … before `start --wait`")
   does not notice this. Fix: section 1.2 (F1, a two-line fork change).
2. **Workload stderr goes to `audit.log`, not `console.log`.** `run_runtime`
   execs the runtime with `2>>"$directory/audit.log"` (helper line 1381); the
   runtime keeps the inherited stdout and stderr ("Stdout and stderr keep their
   actual inherited file descriptions", `crates/ginux-init/src/main.rs:5870`).
   Only fd 0/1 are the PTY of `script(1)` (helper line 1257). Migration doc 2.2
   says both land in `console.log`; the code says otherwise. Good for Recipe:
   the worker's stdout (line records only, `src/worker/run.rs` header) is on
   the PTY, its stderr and all runtime audit lines are in `audit.log`.
3. **Runtime audit lines go to fd 2, not the console.** Inside
   `supervise_native` a local `write_all` closure writes to fd 2 with an
   8192-event budget (`main.rs:5693-5737`), so `GINUX_NATIVE_AUDIT: …` lines
   (`main.rs:7971-7988`) land in `audit.log`. Only
   `print_instance_start_summary` (`main.rs:11537`, module `write_all` → fd 1)
   writes `GINUX_INSTANCE_START: …` to the console before the workload runs.
   The interim design's finding 2 (runtime end lines on fd 1) is true only of
   that summary.
4. **`ginux validate` prints the compiled-in catalog, not the config's.**
   `validate_recipe` prints `ginux_contract::CATALOG_VERSION` (always `2`)
   whatever `[contract] catalog` says (CLI `validate_recipe`). Recipe must
   check its own rendered catalog and trust only `digest=`.
5. **The contract digest does not cover filesystem or network lists.**
   `InstanceConfig::effective_contract` (`crates/ginux-instance/src/lib.rs:1095-1127`)
   feeds `[contract] allow/deny` and `[contract.syscalls] allow/deny` (plus the
   legacy read/write/... lists) to `resolve_recipe_catalog`; `filesystem_*`,
   `egress_allow`, `process_*` are not inputs. So two base digests (network,
   offline) still suffice, and per-port outbound lists do not multiply them.
6. **Catalog 2 lacks seven operations the fork added in 1002.**
   `CONFIGURED_OPERATIONS` (`crates/ginux-contract/src/lib.rs:191-242`) adds
   `lstat`, `readv`, `pipe`, `nanosleep`, `sendfile`, `link`, `rt_sigpending`
   only under catalog 1002 (`catalog_operations`, line 2203). Under catalog 2
   `syscall.*` does not cover them. Whether the native configured path then
   refuses them is unverified (risk R2).
7. **Native mode 3 runs the workload as virtual root unless `[access]` says
   otherwise** (`main.rs:5835-5846`; default `access_uid: 0`,
   `ginux-instance/src/lib.rs:491`). The fork's launch ran it as virtual
   1000 (`Credentials::unprivileged(1000, 1000)`, same lines); Recipe's
   `domain::UID` is 1000. Recipe must render `[access] uid = 1000, gid = 1000`.
8. **Network: native mode 3 serves only `backend = "shared"`**
   (`main.rs:7305-7306`) checked against `[contract.network]` `tcp:PORT`
   lists; `mediated`, `endpoints`, `max_connections`, `max_bytes` belong to
   the fork's launch lane (`network_hooks.rs:63`).
9. **Instance names are at most 64 bytes** (`IDENTIFIER_BYTES`,
   `ginux-instance/src/lib.rs:146`; the helper sets `instance.id` to the name,
   helper line 755). Recipe's invocation id can be 64 bytes alone
   (`src/inputs/mod.rs` `PreparedInvocation::invocation_id`), so it cannot be
   the name.
10. **Everything privileged is root via `sudo`.** The CLI always spawns
    `sudo HELPER` (CLI `Helper::helper_command`), and the helper redirects
    nothing through the environment (`sudo` resets it), so it runs
    `/usr/libexec/ginux/ginux-runtime` and `/usr/libexec/ginux/ginux-image`
    (helper lines 6-7). The ordinary-user path that works today (the fork's
    launch in a user namespace, `docs/recipe-standalone-release.md`, Fedora
    and Ubuntu runs of 2026-11-13) is gone. Section 3.

---

## 1. The new runtime half

### 1.1 Shape

`src/launch/configured.rs` is deleted and replaced by `src/launch/lifecycle.rs`
with the same single entry point and the same types, so `run_domain_inner`
(`src/launch/mod.rs:252`) changes only its call and two fields:

```rust
pub(crate) struct LaunchSpec<'a> {
    pub prepared: &'a PreparedInvocation,
    pub request: WorkerRequest,
    pub ginux: &'a crate::ginux::Ginux,        // was `launcher: &Path`
    pub diagnostics: &'a InvocationLog,
    pub worker_digest: &'a str,
    pub run_dir: &'a TemporaryDirectory,
    pub restore: bool,
    pub restore_path: &'a Path,
    pub export_path: &'a Path,
    pub timeout: Duration,
    pub progress: &'a mut Option<ProgressFile>,
}                                              // `image: &mut Option<VerifiedImage>` goes
pub(crate) struct RunOutcome { result, launcher_code /* CLI exit */, outcome, code, log }
pub(crate) fn run(spec: LaunchSpec<'_>) -> Result<RunOutcome>;
```

`src/ginux.rs` is rewritten as the only module that spawns the `ginux` CLI
(struct `Ginux { cli: PathBuf, state_root: String, run_root: String }`, one
method per verb below, the status record and `Outcome`). Every call is
`CLI --state-root S --run-root R VERB …` (CLI `Cli.state_root/run_root`,
`Helper::new`), with stdin `/dev/null` except where noted, stdout and stderr
captured with a 256 KiB cap and a per-verb deadline through `src/process.rs`.

### 1.2 One run, step by step (`lifecycle::run`)

0. **Name.** `recipe-<uid>-<pid>-<8 hex random>` (≤ 39 bytes; finding 9).
   The invocation log records `{"instance": NAME}` beside the invocation id
   (`InvocationLog::authority`), so a reader can map one to the other.
1. **Image present.** `Ginux::ensure_image(reference, layout)` (section 5.4):
   if `STATE/cache/oci/blobs/sha256/<manifest hex>` exists (the cache
   directories are 0755 root, readable by anyone; `packaging/rpm/ginux.tmpfiles`
   and helper `import_image`), nothing; else `ginux import-image LAYOUT`
   (stdin of the helper is the CLI's stream, CLI `import_image`) and compare
   its stdout `sha256:<hex>\n` with the reference's digest; a mismatch is a
   `Startup` failure. Serialized per digest with a lock file in Recipe's own
   cache dir so parallel runs import once.
2. **Create.** Write `run_dir/instance.toml` (section 1.3), then
   `ginux import NAME run_dir/instance.toml` (helper `import_instance`; the
   helper reads the file as root, helper line 713, so it may live in the
   caller's temp dir). Exit 0 → go on; 73 (exists) → new random suffix, once;
   anything else → `Startup` with the helper's stderr.
   **F1 (fork, prerequisite):** move the init check out of `import_instance`
   (helper line 807) into `start_instance` before the supervisor is spawned
   (after line 1117, same message, exit 65). An instance whose init is
   injected later is then creatable; a missing init still fails, at start,
   before any state change. Additive, upstream-shaped; owned by WP4.
3. **Stage (one tar).** `crate::inputs::inject_archive(prepared, &mut request,
   restore.then_some(restore_path), &run_dir.join("inject.tar"))` (WP2,
   section 2.12) writes ONE uncompressed GNU tar, members relative with no
   leading `./` (the injector refuses `./`, interim design section 7), only
   directories, regular files and symlinks, every name once, parents first:
   - `inputs/` with every staged input (`stage_inputs` rules: digests checked
     while copying, `mode | 0o444` for files), the input-modes manifest, the
     request file `.recipe-request.json`, the worker `recipe-worker` (0555,
     hashed while appended; must equal `worker_digest` when that is set, as
     `configured::run` checks today), input directories 0555;
   - `inputs/recipe-offline-cache/sha256/<hex>` for each declared download
     pin of this recipe present in the offline cache (section 2, N12);
   - `work/`, `work/build/`, `work/tools/`, `work/tmp/`, `out/` (0755: the
     owner after injection is uid 65534, the workload, so 0777 is not
     needed); `etc/hosts` (0444, `127.0.0.1 localhost`, `::1 localhost`, then
     the pinned names, as `network::install_hosts` writes today, but always);
   - when `restore`: every member of the restore tar (`Retained::restore_input`
     / `restore_download`, or the seeded target view of `seed_target`) re-emitted
     under its own name, duplicates of the skeleton directories dropped,
     hard links written as regular copies (the injector accepts no hard
     links; `seed_target` emits `EntryType::Link`, `src/inputs/root.rs:655-660`).
   Returns `{worker_digest, bytes, members}`. Then
   `ginux inject NAME --input inject.tar --into / --max-bytes B --max-members M
   --max-path 4096` with `B = bytes + 1 MiB`, `M = members + 64` (CLI
   `inject_instance`). Any non-zero exit: delete the instance (the injector is
   not atomic, interim design section 7) and fail `Startup`; 65 names a
   conflict (the image already has a path Recipe injects, see 5.2).
4. **Start and wait.** Spawn `CLI … start NAME --wait` (CLI `start_waiting`):
   own process group (`process_group(0)`), `pre_exec` sets
   `PR_SET_PDEATHSIG=SIGKILL` and checks `getppid()` as `configured::run`
   does today (lines 149-165, minus the lease descriptors and the binds).
   PDEATHSIG survives the exec of the non-setuid `ginux`; the CLI does not
   exec `sudo` for exactly this reason (interim design 2.1, "Caller binding").
   The CLI hands its own pid and start time to the helper, which polls them
   every 0.2 s (`caller_alive`, helper line 1157) and tears the instance down
   when they vanish (`teardown`, line 1189: TERM to the supervisor, ≤ 5 s,
   KILL, outcome `supervisor-error`).
   stdout/stderr of the CLI: piped; stdout must stay empty (helper prints
   nothing in wait mode), stderr goes to the capped drain
   (`worker_io::spawn_stderr_drain`) as a third log, `ginux-cli.log`.
5. **Read lines while it runs.** Section 1.5.
6. **Deadline and cancel.** Unchanged rules from `worker_io::pump_lines`: the
   execution deadline (`ExecutionOptions::timeout_secs`), the output timeout
   (`RECIPE_OUTPUT_TIMEOUT_SECS`), cancellation (`crate::cancellation::signal`).
   On any of them: `kill(-pgid, SIGTERM)`. The signal reaches the CLI and
   `sudo` from outside their process group, so `sudo` relays it to the helper
   (whose `trap teardown HUP INT TERM`, line 1222, starts teardown at once);
   the CLI's own relay is ignored by `sudo` (same group) and the CLI dies by
   the signal after 1 s (`wait.rs` `RELAY_GRACE`, `die_by`), which also trips
   `caller_alive`. Recipe waits for the CLI up to `SHUTDOWN_GRACE` (10 s,
   `worker_io.rs:54`; helper teardown is bounded at about 7.5 s), then
   `kill(-pgid, SIGKILL)`. This replaces `reap_launcher`'s group kill for the
   Ginux half; `reap_launcher` stays for `host.rs`.
7. **Outcome.** After the CLI exited: `ginux status NAME --format json`
   (helper `status_json`, line 1002): one line
   `{"version":1,"name":…,"state":"running|stopped","contract":…,"pid":…,
   "outcome":{"version":1,"outcome":"exited|signaled|supervisor-error","code":…,"signal":…,"generation":null}|null,
   "paths":{"config","deployment","console_log","audit_log","root"}}`.
   Parsed strictly (serde, `deny_unknown_fields` off only for `paths`);
   `state` must be `stopped`; `outcome: null` reads as `supervisor-error`.
   `RunOutcome.launcher_code` is the CLI's exit code (workload code, 128+n, or
   70, with 64-76 helper codes overlapping, CLI `start_waiting` comment), so
   it is recorded but never trusted alone: the status outcome is the authority,
   exactly the role `ResultRecord::read` has today.
8. **Extract.** Only when the outcome is `exited` (the worker exits 0 after
   writing its result even on failure, `src/worker/run.rs` header; any other
   outcome has no result, as the old supervisor exported only after a clean
   exit): `ginux extract NAME --output EXPORT --max-bytes A --max-members O
   --max-path 4096 /work /out`, `EXPORT = spec.export_path`,
   `A = options.archive_bound()` (`src/options.rs:135`), `O =
   storage_max_objects * 3` (the bound `result_from_archive` already uses).
   Members are root-relative (`work/…`, `out/…`, export-tree `export_path`
   emits the requested directory as `out/`), the same shape as the fork's
   export, so `worker_io::result_from_archive` (line 440, member
   `work/.recipe-result.json`), `publication::prepare` and `Retained::finish`
   read it unchanged. Exit 76 → `Storage` "outputs exceed their bound"
   (this is where `storage_max_bytes` is now enforced, after the fact,
   section 2 N13); 65 "unsupported member type" (a FIFO, socket or device left
   in `/work` or `/out`) → `Workload` naming the path; 75 → retry once.
9. **Logs.** Copy `paths.audit_log` (worker stderr, runtime audit,
   `GINUX_AUDIT_DROPPED`) into the invocation's `SUPERVISOR_LOG`
   (`crate::invocation_log::SUPERVISOR_LOG`) with the existing cap
   (`LOG_CAP_BYTES`) and tail (`LOG_TAIL_BYTES`), producing the same
   `DrainedLog`. Read before delete.
10. **Agreement.** `check_agreement` and `check_launcher_exit` move unchanged
    except that the record is the status outcome and "launcher exit" reads
    "ginux start exit". `enrich_stream_error` takes the status outcome instead
    of `peek_outcome(path)`.
11. **Delete, always.** A guard object created right after step 2 runs
    `ginux delete NAME` on every path out of `run` (success, error, panic
    unwind, cancellation). 73 (still running: teardown not finished) → poll
    `status` every 250 ms up to 15 s, then `ginux stop NAME` and retry once;
    66 → already gone. A failed delete is a warning naming the instance (the
    run's own result stands), and the sweep (1.6) retries later.

### 1.3 The instance TOML (`src/inputs/config.rs`, `InstanceConfig::render`)

Derived field by field from today's renderer (`config.rs:126-190`) and
`Contract::render` (`src/needs/contract.rs:166-173`):

```toml
schema_version = 1
[instance]
id = "<NAME>"                         # was the invocation id; helper overwrites with NAME anyway
root = "local/recipe-compiler@sha256:<manifest hex>"   # section 5; minimal: local/recipe-minimal@…
init = "/inputs/recipe-worker"        # domain::WORKER, unchanged
args = ["--__recipe-worker", "/inputs/recipe.rhai"]    # domain::WORKER_ARGS, unchanged
environment = [ …≤ 8 allowlisted entries, unchanged… ]
[resources]
vcpus = <options.jobs>
memory_mib = <options.memory_mib>     # native meaning: per-process address-space bound (config/instance-gate5.toml comment)
[execution]
profile = "balanced"
backend = "ptrace-native"
[access]
uid = 1000                            # domain::UID; finding 7
gid = 1000
[contract]
version = 1
catalog = 2                           # pins().catalog
default = "deny"
allow = [ …Contract::derived(network).allow, unchanged… ]
deny = [ …unchanged… ]
[contract.filesystem]                 # not in the digest (finding 5); fixed text
default = "deny"
read = ["/"]
write = ["/work", "/out"]
create = ["/work", "/out"]
[contract.network]                    # only with a grant
default = "deny"
outbound = ["tcp:<port>", …]          # the distinct ports of the pinned endpoints, sorted, ≤ 8
[storage]
assignments = []                      # as config/instance-gate5.toml; no memory:, no limits
[devices]
assignments = ["null", "random", "urandom"]
[network]
backend = "none"                      # "shared" with a grant (finding 8)
[audit]
enabled = true
[failure]
policy = "stop"
```

Removed lines: `[storage] max_objects/max_bytes/archive_import`, `memory:`
assignments, `[network] endpoints/max_connections/max_bytes`, the fallback
renders without archive import or random devices (`render` lines 115-124).
`[contract.syscalls]` stays absent, as today (`control` defaults to
`selective`, `docs/configurable-contract.md` "Syscall control"); if the
derivation in section 6 shows the native path needs an explicit syscall list,
that is decided there, with the digests. The `§2` caps (`MAX_ARGS`,
`MAX_ENV`, `MAX_CONFIG` 4096) stay. `reject_host_leak`
(`src/inputs/mod.rs:285`) keeps running on the text. Validation before create:
`ginux validate FILE` (unprivileged, CLI `validate_recipe` parses locally),
section 2 N3/N17.

### 1.4 What replaces `PDEATHSIG`/process groups, and the 1 s relay

| Event | Today (`configured::run`) | After |
| --- | --- | --- |
| Coordinator killed | launcher dies by `PDEATHSIG`; `PTRACE_O_EXITKILL` kills tracees | CLI dies by `PDEATHSIG`; helper sees the caller gone within 0.2 s and runs `teardown` (≤ ~7.5 s); tracees die by `PTRACE_O_EXITKILL` (`main.rs:5806`); the stopped instance stays until delete/sweep |
| Ctrl-C / SIGTERM to Recipe | `reap_launcher` sends the signal to the launcher's group | group SIGTERM to CLI+sudo; sudo relays to the helper; CLI exits after ≤ 1 s by the signal |
| Deadline | `Category::Deadline`, group kill | same signal path, `Category::Deadline` |
| Helper SIGKILLed (only root can) | n/a | the instance keeps running detached (interim design 2.1 threat note); the sweep cannot delete a running instance; `ginux stop` by the sweep after the owner is dead |

### 1.5 The line protocol over the PTY console

What must be true, from the code:
- The worker writes one record per `write_all` + `flush` to fd 1
  (`src/worker/run.rs` `write_line`), fd 1 is the PTY slave of `script -qefE
  never` (helper line 1257), whose output (`-f` flushes) is appended to
  `console.log`. No input is ever written to `console.in`, so there is no
  echo; `-E never` disables script's own echo. The tty's output processing
  turns each `\n` into `\r\n` (default `ONLCR`).
- Before the workload, the runtime writes `GINUX_INSTANCE_START: …` to the
  same console (finding 3).
- `console.log` is truncated by `start` (helper line 1130) and does not exist
  before the first start of a new instance, so a reader cannot see stale bytes.
- The file is root-owned in `STATE/instances/NAME/` (0755 directories, file
  mode from the helper's umask; helper never sets one except for the outcome,
  line 207). It is readable by the caller only when the effective umask under
  `sudo` leaves it `0644`. **Required:** F2 (fork, WP4): `start_instance`
  creates `console.log` and `audit.log` with an explicit `chmod 0644` (after
  helper line 1130). Without F2 a user with umask 077 gets no lines (risk R6).

Recipe side (WP1, `src/launch/worker_io.rs`):
- A new `ConsoleTail` source: opens `paths.console_log` from the first
  `status --format json` it can get after spawning `start --wait` (poll every
  100 ms until the file exists, bounded by the output timeout), then reads
  with `O_RDONLY|O_NONBLOCK`; a read of 0 is "no data yet" while the CLI runs
  and end of file once the CLI has exited and one more read returns 0 (all
  output is flushed before the helper's `wait_for_instance` returns, because
  the supervisor waits for `script`, helper line 1284).
- `pump_lines` becomes generic over a `LineSource` trait implemented by
  `DeadlinePipe<ChildStdout>` (host runs) and `ConsoleTail` (Ginux runs);
  `LineReader` keeps the `MAX_LINE_BYTES` cut, the complete-line timeout and
  the idle/startup deadlines unchanged.
- Each line: strip one trailing `\r` (serde already treats `\r` as
  whitespace, but plain text would keep it), then `LineRecord::parse`
  (`src/worker/records.rs:637`). Lines starting `GINUX_` are runtime text:
  appended to the supervisor log copy, not relayed to the user. Other
  non-records stay plain text within `MAX_DIAGNOSTIC_BYTES`, as today.
- Ordering: the worker is the only writer during the run except the start
  summary, which precedes the worker's first line. Torn lines can occur only if
  a recipe program writes to the inherited console itself; the worker runs
  programs with captured output, so a torn line can only become plain text,
  never a forged success (results come from the extracted file).
- Truncation: `console.log` is unbounded on disk (risk R7); Recipe reads at
  most what its budgets keep and deletes the instance afterwards.

### 1.6 Leftovers sweep

At the start of every Ginux-using command (`backend.rs`, once per process):
list `STATE/instances/` (readable), for each `recipe-<uid>-<pid>-<hex>` with
`<uid>` = own uid and `<pid>` not alive (`kill(pid, 0)` = ESRCH): `status
--format json`; `stopped` → `delete`; `running` (helper died) → `stop`, then
`delete`. A live pid is skipped (pid reuse can only leave garbage, never kill a
live run). Root sweeps every uid. Failures are warnings.

---

## 2. Every need (N1-N19) and every section 4 item after the cutover

| # | After the cutover | Lost? |
| --- | --- | --- |
| N1 init/args/env | same fields (1.3); init check moved by F1 | no |
| N2 vcpus/memory | `[resources]`; native semantics are affinity and per-process address space, same as the fork's launch config comments | no |
| N3 contract | catalog 2, same allow/deny, digests re-derived (section 6); `ginux validate` digest checked against `pins()`; catalog checked from Recipe's own text (finding 4) | the seven 1002 operations (finding 6), R2 |
| N4 devices | `["null","random","urandom"]` always; the helper `mknod`s console/null/zero/random/urandom into every root (helper `prepare_root`, line 641) | the `random_devices` probe and `RECIPE_RANDOM_DEVICES` go: no older-launcher case remains |
| N5 audit/failure | `[audit] enabled = true`, `[failure] policy = "stop"` | no |
| N6 image | OCI layout + `import-image`, manifest digest is the identity (section 5) | no |
| N7 fresh root per run | one instance per run, unique name, always deleted; reflink base roots where the filesystem allows (helper `materialize_root`) else full expansion | **cost**: without reflinks each run expands ~1.3 GB and runs `chown -R` and `find -exec chmod` over it (helper lines 650-651) — slower than today's verified unpack (R5) |
| N8 caller-owned lifetime | 1.4 | detached leftovers only if the helper itself is SIGKILLed |
| N9 outcome | `status --format json` outcome (1.2 step 7) | no |
| N10 | dropped (D6) | — |
| N11 streams | stdout records on the console (1.5); stderr + audit in `audit.log` (finding 2) | unbounded host files until delete (R7) |
| N12 inputs | copied by `inject` (1.2 step 3) | **bind_read_only goes.** Live system roots (`InputKind::Bind`, `--system`) are copied, which is the fallback `prepare_invocation` already takes without `CAP_SYS_ADMIN` (`src/inputs/mod.rs:125-140`): GBs per run, no safety loss. The offline cache, bound read-only today (`root.rs:211-238`), is replaced by injecting only the declared pins (`Origin::Url { sha256 }`, `src/declarations.rs:781-831`) present in the cache: downloads a recipe computes at run time without declaring them no longer find the cache (behaviour regression for offline OS builds, R8). Inputs are owned by uid 65534 after injection, so only `[contract.filesystem] write/create = ["/work","/out"]` keeps the workload from changing them (enforcement on the native path to be verified, R3) |
| N13 storage limits | `[storage]` has no limits upstream or in the fork's public path (U9 not implemented). Recipe keeps two checks: (a) `extract --max-bytes/--max-members` (1.2 step 8) refuses oversized outputs after the run; (b) a free-space watchdog in `lifecycle::run`: every 5 s `statvfs(STATE)`; if free space drops below `max(2 GiB, 5 %)` the run is cancelled (`Category::Storage`, "the host disk is nearly full") | **SAFETY REGRESSION.** A recipe can fill the host filesystem holding `/var/lib/recipe/ginux` during the run; the old `memory:` storage refused writes past `max_bytes`/`max_objects` inside the domain. The watchdog bounds the damage, not the write. Documented in `docs/runtime-limits.md` |
| N14 export | `extract` (1.2 step 8), result read from the export as today | special files in `/work` or `/out` now fail the run (65) |
| N15 | dropped (D1) | — |
| N16 network grants | `backend = "shared"` + `outbound = ["tcp:PORT"…]` from the pinned endpoints' ports; `/etc/hosts` injected with the pins. Recipe-side bound: the worker installs `HostNetwork` (`src/helpers/acquire/net.rs`) in Ginux runs too (request field, 2.12), so Recipe's own downloads reach only the declared `HOST:PORT` at the pinned addresses, bodies are counted against `network_max_bytes`, and `network_usage` comes from the worker's result (`launch/mod.rs:156-160` already handles `Some`) | **SAFETY REGRESSION.** Any program the recipe starts (git, curl, a build system) can connect to *any address* on a granted port, with no connection or byte cap; the shared host network is visible (U11 not implemented). The worker-side bound covers only Recipe's own HTTP client. The `ginux: network-summary` and `egress=denied` reports (`needs/report.rs:273-367`) become `unobserved`/None |
| N17 diagnostics | `ginux validate/inspect/explain FILE` (unprivileged; text, same `Contract valid: … digest= …` and `Rule: … source= index=` lines, CLI `validate_recipe`, `explain_recipe`); run audit parsed from `GINUX_NATIVE_AUDIT:` lines (fields `operation=`, `policy-source=`, `policy-rule=`, `main.rs:7971-7988`) in `explain_invocation` | denial summary, network audit fields |
| N18 hosts | section 3 | ordinary users without a sudo rule (**OWNER**) |
| N19 | archive import and descriptor reservation: deleted, fallback stays | speed only |

Section 4 of the migration doc, item by item:
- `src/launch/` split: done in Stage 2; `configured.rs` → `lifecycle.rs`.
- `config.rs`: no `memory:`, `endpoints`, `archive_import` (1.3). The doc's
  "adds volumes and the storage limit from U7/U9" cannot happen: neither is
  implemented anywhere in the fork's public path (contradiction recorded).
- `contract.rs`: re-derived pins (section 6); `configured-network-audit` and
  `configured-denial-audit` checks (`contract.rs:377-384`), `probe_feature`,
  `archive_import`, `random_devices` and both env vars deleted.
- `src/ginux.rs` result-record reader and `src/target.rs` ioctl: the reader is
  replaced (1.1); the ioctl path is deleted (section 8).
- `backend.rs`, `runtime.rs`, `bin/recipe/runtime.rs`: section 4.
- `distro/builder` stops building the fork launcher alone: section 7.
- `GINUX_BEHAVIOUR` bumped: section 4.

### 2.12 New request field (WP2)

`WorkerRequest` gains `network: Option<NetworkGrant { pins: BTreeMap<String,
Vec<String>>, max_bytes: u64 }>` for both runtimes; `HostRun` loses those two
fields (`src/worker/records.rs:164-182`), keeping only what is host-specific.
The worker installs `HostNetwork` whenever `network` is set
(`src/worker/run.rs:188`). `RECORDS_VERSION` is bumped. `inject_archive`
(new, `src/inputs/root.rs`) replaces `prepare_root` for Ginux runs and reuses
`stage_inputs`' checks; `prepare_root`, `PreparedRoot`, `seal_inputs` (Ginux
use) and `seed_target`'s hard-link branch change as section 8 lists.

---

## 3. Privilege and host model (**OWNER decision**)

Facts:
- Every privileged verb runs `sudo /usr/libexec/ginux/ginux-helper --protocol 1
  …` (CLI `helper_command`; `DEFAULT_HELPER`, overridable only on the CLI side
  by `GINUX_INSTANCE_HELPER`, CLI `helper_path`). `sudo` is spawned without
  `-n`, so without a rule it prompts on the caller's tty, and fails without one
  (the desktop app has none).
- The helper requires every component of a state or run root to be a real
  directory owned by root, not group/other writable (`trusted_root_level`,
  helper lines 51-79), and creates missing levels as root 0755.
- Gate 26 ships no sudoers rule and treats the helper as administrator-only;
  the interim design repeats it ("anyone who may run the helper through sudo
  is administrator-equivalent today", section 2.4 threat note).
- The helper resolves its tools only at compiled-in paths when run through
  sudo (environment reset; the e2e script needs a wrapper, 
  `scripts/test-public-lifecycle.py` lines 172-180). So the helper, runtime and
  image tool must be root-owned, not user-writable files at
  `/usr/libexec/ginux/`. A user-writable copy under `~/.local` reached through
  a NOPASSWD rule would be a root escalation.
- Root-launched workloads drop to host uid 65534 (`docs/configurable-contract.md`,
  "Bootstrap and host execution boundary"); no user namespace, so the AppArmor
  `userns` profile (`tools/recipe/data/apparmor-ginux`) has no purpose.

What each party experiences after the cutover:
- **OS builder** (`cargo xtask build`, already root): `sudo` as root needs no
  rule on Fedora/Rocky/Ubuntu defaults, but the `sudo` binary must exist on the
  build host; state root `/var/lib/recipe/ginux` is created on first use; the
  four Ginux files must be installed root-owned at `/usr/libexec/ginux/` on the
  build host, or the builder writes a root-owned wrapper and sets
  `GINUX_INSTANCE_HELPER` (section 7).
- **Standalone user on Fedora 44 / Ubuntu 24.04**: needs the Ginux files at
  `/usr/libexec/ginux/` (root install, which `web-install.sh` already performs
  through root/sudo/doas, line 7 and 301) and, to run anything in Ginux, a
  sudoers rule; without it Recipe stops with the way forward (run as root, or
  install the rule). Recipes that start no program still run on the host
  (`--execution auto`, `src/execution.rs`).
- **Installed LevitateOS/AcornOS desktop user**: same as standalone; the
  images ship the rule.

What `recipe runtime stage` must install (into `--root`):
`<prefix>/libexec/recipe/ginux` (the CLI, `ginux-cli`), `usr/libexec/ginux/ginux-helper`
(0755), `usr/libexec/ginux/ginux-image` (`ginux-oci`), `usr/libexec/ginux/ginux-runtime`
(`ginux-init --bin ginux`), `usr/lib64/ginux/libginux_allocator.so` (the helper
copies it into every root when present, helper lines 653-658, so it must be
present everywhere or nowhere: ship it, as the RPM does), the image layouts
(5.4), and with `--sudoers GROUP`: `etc/sudoers.d/recipe-ginux` (0440):

```
%GROUP ALL=(root) NOPASSWD: /usr/libexec/ginux/ginux-helper --protocol 1 --state-root /var/lib/recipe/ginux --run-root /run/recipe/ginux *
```

(it pins Recipe's roots; the helper verbs stay as powerful as Gate 26 says).
No host confinement profile is needed (no user namespace); SELinux on Fedora
runs the helper as `unconfined_t` through sudo, untested (R9).

Fork packaging today: `packaging/rpm/{ginux.spec.in,ginux.tmpfiles,ginux.rpmlintrc}`
only, Rocky 10, `Requires: coreutils gawk grep iproute procps-ng sed shadow-utils
sudo systemd util-linux` (spec lines 17-26), no sudoers file, no `.deb`, no
OpenRC/tmpfiles alternative. Missing for Fedora 44 and Ubuntu 24.04: the four
binaries built portably (the CLI and image tool are ordinary Rust binaries;
build them for `x86_64-unknown-linux-musl` static so one build runs on both,
to be checked), the sudoers rule, and the helper's GNU-only tool needs
(`mv -T --no-copy --update=none`, `cp --reflink`, `script -E never`, `flock`,
`gawk`-compatible `awk`, `stat -c`), present on both. For **AcornOS** (musl,
BusyBox, OpenRC, positioned "small and strict") the helper needs util-linux,
coreutils, gawk/procps and sudo added to the image (R4).

Options:
- **A. sudoers rule for the administrators' group, shipped by Recipe**
  (`%wheel` on Fedora/LevitateOS/AcornOS, `%sudo` on Ubuntu; installer picks).
  Admins already may become root, so the rule adds no authority to them; other
  users cannot use Ginux. **Recommended.**
- B. A dedicated `recipe` group with the same rule: any member is
  root-equivalent through the helper; the installer adds the installing user.
  Wider than A for the same convenience.
- C. No rule: Ginux runs only as root (`sudo recipe …`, the GUI through its
  polkit helper). Simplest and honest; "for me" installs by ordinary users lose
  Ginux runs.
- D. Keep the fork's unprivileged `launch` for users: forbidden (no dual runtime).

---

## 4. Environment, resolution, behaviour version, runtime staging

| Today | After |
| --- | --- |
| `GINUX_BIN` (launcher; `backend.rs` `resolve_launcher`, `runtime.rs` `launcher()`) | `RECIPE_GINUX`: the `ginux` CLI; default `runtime::ginux_cli()` = `<prefix>/libexec/recipe/ginux`, else `/usr/libexec/recipe/ginux` (same paths as `LAUNCHER`, `runtime.rs:8`, now holding the CLI) |
| — | `RECIPE_GINUX_STATE_ROOT` (default `/var/lib/recipe/ginux`), `RECIPE_GINUX_RUN_ROOT` (default `/run/recipe/ginux`); both must pass the helper's `trusted_root` |
| `GINUX_ROOT` / `GINUX_MINIMAL_ROOT` (`oci:NAME@sha256:<tar>`) | `RECIPE_IMAGE` / `RECIPE_MINIMAL_IMAGE`: `local/recipe-compiler@sha256:<manifest>` / `local/recipe-minimal@…` |
| `GINUX_ROOTFS` / `GINUX_MINIMAL_ROOTFS` (tar) | `RECIPE_IMAGE_LAYOUT` / `RECIPE_MINIMAL_IMAGE_LAYOUT`: an OCI layout directory or uncompressed tar of one; needed only when the image is not yet imported; still set as a pair with the reference |
| — | `GINUX_INSTANCE_HELPER` (the CLI's own variable, CLI `helper_path`) passes through Recipe to the CLI child unchanged; only the builder and the e2e fixtures set it (wrapper, Q5) |
| `RECIPE_ARCHIVE_IMPORT`, `RECIPE_RANDOM_DEVICES` | deleted |
| `RECIPE_RUNTIME_URL` | stays; asset `image.oci.tar.zst` instead of `rootfs.tar.zst` (`runtime.rs:281-289`) |

A set `GINUX_BIN`, `GINUX_ROOT*`, `GINUX_MINIMAL_ROOT*` makes Recipe stop
with "GINUX_X is gone; set RECIPE_Y" (a stale ambient variable once cost a day,
memory note on `GINUX_ROOT`); this is a refusal, not a second spelling.

`src/runtime.rs`: `root_digest` accepts `REGISTRY/REPO@sha256:<hex>` (the
helper's `materialize_root` takes the 64 hex after `@sha256:`, helper line 250;
`parse_reference` needs a registry component, interim design finding 8) and no
longer requires `oci:`. `DEFAULT_ROOT_REF` (line 21, an unpublished bare base)
is deleted: no reference and no installed `root-reference` is a `Startup`
error with the way forward. `rootfs_path`/`ensure_rootfs`/`open_rootfs` become
`layout_path`/`ensure_layout` (download `image.oci.tar.zst`, decompress to
`~/.cache/recipe/images/<manifest hex>.oci.tar`, no Recipe-side hash: the
import verifies every blob and the printed manifest digest must match).
`build_image`/`remove_build_image` count and remove only the download cache;
the imported blobs and base roots under the root-owned state root cannot be
removed by any public verb (no image delete exists) — reported as
`"system_bytes"` and documented (`recipe runtime remove` frees less, R10).
`ROOTFS`/`MINIMAL_ROOTFS` constants become `share/recipe/runtime/compiler.oci.tar`
and `minimal.oci.tar`.

`src/identity.rs`: `GINUX_BEHAVIOUR` 1 → 2, comment: "2: the public lifecycle
(native configured runtime, catalog 2, virtual uid from `[access]`, root-overlay
host tree, inputs injected as copies without hard links, `/etc/hosts` always
present, shared network)". `ENGINE_BEHAVIOUR` stays 3; `RECORDS_VERSION` bumps
(2.12). Exact identity (`src/retained.rs:263-272`) hashes the four Ginux files
(CLI, helper, image tool, runtime) instead of the launcher: `Ginux::digest()`
= SHA-256 of their four SHA-256s in that order.

`recipe runtime stage` (`src/bin/recipe/runtime.rs:334`): `--launcher` is
replaced by `--ginux-cli FILE --ginux-helper FILE --ginux-image FILE
--ginux-runtime FILE --ginux-allocator FILE`, `--image/--minimal-image` take
layouts (5.4) and are checked by computing the manifest digest from the
layout's `index.json` and manifest blob (no tar hash), `--apparmor` is replaced
by `--sudoers GROUP` (section 3). `status` reports the CLI, the four files
(present, root-owned, mode), the state root, whether the image is imported
(manifest blob present), and whether `sudo -n -l HELPER --protocol 1
--state-root … --run-root … list` succeeds.

Standalone installer (`tools/recipe/scripts/web-install.sh`) and
`docs/recipe-standalone-release.md`: the install step (already root) installs
the five Ginux files and the sudoers rule for the chosen group (A), creates
nothing else (the helper makes the state root). The release command
(`xtask/src/tasks/recipe_release.rs`) builds and stages the five files and
the layouts (section 7); `CONFFILES` loses `etc/apparmor.d/recipe-ginux` and
gains `etc/sudoers.d/recipe-ginux`; the deb `postinst` stops loading AppArmor.
The doc's privilege section (step 3, "Privileges are settled on a foreign
host") is rewritten from section 3.

---

## 5. The compiler image as an OCI layout

### 5.1 Producer (`src/image.rs`, `src/bin/recipe-image.rs`)

`recipe-image` keeps writing the layer exactly as now (`recipe-image.rs:176-257`,
normalized headers, every member through `image::Validator`), into
`blobs/sha256/<L>` instead of `rootfs.tar`, then writes, byte-stable (keys in
this order, no whitespace, no trailing newline, `serde_json` from a struct
with fixed field order):

```
oci-layout     {"imageLayoutVersion":"1.0.0"}
blobs/sha256/C {"architecture":"amd64","os":"linux","rootfs":{"type":"layers","diff_ids":["sha256:L"]},"config":{}}
blobs/sha256/M {"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json","config":{"mediaType":"application/vnd.oci.image.config.v1+json","digest":"sha256:C","size":SC},"layers":[{"mediaType":"application/vnd.oci.image.layer.v1.tar","digest":"sha256:L","size":SL}]}
index.json     {"schemaVersion":2,"manifests":[{"mediaType":"application/vnd.oci.image.manifest.v1+json","digest":"sha256:M","size":SM,"platform":{"architecture":"amd64","os":"linux"}}]}
```

(interim design 2.5; identical to `test-public-lifecycle.py` `write_layout`).
The layer is uncompressed, so diff_id = layer digest (checked by
`import-layout` step 5). `provenance.json` gains `"manifest_digest":"<M hex>"`
and `"layer_sha256"` (the old `"sha256"` field is replaced, not kept).
The shared function is `levitate_recipe::image::write_layout(layer: &Path,
out: &Path) -> Result<String /* M hex */>` so the builder's own roots (5.3) use
the same bytes. It writes into a fresh directory and also an uncompressed
`image.oci.tar` of it (members `oci-layout`, `index.json`, `blobs/`,
`blobs/sha256/`, `blobs/sha256/<hex>` in that order, normalized headers; the
CLI accepts exactly these names, interim design 2.5).

### 5.2 New layer rules (`image::Validator`, both producer and the builder's roots)

- no member whose name starts `.wh.` in any component (the expander treats it
  as a whiteout, interim design finding 7);
- `etc/hosts` is left out of the layer (Recipe injects it; the injector never
  overwrites, so an image with it fails every run with exit 65);
- device and FIFO entries were already refused (`image.rs`
  `Validator::check`, final `else` branch); reserved roots `inputs`, `work`,
  `out` stay refused (`RESERVED_ROOTS`).

### 5.3 Identity, pins, prebuilt, builder

- The manifest digest replaces the tar digest everywhere the image is named:
  `bootstrap/product-ci.json`, `minimal-runtime.json`, `managed-ci.json`
  `image_sha256` → `image_digest` (the manifest hex). The reference string
  `local/recipe-compiler@sha256:<M>` enters the state identity through
  `prepared.root_reference` (`src/retained.rs:300`) as it does today, so every
  saved state changes once (expected; also changed by the pins and
  `GINUX_BEHAVIOUR`).
- `RuntimeKind::pins()` holds only the catalog and the two contract digests;
  the image digest is not a pin there (unchanged split: pins are about the
  runtime, the reference about the image). The table's single variant is
  renamed `ConfiguredLaunch` → `PublicLifecycle` (the old row is deleted).
- Prebuilt (`github.com/LevitateOS/prebuilt`, `distro/builder/src/prebuilt.rs`
  `publish_runtime_cmd`): release `recipe-runtime-<M first 16>` with asset
  `image.oci.tar.zst` (zstd of the layout tar). `xtask` `check_runtime_release`
  (`recipe_release.rs:384-391`) checks that asset name.
- `distro/builder`: `PinnedImage::select` (`src/recipe/mod.rs:107-143`) reads
  `<dir>/image.oci.tar` and `provenance.json` `manifest_digest`;
  `reference()` returns `local/<name>@sha256:<M>`. Runs pass
  `RECIPE_IMAGE`/`RECIPE_IMAGE_LAYOUT` (and the minimal pair) instead of
  `GINUX_ROOT`/`GINUX_ROOTFS` (`mod.rs:821-848`, `LAUNCHER_ENV` line 35).
  `Invocation::root` (`mod.rs:526`, used by `desktop_apps.rs:185`) takes a
  rootfs tar today: it must turn that tar into a layout with
  `image::write_layout` once per distinct root (cache by tar SHA-256 under
  `.artifacts/ginux-env/roots/<sha>/`), and pass the manifest reference.
  `xtask/src/tasks/base.rs:283` reads the compiler image's rpmdb from
  `rootfs.tar`: read the layer blob `blobs/sha256/<L>` instead (same bytes).

### 5.4 Import, once per host

`Ginux::ensure_image` (1.2 step 1). The OS images ship the layouts at
`/usr/share/recipe/runtime/{compiler,minimal}.oci.tar`; the first run imports
them (one copy into `/var/lib/recipe/ginux/cache/oci`, about 1.3 GB more on
disk). Open question Q3 offers shipping the cache pre-populated instead.

---

## 6. Re-deriving `CATALOG` and the two digests (**NEEDS BINARY**)

The only step of the cutover that needs a built fork binary; it is not part of
writing the change set and is done at the start of the verification phase.

1. Build the fork CLI at the pin (`cargo build -p ginux-cli --release` in
   `tools/ginux`).
2. Write the two declaration-only configs exactly as the new
   `contract::inspection_config` renders them for `Contract::derived(false)`
   and `Contract::derived(true)` (catalog 2, section 1.3's `[contract]`; the
   filesystem/network sections do not change the digest, finding 5). The
   simplest source of the exact bytes is `recipe inspect --json` on two
   fixture recipes (one without, one with `NEEDS.network`): with placeholder
   pins it fails with "Ginux catalog-2 build-base digest changed: expected X,
   got Y" (`contract.rs:401-406`), Y being the derived digest. Cross-check
   each with `ginux validate FILE` (line `Contract valid: instance=… catalog=2
   digest=Y operations=N`). Never type a digest by hand.
3. Review, as the comment above `pins()` requires (`contract.rs:51-76`): for
   each config `ginux inspect FILE` (`Decisions: emulate= mediate= deny=
   total=`) and, for every selector in `allow`/`deny` and for each of
   `syscall.lstat`, `syscall.readv`, `syscall.pipe`, `syscall.nanosleep`,
   `syscall.sendfile`, `syscall.link`, `syscall.rt_sigpending`,
   `ginux explain FILE OP` (exit 2 and "not present in catalog version 2"
   proves absence, CLI `explain_recipe`). Record: catalog 1002 → 2, the seven
   configured operations are gone from the catalog (their behaviour under
   native configured mode verified by an e2e run, R2), selectors unchanged,
   both digests change.
4. Write the values into the `PublicLifecycle` row and append the review
   entry to the comment ("Reviewed for the public-lifecycle cutover
   (LevitateOS/ginux 8f4d53ba): …").

---

## 7. Builder and repository plumbing

| Place | Change |
| --- | --- |
| `distro/builder/src/recipe/mod.rs` `ginux_binary` (line 362) | becomes `ginux_tools(repo) -> GinuxTools {cli, helper, image, runtime, allocator}`: `cargo build --release -p ginux-cli -p ginux-oci -p ginux-allocator` plus the runtime build as now (`-p ginux-init --bin ginux --target x86_64-unknown-linux-gnu`); the helper is `tools/ginux/scripts/ginux-host-instance-helper.sh`. `.artifacts` is user-owned, so it fails the helper's root-owned-chain rule; the builder (root) copies the five files into `/var/lib/levitate-builder/ginux/` (root-owned 0755 chain) and writes a root-owned wrapper there that exports `GINUX_RUNTIME`/`GINUX_IMAGE_TOOL` and execs the helper copy; runs get `GINUX_INSTANCE_HELPER=<wrapper>` (Q5). The host's `/usr/libexec` is never touched |
| same, `Invocation::run` (lines 799-848) | sets `RECIPE_GINUX` (the checkout's CLI build), `GINUX_INSTANCE_HELPER` (the wrapper), `RECIPE_IMAGE[_LAYOUT]`, `RECIPE_MINIMAL_IMAGE[_LAYOUT]`, keeps `RECIPE_WORKER_BIN`; drops `GINUX_BIN`, `GINUX_ROOT*`; `LAUNCHER_ENV` becomes `["RECIPE_IMAGE","RECIPE_IMAGE_LAYOUT","RECIPE_OUTPUT_TIMEOUT_SECS"]`; the env scrub keeps removing every other `RECIPE_*`/`GINUX_*` |
| `distro/builder/src/pipeline/sources.rs:57-59` | hashes the five Ginux files ("shipped ginux tools") instead of the launcher |
| `distro/builder/src/parts.rs:66-71, 300-356` | `DEFAULT_GINUX_ROOTFS` → `.artifacts/ginux-env/prepared-image-build-tools/image.oci.tar` (and minimal); `ginux-launcher` key → `ginux-tools` (digest of the five); env names as section 4 |
| `distro/builder/src/compose.rs:314-316` | builds `ginux_tools` once before threads |
| `distro/builder/src/prebuilt.rs:280-300, publish_runtime_cmd` | ambient-variable warning uses `RECIPE_IMAGE`/`RECIPE_MINIMAL_IMAGE`; publishes `image.oci.tar.zst` |
| `distro/builder/src/desktop_apps.rs:185` | passes a layout built from the variant root (5.3) |
| `distro/builder/os-recipes/install-tools.rhai` (lines 24, 124-139, 191) | inputs `ginux_cli`, `ginux_helper`, `ginux_image`, `ginux_runtime`, `ginux_allocator`, `runtime_layout`, `minimal_layout` replace `launcher`, `runtime_image`, `minimal_image`; `recipe_runtime` calls the new `runtime stage` flags with `--sudoers wheel`; `verify` checks `usr/libexec/ginux/{ginux-helper,ginux-image,ginux-runtime}`, `usr/libexec/recipe/ginux`, the `.oci.tar` files and `etc/sudoers.d/recipe-ginux`; `target_mode` 0440 root for the sudoers file, 0755 root for the helper |
| `distro/builder/README.md` | names |
| `xtask/src/tasks/recipe_release.rs` (lines 3-41, 145-160, 358-369, 384-413) | `--launcher` → `--ginux-dir`; builds the five tools; static-ELF check applies to the runtime; CLI and image tool built for musl static (R11); asset `image.oci.tar.zst` |
| `xtask/src/util/distro_builder.rs:112-115` | pass-through list → `RECIPE_IMAGE`, `RECIPE_IMAGE_LAYOUT`, `RECIPE_MINIMAL_IMAGE`, `RECIPE_MINIMAL_IMAGE_LAYOUT` |
| `xtask/src/tasks/base.rs:283` | reads the layer blob of the layout |
| `tools/recipe/.github/workflows/ci.yml` (managed-integration, `workflow_dispatch` only) | builds CLI, image tool, runtime; installs them as root to `/usr/libexec/ginux` on the runner; produces the layout; passes `--ginux-dir` to `managed-integration.py`. The quick `ci` job is unaffected |
| `tools/recipe/scripts/managed-integration.py:44-61` | environment `RECIPE_GINUX`, `RECIPE_IMAGE`, `RECIPE_IMAGE_LAYOUT` instead of `GINUX_BIN/ROOTFS/ROOT`; runs as root already (`sudo python3`) |
| `tools/recipe/scripts/prepare-managed-image.py:98` | records the manifest digest from `provenance.json` |
| `tools/ginux/scripts/ginux-host-instance-helper.sh` (fork) | F1 (init check to `start`), F2 (`chmod 0644` on the two logs) |
| `testing/ginux-boot/*` | uses `ginux native` directly (`stage.sh:2`); unaffected |
| Superproject `.github/workflows` | no file names the launcher (grep); unaffected |
| Docs | `tools/recipe/README.md`, `CLAUDE.md` (environment row), `docs/man/recipe.1`, `docs/setup-linux.md`, `docs/runtime-limits.md` (N13/N16 regressions), `bootstrap/README.md`, migration doc status, superproject `docs/recipe-local-validation.md`, `docs/recipe-standalone-release.md`, `docs/ginux-recipe-interfaces.md` |

### e2e suites (`tools/recipe/tests/e2e/*`, `tests/library_backend.rs`, `tests/image_producer.rs`)

All Ginux suites need: root (or the section 3 sudo rule) on a host with the
five Ginux files at `/usr/libexec/ginux/`, the layout imported or
`RECIPE_IMAGE_LAYOUT` set, and a trusted state root. Common change in
`tests/e2e.rs`: a helper `ginux_env(&mut Command)` sets `RECIPE_GINUX`,
`RECIPE_IMAGE`, `RECIPE_IMAGE_LAYOUT` and a per-test
`RECIPE_GINUX_STATE_ROOT=/var/lib/recipe-e2e/<test>` (root-owned, so the
fixture creates it as root) so tests never share instances.

| Suite | Today | Adaptation |
| --- | --- | --- |
| `managed_cancellation.rs:210-274` | wrapper scripts around `GINUX_BIN` that delay or fail launcher exit (exit 37) | wrapper around the `ginux` CLI (via `RECIPE_GINUX`): for `start` it runs the real CLI then sleeps 12 s / exits 37; asserts "ginux start exited with code 37" and no publication; cancellation cases assert the instance is gone (`STATE/instances` empty) after SIGINT |
| `managed_inputs.rs:206-213, 1120-1145, 1378-1440` | wrapper rewrites `--export-file` tar with traversal/symlink/hardlink/longname members | wrapper on `extract`: after the real CLI wrote `--output FILE`, rewrite FILE with the same seven malformed archives; assertions unchanged (publication still refuses). The partial-line case (`RECIPE_OUTPUT_TIMEOUT_SECS=1`) keeps its bound against the console tail |
| `needs.rs:450-500` | wrapper rewrites `/inputs/.recipe-request.json` before `launch` | wrapper on `inject`: rewrite the request member inside `--input FILE` to version 8, then call the real CLI; same assertion |
| `managed_state.rs:9,429`, `managed_target.rs:24` | require `GINUX_BIN` | `ginux_env`; kept-state cases unchanged |
| `managed_storage.rs` | `memory:` limits | asserts the new behaviour: an output over `storage_max_bytes` fails at extract with the storage message; the free-space watchdog is not tested (it needs a small filesystem; use a loop-mounted 1 GiB state root only if cheap) |
| `worker_entry.rs:20,39` | `GINUX_BIN` missing file | `RECIPE_GINUX` missing file; message names the variable |
| `library_backend.rs` | asserts `engine.launcher().path()` == `GINUX_BIN` | `engine.ginux().cli()` == `RECIPE_GINUX` |
| `image_producer.rs` | checks `rootfs.tar` digest | checks the layout: manifest digest stable across two productions, `index.json`/manifest/config bytes exact, no `etc/hosts`, no `.wh.` |
| others (`archives`, `downloads`, `git_acquisition`, `kernel_*`, `preseed_recipes`, `dependencies`, `cleanup`, `query`, `cli`, …) | run through `recipe_command` (`--execution ginux`) | only `ginux_env`; `downloads` and `git_acquisition` gain one case each: a program in the recipe connecting to an undeclared address on a granted port succeeds (documents N16) |

---

## 8. Deletion list, and what stays

Deleted (files, functions, constants, env vars):
- `src/launch/configured.rs` whole: `run`, `ensure_open_file_limit`,
  `LAUNCHER_FD_MARGIN`, `bind_read_only` (moved: `enrich_stream_error`,
  `check_agreement`, `check_launcher_exit` → `lifecycle.rs`).
- `src/ginux.rs`: `ResultRecord::read`, `peek_outcome`, `result_note`
  (replaced by the status record); `Outcome` stays.
- `src/inputs/image_cache.rs` whole (`populate`, `cached_bytes`,
  `remove_caches`) and its re-exports in `src/inputs/mod.rs:31-34`.
- `src/inputs/root.rs`: `VerifiedImage`, `unpack_verified`, `Hashed`,
  `verified_snapshot`, `unpack_image`, `PreparedRoot`, `prepare_root`,
  `MAX_WORKER_BYTES` moves to `inject_archive`; `seed_target`'s hard-link
  branch (regular copies instead).
- `src/inputs/mod.rs`: `can_bind`, the bind/copy switch (lines 122-142: always
  copy), the launcher probes (lines 182-199 → `ginux validate`),
  `PreparedInvocation::binds`, `lease_fd` uses for the launcher.
- `src/inputs/config.rs`: `archive_import`, `random_devices` fields, the
  three-step `render` fallback, storage limits and endpoint lines.
- `src/inputs/network.rs` `install_hosts` (the hosts text moves into the tar).
- `src/needs/contract.rs`: `RuntimeKind::ConfiguredLaunch` row,
  `ARCHIVE_IMPORT_ENV`, `RANDOM_DEVICES_ENV`, `probe_feature`,
  `archive_import`, `random_devices`, the `configured-network-audit` /
  `configured-denial-audit` checks, `command`'s `instance-validate` /
  `instance-explain` verbs (now `validate` / `explain` on the CLI).
- `src/needs/report.rs`: `ginux: audit` / `denial-summary` parsing (→
  `GINUX_NATIVE_AUDIT:`), `network_usage` from `ginux: network-summary`
  (→ worker-reported usage; the function returns `unobserved` otherwise),
  `refused_connection`'s `egress=denied` (returns None; caller copes).
- `src/target.rs:1490-1938`: `GINUX_ARCHIVE_IMPORT`,
  `GINUX_ARCHIVE_CLEAR_SPECIAL_MODES`, `IMPORT_CHUNK`, `IMPORT_MAX_LINK`,
  `ArchiveImportRequest`, `open_import`, `Applied`, `Sent`, `ImportStream`,
  `import_stopped`, `OpenDir`, `AfterImport`, `send_member`, `close_dir`,
  `import_tree`, `import_members`; `unpack_tree` calls `extract_tree` only.
- `src/backend.rs`: `GinuxLauncher`, `resolve_launcher`, `is_executable`
  (→ `crate::ginux::Ginux::from_env`); `src/lib.rs` `with_launcher`/`launcher`
  → `with_ginux`/`ginux`, re-export renamed.
- `src/runtime.rs`: `LAUNCHER` meaning, `DEFAULT_ROOT_REF`, `rootfs_path`,
  `ensure_rootfs`, `open_rootfs`, tar-named constants (renamed, section 4).
- `src/bin/recipe/runtime.rs`: `--launcher`, `--apparmor`, `APPARMOR`;
  `tools/recipe/data/apparmor-ginux` file.
- Env vars: `GINUX_BIN`, `GINUX_ROOT`, `GINUX_ROOTFS`, `GINUX_MINIMAL_ROOT`,
  `GINUX_MINIMAL_ROOTFS`, `RECIPE_ARCHIVE_IMPORT`, `RECIPE_RANDOM_DEVICES`.
- Builder/xtask: `ginux_binary`, `GINUX_*` plumbing, `build_launcher`,
  `rootfs.tar` image paths (section 7).

Stays: `src/launch/mod.rs` (start decision, `done_report`, `finish_outputs`,
`complete`, `recorded`), `worker_io` (line records, `result_from_archive`,
`result_from_tree`, drains, `reap_launcher` for host runs), host mode
(`src/host.rs`, `src/execution.rs`), `publication.rs`, `retained.rs` (except
the exact-identity line), the recipe format, the worker, the offline cache
module, `--restore-file`'s role (now a merged part of the injected tar),
`domain.rs` layout, `image::Validator` (with 5.2 additions).

---

## 9. Work packages (parallel, disjoint files)

Interfaces fixed by this document (section numbers) so nobody waits:
`crate::ginux::Ginux` API (1.1-1.2), `inputs::inject_archive` signature (1.2
step 3), `InstanceConfig::render` text (1.3), `image::write_layout` (5.1), env
names (4), `WorkerRequest.network` (2.12).

- **WP1 — lifecycle adapter.** Owns `src/launch/lifecycle.rs` (new),
  `src/launch/configured.rs` (delete), `src/launch/mod.rs`,
  `src/launch/worker_io.rs`, `src/ginux.rs`, `src/host.rs` (only the
  `pump_lines`/`LineSource` call), `src/cancellation.rs` if needed. Implements
  1.2 steps 0-2, 4-11, 1.4, 1.5, 1.6.
- **WP2 — inputs, config, contract, worker records.** Owns `src/inputs/{mod,config,root,network,image_cache}.rs`,
  `src/needs/{contract,report}.rs`, `src/target.rs`, `src/worker/{records,run}.rs`,
  `src/helpers/acquire/net.rs`, `src/domain.rs`. Implements 1.2 step 3, 1.3,
  section 2 N12/N16/N17, 2.12, the `PublicLifecycle` row with placeholder
  digests (section 6 fills them).
- **WP3 — resolution, runtime, image producer, CLI.** Owns `src/backend.rs`,
  `src/lib.rs`, `src/runtime.rs`, `src/identity.rs`, `src/retained.rs`,
  `src/image.rs`, `src/bin/recipe-image.rs`, `src/bin/recipe/{runtime,cli,commands}.rs`,
  `data/apparmor-ginux` (delete), `data/sudoers-recipe-ginux` (new),
  `bootstrap/*.json`, `scripts/web-install.sh`. Implements sections 3 (stage
  side), 4, 5.1-5.2, 5.4.
- **WP4 — fork patches, builder, xtask, scripts, CI.** Owns
  `tools/ginux/scripts/ginux-host-instance-helper.sh` (F1, F2, committed in
  the fork with its `test-public-lifecycle.py` case), `distro/builder/**`,
  `xtask/**`, `tools/recipe/scripts/{managed-integration,prepare-managed-image}.py`,
  `tools/recipe/.github/workflows/ci.yml`. Implements sections 5.3 and 7 (table).
- **WP5 — tests and docs.** Owns `tools/recipe/tests/**`, `tools/recipe/{README.md,CLAUDE.md,CHANGELOG.md}`,
  `tools/recipe/docs/**`, superproject `docs/recipe-local-validation.md`,
  `docs/recipe-standalone-release.md`, `docs/ginux-recipe-interfaces.md`.

Hand-offs: WP3 → WP1 the `Ginux::from_env` constructor stub (types only) on
day one, since `backend.rs` constructs what `lifecycle.rs` consumes — WP1 owns
the struct in `src/ginux.rs`, WP3 only calls `Ginux::from_env()` and
`Ginux::digest()`. WP2 → WP1 `inject_archive`. WP3 → WP4 `image::write_layout`
(builder calls it). WP2/WP3 → WP5 the final env names and messages.

Order: (1) all five in parallel against the fixed interfaces; (2) when all are
written: section 6 (needs the binary), then the single verification pass
(e2e suites, a catalog install, one OS part build, standalone installs on
Fedora 44 and Ubuntu 24.04) — not part of this task.

---

## 10. Open questions (with recommended answers) and risks

Open questions:
- **Q1 (OWNER) privilege model** — section 3; recommend option A.
- **Q2 F1/F2 in the fork** — the cutover is blocked without F1. Recommend doing
  both as interim fork commits (D3 allows it) and drafting them as a small
  addition to U2/U4 for upstream later; no upstream post.
- **Q3 double image on installed systems** — recommend the OS builder stages
  the imported cache directly (`/var/lib/recipe/ginux/cache/oci/blobs/sha256/*`
  are content-addressed files, the same bytes `import-layout` commits) instead
  of shipping `.oci.tar` and importing on first run; the standalone release
  keeps download + import.
- **Q4 shared vs per-user state root** — recommend one shared root
  `/var/lib/recipe/ginux` (one image cache per host; names carry the uid);
  per-user roots would import 1.3 GB per user (Q12 of the interim design keeps
  caches per root).
- **Q5 builder: install to `/usr/libexec/ginux` or a wrapper** — recommend a
  root-owned wrapper under `/var/lib/levitate-builder/ginux/` (root-owned
  0755 chain) that exports `GINUX_RUNTIME`/`GINUX_IMAGE_TOOL` and execs the
  checkout's helper copy, with `GINUX_INSTANCE_HELPER` pointing at it (the
  e2e script's pattern, `test-public-lifecycle.py:172-180`); it never touches
  the host's `/usr/libexec`. Root caller, so no sudo rule is involved.
- **Q6 extract `/work` whole or only what is needed** — recommend `/work` and
  `/out` (identical archive shape to today, every reader unchanged); revisit
  if export size of build trees hurts (they were exported before too).
- **Q7 hosts file always** — recommend always (localhost lines), for one rule
  and one image rule (5.2); bumps behaviour anyway.
- **Q8 a sudo-less root path** — recommend asking nothing of the fork now;
  `sudo` must exist on build hosts and OS images (AcornOS: R4).

Risks:
- **R1** N13 and N16 are real safety regressions (host disk, any destination
  on a granted port, no caps); shipping the cutover means accepting them until
  U9/U11 exist. Said in `docs/runtime-limits.md` and the release notes.
- **R2** Catalog 2 without the seven 1002 syscalls may break builds under the
  native configured path (glibc mostly uses `newfstatat`, `pipe2`,
  `clock_nanosleep`, but `readv`/`sendfile`/`link` are common). If it does,
  the only fork-local fix is to upstream those catalog entries; keeping
  `catalog = 1002` would contradict the move.
- **R3** Whether `[contract.filesystem]` write/create lists and `[access]`
  are enforced by the native path for a non-systemd init is read from docs,
  not proven; if not, inputs are writable by the workload (no host effect).
- **R4** AcornOS: the helper needs GNU coreutils, util-linux `script`/`flock`,
  gawk, procps, `sudo`, and assumes systemd-style tmpfiles only through the
  RPM; this collides with "small and strict".
- **R5** Per-run cost on non-reflink filesystems (expand + `chown -R` of the
  whole image, then inject copies, then extract copies) may dominate OS
  builds; measure once in verification (migration doc section 8 risk 2).
- **R6** Console readability depends on F2; without it a user umask of 077
  silently yields no progress/diagnostic lines (results still arrive).
- **R7** `console.log`/`audit.log` grow without bound during a run.
- **R8** Offline builds: only declared download pins are injected; downloads
  computed at run time no longer see the offline cache.
- **R9** SELinux (Fedora 44) and sudo `use_pty` with binary stdin/stdout for
  `inject`/`import-image`/`extract` are unproven (interim design Q6).
- **R10** `recipe runtime remove` cannot free imported images or base roots
  (no public verb), contradicting the disk rule "remove frees everything".
- **R11** The CLI and image tool are not yet known to build as static musl;
  a glibc build ties the standalone release to the build host's glibc.
- **R12** All end-to-end behaviour of the fork's interim verbs is unrun
  (migration doc Stage 3 status); the first Recipe run is also their first
  real run.
