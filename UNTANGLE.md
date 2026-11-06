# Untangle log

## Scope

- Target: the `levitate-recipe` crate in `tools/recipe` only (`src/`, `tests/`, bin targets, and the crate's own docs/examples/scripts). Callers in the LevitateOS superproject (xtask, distro-builder, distro-builder/recipes) are out of scope; links to them are recorded only as cross-boundary notes.
- Additions chosen (2026-11-06): **Comments** (remove comments that only restate the code) and **Tests** (remove tests that wouldn't catch a real bug, and fix the ones that fail without one).
- Project rule that shapes the Tests addition: workspace and crate `AGENTS.md` prohibit unit tests (including assertion-based doctests and mock-only suites); focused integration/CLI/runtime tests are allowed.

## Baseline

- Repo: `tools/recipe` (its own Git repo, submodule of LevitateOS).
- Branch: `ginux-recipe/r06-launch`
- Commit: `022e91c5049350bf449847a2bc1365d067f1c290` ("fix(recipe): let final export use the execution deadline")
- Working tree: clean apart from untracked `.artifacts/` (pre-existing) plus this log and `.untangle/` (audit scripts).
- Build: `cargo build --all-targets` → succeeds, no warnings printed.
- The crate is a member of the LevitateOS Cargo workspace; artifacts land in `/home/vince/Projects/LevitateOS/target/`.

### Unprivileged test run (no Ginux launcher)

`cargo test --no-fail-fast` (from `tools/recipe`, no `GINUX_*` env) → exit 101.

| Target | Passed | Failed | Ignored |
| --- | --- | --- | --- |
| lib / `recipe` bin / `recipe-image` bin (unit harness) | 0 | 0 | 0 |
| `tests/e2e.rs` | 18 | 116 | 0 |
| `tests/image_producer.rs` | 1 | 0 | 0 |
| `tests/library_backend.rs` | 0 | 1 | 0 |
| doctests | 0 | 0 | 9 (all `ignore`d, in `helpers/internal/{fs_utils,hash,progress}.rs`) |

Every failure is environmental: 57 report `GINUX_BIN is not set`, 28 "managed CLI checks require GINUX_BIN", 15 "real Ginux launcher required", 3 `GINUX_*` env `NotPresent` unwraps, ~10 stderr assertions whose stderr is the same backend-startup error, and 4 `downloads::*` "did not connect to its HTTP fixture: Resource temporarily unavailable (os error 11)". The 18 e2e passes are: `cli::{test_cli_help, test_cli_help_includes_machine_events_flag, test_cli_version, test_cli_install_nonexistent_package}`, `managed_autofix::managed_execution_without_autofix_never_invokes_provider`, `query::{test_cli_info_shows_details, test_cli_list_empty, test_cli_list_recipes, test_cli_metadata_inherits_without_evaluating_setup, test_cli_metadata_reports_dynamic_values_without_execution, test_cli_metadata_rejects_malformed_declarations}`, `worker_entry::{private_worker_rejects_missing_bootstrap_before_recipe_setup, public_cli_worker_environment_never_selects_host_execution}`, `managed_inputs::{cli_invalid_worker_cleans_up_a_prepared_readonly_image, cli_missing_or_mismatched_root_image_fails_before_recipe_setup, cli_image_cannot_replace_invocation_paths_or_duplicate_files, cli_image_traversal_and_symlink_ancestors_preserve_host_files, cli_partial_launcher_frame_times_out_and_reaps_pipe_holders}`. Full log was kept in the audit session's scratchpad (`baseline-test.log`).

### Privileged managed run (real Ginux launcher)

The documented reproduction (superproject `docs/recipe-local-validation.md`), run at HEAD with copied immutable binaries:

```sh
sudo -n env RECIPE_TEST_BIN=<copy of target/debug/recipe @022e91c> RECIPE_WORKER_BIN=<same> \
  GINUX_BIN=<copy of /tmp/ginux-t24-hidden-devices> \
  GINUX_ROOTFS=$LEVITATE/.artifacts/ginux-env/prepared-image-git-musl/rootfs.tar \
  GINUX_ROOT=oci:recipe-compiler@sha256:9f220633d34efff68b1535375fbb4fa3c4c2c8843917859fd68d8f6edec27ff4 \
  <copy of e2e test binary> --test-threads=6
```

Result: **`tests/e2e.rs` 134 passed, 0 failed** (1782.6 s, `--test-threads=6`), exit 0. Run from the superproject root, so `library_backend`/publication side effects land in `$LEVITATE/.artifacts/ginux-export`.

`tests/library_backend.rs` needs `RECIPE_WORKER=1 RECIPE_INVOCATION=spoofed-public-call` in the test env (see its comment at `:6-8`). With those set and the same env as above: **1 passed**. Without them it fails at `library_backend.rs:8` (`NotPresent`).

**Behavioral baseline for the refactor: 134/134 e2e + 1/1 library_backend + 1/1 image_producer under the privileged environment; the unprivileged run result above.** The binaries used were copies taken at `022e91c`; rebuild and rerun the same command after each fix.

### CLI behavior snapshots

Saved under `.untangle/out/baseline-cli/`: `recipe --help`, `--version`, every subcommand's `--help`, `info examples/hello.rhai`, `-r examples list`, `hash examples/hello.rhai`, and an unprivileged `install examples/hello.rhai` (fails with the backend-startup error, exit 1). Compare these byte-for-byte after the refactor (they include ANSI colour codes and the user's home path).

## Map

Scripts (rerun from `tools/recipe`): `python3 -I .untangle/skeleton.py src > .untangle/out/skeleton.rs` (2.7k lines, ~95 KB), `python3 -I .untangle/deps.py > .untangle/out/deps.txt`, `bash .untangle/history.sh > .untangle/out/history.txt`.

### Areas (src ≈ 16.3k lines, tests ≈ 6.8k lines, 200 commits since 2026-01-16)

| Area | Files | Lines | What it is |
| --- | --- | --- | --- |
| Launch / worker | `launch.rs`, `backend.rs`, `worker/{mod,protocol,run}.rs`, `retained.rs`, `publication.rs`, `temporary.rs`, `cancellation.rs`, `options.rs`, `logging.rs`, `lib.rs` | ≈4.9k | Coordinator side starts the Ginux launcher, frames a protocol with the in-domain worker, retains checkpoints/logs, publishes outputs. Worker side runs the engine inside the domain. |
| Inputs | `inputs/{mod,config,discover,env,network,root}.rs` | 2.1k | Prepares the domain: root image, imported `--input`s, recipe discovery/imports, env, network grants, Ginux config. |
| CLI binaries | `bin/recipe.rs`, `bin/recipe/{cli,commands}.rs`, `bin/recipe-image.rs` | 1.1k | `recipe` CLI and the separate compiler-image producer. |
| Core engine | `core/{mod,executor,invocation,runner,build_deps,events}.rs`, `core/executor/private/*`, `core/output/*` | 2.4k | Rhai engine set-up, API2 context, acquire/build/install lifecycle with `is_*` checks, dependency recipes, events and narration. |
| Helpers (acquire/build/internal) | `helpers/mod.rs`, `helpers/acquire/*`, `helpers/build/*`, `helpers/internal/*` | ≈2.9k | Rhai-facing helper registration; download/verify/git/http; archive extraction; cmd/fs/hash/url/progress utilities. |
| Helpers (install/util) + LLM + autofix | `helpers/install/*`, `helpers/util/*`, `helpers/llm.rs`, `llm/*`, `core/autofix/*` | ≈2.6k | Filesystem/process/shell helpers; LLM provider config and invocation; `--autofix` prompt/parse/patch. |
| Tests | `tests/e2e.rs` + `tests/e2e/*` (23 files), `tests/image_producer.rs`, `tests/library_backend.rs` | 6.8k | CLI-level e2e suites; most need a real Ginux launcher. |
| Docs / packaging | 12 root `.md` files, `docs/` (incl. man pages), `bootstrap/`, `scripts/`, `Cargo.toml` | — | User and agent docs, RPM metadata, image manifests. |

### Hotspots (churn, last year)

`lib.rs` (47 commits), `tests/e2e.rs` (38), `bin/recipe.rs` (35), `tests/e2e/managed_inputs.rs` (27), `helpers/mod.rs` (24), `core/executor.rs` (21), `worker/run.rs` (17), `executor/private/{actions,flow}.rs` (15/14), `launch.rs` (14). Last 3 months: `lib.rs`, `worker/run.rs`, `launch.rs`, `executor/private/{actions,flow}.rs`, `inputs/mod.rs`, `backend.rs`. Much historic churn sits in modules since deleted (`src/engine/*`, `core/{lifecycle,deps,lockfile,version}.rs`), so the current structure is young: the Ginux managed-launch layer was added on top of an older in-process engine.

### Dependency graph

- Highest fan-in: `core::output` (11), `inputs` (9), `cancellation` (7), `logging` (7), `worker::protocol` (6), `core::executor` (6), `core::invocation` (6).
- Highest fan-out: `launch` (7), `worker::run` (6), `helpers::acquire::download` (6), `logging` (5), `inputs::root` (5).
- One module cycle: `core::invocation` ↔ `core::runner`.
- Direction smells: `logging` (crate-wide utility) imports `core::events`, `core::output`, `inputs`, `retained` and `worker::protocol`; `helpers::internal::cmd` (low-level process runner) imports `worker::protocol`; `inputs::discover` imports `core::executor`; `inputs::config` imports `worker`; every helper family imports `core::output` for narration.
- Two-sided protocol: `worker::protocol` is shared by `launch`, `backend`, `retained`, `logging`, `helpers::internal::cmd` and `worker::run`, so coordinator-side and domain-side code are bound through one module.

### Co-change (far pairs)

`bin/recipe.rs` ↔ `lib.rs` (15 commits), `core/executor.rs` ↔ `lib.rs` (7), `backend.rs` ↔ `lib.rs` (6), `bin/recipe.rs` ↔ `core/executor.rs` (6), `backend.rs` ↔ `worker/run.rs` (5), `bin/recipe.rs` ↔ `core/output.rs` (5), `lib.rs` ↔ `worker/run.rs` (5), `launch.rs` ↔ `inputs/mod.rs` / `worker/run.rs` (3 each), `bin/recipe/cli.rs` ↔ `backend.rs` / `worker/run.rs` (3 each). `lib.rs` is the hub: it changes alongside the CLI, the executor, the backend and the worker.

### Where leverage looks highest

1. The `lib.rs` / `backend.rs` / `worker::run` / `bin/recipe` cluster: invocation options flow through four places.
2. The coordinator ↔ worker boundary (`launch`, `worker::protocol`, `retained`, `logging`, `inputs::env`): env names, limits and timeouts are defined on one side and read on the other.
3. Limits copied between `bin/recipe-image.rs`, `inputs/root.rs` and `inputs/discover.rs`.
4. `helpers/mod.rs` registration versus helper docs (`HELPERS_AUDIT.md`, man page 7).

## Problems

**671 problems**: 355 hidden, 54 needless, 94 surplus, **168 special cases** (the number the refactor works to bring down).

Every entry has been moved under its fix in `## Fixes` (or under `### Leave as is`), keeping its ID. The section headings below record where each ID range came from.

### A. Launch / worker / crate root

<!-- more areas appended below as they arrive -->
### C. Core engine (`src/core/` minus autofix)

### D. Helpers: registry, acquire, build, internal

### B. Inputs and CLI binaries

(Items already logged under A/C are omitted: worker env names, the `RECIPE_WORKER_RECIPES` branch, operation aliases, the force-only-install rule, repeated `validate()`, the `valid_invocation_id` copy, the `config.toml` read-back, unused `PreparedInvocation` fields, the target knobs, the autofix-attempts default, unused `lib.rs` methods, the two launcher resolutions, and the `/inputs/main.rhai` literal.)

### E. Install/util helpers, LLM, autofix

(Items already logged under C/D are omitted: `fs_utils` duplicates and dead code, `RECIPE_TRACE_HELPERS`, Rust-name error labels, the `extends` resolver copies, the autofix reason strings, the PATH tool-dir copy, the `cmd.rs` → `worker::protocol` link, swallowed diagnostic errors, the double `description`, `df` bypassing `cmd`, `check_disk_space` using anyhow, and the copied attempts default.)

### F. Tests

### G. Docs, manifests, packaging, scripts

### H. Completeness pass 1: invocation hub (fresh reviewer, new items only)

A fresh reviewer of `lib.rs`, `backend.rs`, `launch.rs`, `worker/run.rs` and `bin/recipe*` found the items below that the first pass missed. They cluster around call ordering on error and success paths, operation-specific side effects and swallowed teardown errors.

### I. Completeness pass 1: engine lifecycle (fresh reviewer, new items only)

The first pass missed runtime semantics of the main-vs-dependency split and of operation asymmetries.

### J. Completeness pass 2: targeted at the missed patterns

Two reviewers looked for more of the pass-1 patterns in the state/inputs modules and in helpers/llm/autofix/output. Each pattern still produced new items (61 in total), mostly second-order consequences of clusters already listed. That shows the list has the right shape, so the audit stops here. A third pass would mostly add instances of known clusters, not new clusters. (The reviewer also noted that some line numbers in A for `temporary.rs`, `options.rs` and `network.rs` have shifted; re-locate by symbol.)

## Fixes

Generated from `.untangle/fixes.py` by `.untangle/apply_fixes.py`. Each fix appears in execution order, with every problem it removes listed under it by ID. `python3 -I .untangle/check_fixes.py UNTANGLE.md` confirms that every ID is in exactly one place.

**Review status:** the plan was shown to the user on 2026-11-06. The decisions they still need to make are listed in "Open questions" below. Until they answer, the plan proceeds with the defaults stated there.

### Requirements behind the special cases

The 168 special cases reduce to these requirements. The design never stated them, so each fix below is built to state them once:

1. **Every teardown step reports its own failure without masking the primary one.** This rules out swallowed reap, sync, drain and diagnostic-write errors, and also secondary failures that replace the real outcome. (A50, A51, A76, D62, H23, H24, I13, J29, J32, J41, J42 → X6, X7, X12)
2. **Cancellation is a typed outcome that every blocking wait observes, and library callers can trigger it.** This rules out reading a global at labelling time, sleep-polling and uncancellable copies. (A53, A55, A89, E69, B39, J35, J38, J61 → X6)
3. **Each failure has a machine-testable category and reason**, so classification never uses substrings and autofix eligibility never uses string matching. (A20, A21, A35, C48, C51, H28, H29 → X3)
4. **Operations differ in what they need and what they write.** Checks don't fabricate or commit state or resolve DNS; force flags belong only to install; cleanup carries its reason. (A56, H7, B28, C49, J39 → X3, X6)
5. **A recipe's role (main or dependency) is explicit and has its own policy:** force, outputs, work dirs, env and failure eligibility. (C24, C43, C51, I7, I8, I11 → X10)
6. **Phases follow one table of rules** for optionality, missing hooks, forced runs and post-verification. (C44–C47, I25, I26 → X10)
7. **Inputs have declared kinds** (file, tree, rootfs, restore target). Special behavior never keys off the names `rootfs` and `target`. (B45, B46, J34 → X9, X5)
8. **Where outputs and state go comes from the invocation**, not from the cwd, a shared default destination or a leaked tempdir. (A66, H20, B73, F36 → X6, X7)
9. **Every deadline and bound has one definition** that both sides of the boundary read. (A10, A46, A54, B17, B48, A73 → X3, X6, X7)
10. **Producer and consumer agree on artifact formats by sharing code:** the image, the archive and the export tar. (B84–B88, A67, J58 → X8, X5)
11. **Helpers behave the same in the same situation:** missing parents, zero matches, bad patterns, nonzero exits, reuse of existing results and staging before replacing. (E15, E16, E28, E36, J3, J4, J6, J9–J11, J15 → X8, X11, X12)
12. **A knob must reach the process that reads it.** Otherwise it doesn't exist. (D14, D32, E43, J21, J22 → X11)
13. **Crash leftovers are recognized by one rule per store**, and pruning never fails a new invocation. (A65, J43, J44, J46 → X7)

### Fix check (together / apart / direction)

- **Together:** each fix gathers one concept that is spread out today. Examples: the domain layout (X2), the cross-boundary contract (X3), the Ginux interface (X5), resolution (X9), the phase rules (X10) and the helper metadata (X11). The far co-change pairs from the Map (`lib.rs`↔`bin/recipe.rs`/`backend.rs`/`worker/run.rs`) collapse into X4's single `Invocation`.
- **Apart:** X4 splits `ExecutionOptions` by reason to change, and X13 splits `logging.rs` into a sink and a log store. X5 takes Ginux wire formats out of the orchestration in X6, and X9 takes host-side metadata out of the worker executor.
- **Direction after the fixes:** the leaves `domain`, `fsx`/`archive`/`image` and `sink` depend on nothing in the crate. `protocol` depends on `domain`. The coordinator side runs `ginux` and the stores → the run pipeline → `RecipeEngine`. The worker side runs `recipes` → the lifecycle → the helpers, all over the leaves. Today's inversions go away: `logging`/`cmd` → `worker::protocol`, `inputs::discover` → `core::executor`, in-domain code → `inputs`, and the `invocation`↔`runner` cycle. No fix adds an import toward a more volatile module.

### Order

X1 removes about 80 items with little risk and makes rustc report the remaining dead code. X2, X12 and X8 are leaves that the others build on. X3 comes next, then the coordinator side (X7 → X5 → X4 → X6), then the worker side (X9 → X10 → X13 → X11), and then X14. X16 and X15 come last, because they assert and document the final shapes. The Comments addition runs alongside each fix in the files it touches.

### Open questions for the user (defaults in bold)

1. **Library API.** No crate in the superproject links `levitate_recipe` as a library: they all run the `recipe` binary. X1 and X4 shrink the API and change `RecipeEngine` to return JSON instead of `rhai::Map`. **Default: allowed.**
2. **`ginux-domain` dependency.** X5 can validate the domain config with the upstream `tools/ginux` `ginux-domain` crate (a path dependency) instead of copying its limits. That couples the recipe build to the ginux checkout. **Default: keep hand-copied limits, centralized in X5, with a comment naming the upstream source.**
3. **Removing helpers.** X11 removes the `llm_*` helpers from the worker, because they cannot work there. The unexercised public helpers (D17) are left as is. **Default: remove `llm_*` from the worker; keep the rest.**
4. **Deleting docs.** X15 deletes the obsolete design notes and `APOLOGY.md`. **Default: delete; they stay in git history.**
5. **CLI flags.** X4 removes `--target-os`/`--target-arch` (each accepts only one value) and makes `--force-*`/`--llm-profile` subcommand-specific. distro-builder and xtask don't pass these flags. **Default: allowed.**
6. **Branch.** The recipe repo is checked out on `ginux-recipe/r06-launch`. Refactor commits land there, one per fix. **Default: as checked out.**

### 1. X1: Shrink the public surface and delete dead code

Make every module except the deliberate library API (`RecipeEngine`, options, errors) private or `pub(crate)`, so rustc reports dead code. Then delete it: unused functions and re-exports, dead fields, unreachable branches, non-unix fallbacks for a linux-only crate, transitional ticket scaffolding and stale doc references, and unused Cargo deps and features.

- **Solves:** 62 problems (5 hidden, 4 needless, 45 surplus, 8 special)
- **Done when:** `cargo build --all-targets` passes with no `dead_code`/`unused` allows added. None of the listed items exist. `lib.rs` exports only the intended API. `sha3`, `num_cpus` and `semver` and the clap `env`/ureq `json` features are gone or used.
- **Depends on:** none

- **A8** [surplus] `src/worker/run.rs:141-147`: the `RECIPE_WORKER_RECIPES` branch and `"."` fallback are dead config, with no setter in src, tests or the superproject.
- **A9** [special] `src/worker/run.rs:113-123`: `LocalConfig` exists for a transitional R05 harness ("until then the harness passes these…").
- **A17** [surplus] `src/inputs/mod.rs:82-84,99`: `PreparedInvocation::config_text`/`env` are kept in memory and also written to disk. Launch reads only the file. The fields are used only by `examples/prepare_inputs_demo.rs:25`, and `recipe_domain_path()` by nothing.
- **A41** [surplus] `src/worker/run.rs:629-634` and `src/backend.rs:224-228` are non-unix fallbacks in a crate that cannot build off unix (`libc::prctl`, `renameat2` used unconditionally).
- **A42** [special] `src/worker/run.rs:450-455`: a misattached doc ("Build the engine exactly like the host CLI does") claims host/worker parity that nothing enforces.
- **A44** [surplus] `src/launch.rs:103-115,355`: `LaunchReport::{result_signal,result_generation,log_tail}` are never read. `read_log_tail` re-reads a log whose tail the drain thread already computed (`:935-939`).
- **A45** [surplus] `src/launch.rs:86,124`: `pub run_domain`/`pub LaunchReport` in a `pub mod`, with `backend.rs:63` as the only caller.
- **A78** [surplus] `src/logging.rs:18-30`, `src/lib.rs:53-56`: `emit_hook_event`, `emit_hook_event_struct`, `set_machine_events`, `RECIPE_HOOK_EVENT`, `RecipeHookEvent` and `make_machine_hook_event` are pass-throughs/aliases of `core::output`/`core::events`, re-exported twice. Most have no users. The bin calls `output::set_machine_events` (`commands.rs:29`) while the worker calls `logging::set_machine_events` (`run.rs:256`).
- **A80** [surplus] `src/lib.rs:164-168` `with_network_hosts`, `:287-289` `cleanup` (no reason) and `:360-363` `recipes_path()` have no callers anywhere.
- **A82** [surplus] `src/backend.rs:40-41,106`: a redundant `{ … }` block and `let launcher = &self.path;` alias left over from a removed backend switch.
- **A88** [needless] `src/lib.rs:29-30,47`: `cancellation` and `worker` are `pub` only so the bin can reach `install_cli_handlers`, `signal`, `run_worker` and `WORKER_ENTRY_ARG`, which makes the whole wire protocol public API.
- **A91** [hidden] `src/worker/mod.rs:1-6`, `src/worker/protocol.rs:36,79-80`, `src/worker/run.rs:23,113-114,450-453` and `src/launch.rs:3-7,94,106` carry ticket labels (R01…G10) and "transport v2" (the actual version is v6). The docs record a lineage the code no longer has.
- **C53** [special] `src/core/invocation.rs:140-142` bails off-linux, duplicating `src/options.rs:51-61` (already run via `src/worker/protocol.rs:446`).
- **C62** [surplus] `src/core/executor/private/context.rs:56-62`: `tools_prefix_override()` and `tools_prefix()` always return `TOOLS_DOMAIN_PATH`, and `src/core/invocation.rs:53-58` defaults `None` to the same constant. These are three indirections, and `flow.rs:53` and `actions.rs:30,103,175` use them inconsistently.
- **C63** [surplus] `src/core/invocation.rs:103-107,126`: `PreparedScope.bindings` is always `Scope::new()`.
- **C68** [surplus] `src/core/build_deps.rs:42,45`: both `prepare_work_root` calls are no-ops (`context.rs:92`).
- **C70** [surplus] `src/core/mod.rs:6-11`, `runner.rs:8,13`: `pub` items inside the private `mod core` (`src/lib.rs:31`).
- **C71** [surplus] `src/core/output/narration.rs:17-19`: `action_numbered` ignores its counters and is unused.
- **C72** [surplus] `src/core/output/narration.rs:229-294`: `list_item`, `download_progress`, `build_spinner`, `spinner`, `progress_success`, `progress_fail` and `progress_done` are dead (only re-exported at `output/mod.rs:13-16`).
- **C73** [surplus] `src/core/output/logging.rs:11,21-50`, `src/logging.rs:14`: `EVENT_SINK`/`set_event_sink(_handler)` are never set, so `emit_to_sink` is a permanent no-op.
- **C74** [surplus] `src/core/events.rs:50-60,64-66,14`: `as_value`, `make_machine_hook_event` and the `Deserialize` derive are unused.
- **C75** [surplus] `src/core/events.rs:37-46,51-59`: the JSON fallbacks are unreachable (serializing five `String`s cannot fail).
- **C77** [surplus] `src/core/output/narration.rs:134,148-154`: `enforce_verb_in_sentence` never takes its prepend branch.
- **C79** [surplus] `src/core/build_deps.rs:48`: the comment refers to a nonexistent "durable S3 input identity".
- **D24** [special] `src/helpers/acquire/download.rs:67-70`: the `"download"` fallback filename is unreachable (`:39-41` already reject).
- **D28** [surplus] `src/helpers/acquire/verify.rs:120-125`: `compute_hashes` forwards to `internal::hash`, and `FileHashes` is re-exported twice. The only user is the CLI `hash` command (`src/bin/recipe/commands.rs:183`).
- **D29** [hidden] `src/helpers/acquire/verify.rs:10-15`: the module doc example is a stale copy of `download.rs:8-14`.
- **D33** [special] `src/helpers/acquire/http/api.rs:92-93,129-130,192-193`: the `*_with_base` variants exist "for testing" (unit tests are banned), and every caller passes `GITHUB_API_BASE`.
- **D34** [surplus] `src/helpers/acquire/http/api.rs:268-297`: `github_release_assets` is dead.
- **D35** [needless] `src/helpers/acquire/http/api.rs:20,93,130`: `pub(crate)` items are used only in-file.
- **D38** [surplus] `src/helpers/acquire/http/mod.rs:1-6` is a pass-through layer over one file.
- **D45** [surplus] `src/helpers/build/extract.rs:1-4` (a `#[path]` shim) and `build/mod.rs:11-14` are two pass-through layers over `extract_api.rs`.
- **D55** [needless] `src/helpers/build/extract_api.rs:215,301,359`: `pub(crate)`/`pub` items are used only in-file.
- **D57** [needless] `src/helpers/mod.rs:40` + `src/lib.rs:34`: the "NOT exposed" `internal` module (`src/helpers/internal/mod.rs:1-4`) is public API, which hides dead code from rustc. No external user exists.
- **D69** [surplus] `src/helpers/internal/progress.rs:43,66,114`: `create_byte_progress`, `create_download_progress` (an alias) and `with_spinner` are unused.
- **D70** [surplus] `src/helpers/internal/fs_utils.rs`: everything except `ensure_parent_dir` is dead (`glob_paths(_required)`, `set_mode`, `copy_file`, `move_file`, `is_safe_path`, `validate_safe_path`, the `Pipe` trait).
- **D72** [surplus] `src/helpers/internal/url_utils.rs` (191 lines) is entirely dead.
- **D73** [special] `src/helpers/internal/url_utils.rs:40-43` repeats the `git@` check from `:34-38` (dead).
- **D74** [special] `src/helpers/internal/url_utils.rs:73-81` uses a hard-coded TLD list (dead).
- **B35** [surplus] `src/inputs/config.rs:66` validates the compile-time constant `WORKER_INIT` at runtime.
- **B41** [surplus] `src/inputs/env.rs:16-20` (`TOOLCHAIN_ENV_ALLOW`) and `:135-167` (`validate_domain_value`) are reachable only via `extra_env`, and every caller passes `&[]` (`src/backend.rs:53`, `examples/prepare_inputs_demo.rs:24`).
- **B42** [hidden] `src/inputs/env.rs:80` doc says "seven fixed entries", but `:89-100` builds four.
- **B49** [hidden] `src/inputs/discover.rs:496-521` says its secret-path list is "mirrored from G10", but no such list exists anywhere in the workspace.
- **B50** [surplus] `src/inputs/discover.rs:303-305` `stage_named` only forwards to `stage_kind`.
- **B51** [surplus] `src/inputs/discover.rs:579-585` (`cfg(not(unix))`) and `src/inputs/mod.rs:158` are dead platform branches.
- **B59** [surplus] `src/inputs/mod.rs:164-178` writes `manifest.txt`, `sources.txt` and `env.txt`, which nothing reads. `render_manifest` (`:216-235`) and `StagedInput.referenced_as` (`discover.rs:64`) exist only to feed them.
- **B60** [surplus] `src/inputs/config.rs:166-175` `config_text` (pub, re-exported) has no callers and repeats `prepare_invocation`'s sequence.
- **B61** [surplus] The pub re-exports `DomainConfig`, `discover_inputs`, `hash_file`, `allowlisted_env`, `invocation_id`, `MAIN_NAME_DOMAIN` and `StagedInput` (`src/inputs/mod.rs:39-43`) have no users outside the module.
- **E12** [surplus] `#[cfg(not(unix))]` stubs at `src/helpers/install/filesystem.rs:105,205,390,395,409` and `src/helpers/internal/fs_utils.rs:68` cannot build (the crate is unix-only).
- **E21** [surplus] `src/helpers/install/disk.rs:88` `format_bytes` is dead, and `:23-24,35` hand-write the GB conversion twice beside it.
- **E22** [surplus] `src/helpers/install/disk.rs:47` `get_available_space` is `pub` but used only in-file.
- **E24** [surplus] `src/helpers/internal/fs_utils.rs:123-133`: a private `Pipe` trait used once (`:41`).
- **E55** [surplus] `src/llm/providers/claude.rs:47-48`, `codex.rs:87-88`: an empty map extended with a clone is just a clone.
- **E56** [surplus] `src/llm/provider.rs:21-27` `LlmProviderId::as_str` is dead.
- **E68** [surplus] `src/llm/runner.rs:99-101,121-127` check `max_input_bytes` twice, and the second check can never fire.
- **F54** [surplus] `examples/prepare_inputs_demo.rs:1-37` is unrun "reference only" code that must track `prepare_invocation`'s six-arg signature.
- **G44** [surplus] Unused crates: `sha3` (`Cargo.toml:25`), `num_cpus` (`:31`), `semver` (`:37`).
- **G45** [surplus] Unused features: clap `env` (`Cargo.toml:23`) and ureq `json` (`:34`).
- **H18** [surplus] `src/lib.rs:349-358`: `invocation_options` returns a `Result` that cannot fail, so six callers carry a needless `?`.
- **H19** [surplus] `src/lib.rs:165-168`: besides being unused, `with_network_hosts` is order-sensitive, because a later `with_execution_options` (`:159-162`) discards the hosts.
- **I28** [special] `src/core/build_deps.rs:3-4` documents cycle detection "before locking", but no locking exists in the worker.
- **J26** [hidden] `src/helpers/install/disk.rs:12` documents "creates if doesn't exist", but `:47-60` never creates.

### 2. X2: Name the Ginux domain layout once

Add a small, dependency-free `domain` module that owns every in-domain fact: the mount points (`/inputs`, `/work`, `/work/build`, `/work/tools` and its bin dirs, `/work/tmp`, `/out`), the writable roots, the main recipe name, the worker file name, `.recipe-input-modes.json` and its schema, the reserved input names, the worker argv shape and its env names (`RECIPE_WORKER`, `RECIPE_INVOCATION`), and derived length bounds. The coordinator (inputs, config, root, env, publication, retained) and the worker (run, invocation, executor, helpers) both import it, so neither side spells a path.

- **Solves:** 22 problems (19 hidden, 1 needless, 0 surplus, 2 special)
- **Done when:** A grep for the domain path literals and reserved names outside `domain` finds only tests asserting via the constants. The worker entry, the args written by config and the worker lookup all use one definition.
- **Depends on:** X1

- **A1** [hidden] `src/inputs/env.rs:97-98` sets `RECIPE_WORKER=1` and `RECIPE_INVOCATION`; `src/worker/run.rs:299` and `:305` read them through separate string literals. Renaming either side breaks worker bootstrap without any error, and unlike `WORKER_ENTRY_ARG` there is no shared constant.
- **A2** [hidden] `src/worker/run.rs:298` requires `args_os().count() == 3`, which holds only because `src/inputs/config.rs:93-96` puts exactly two args after argv[0]. Adding a worker arg breaks the check.
- **A3** [hidden] `src/worker/run.rs:134` hard-codes `/work/build`. The same path is a storage-assignment literal at `src/inputs/config.rs:136`, and `src/core/invocation.rs:45-63` builds the runtime paths (`/out`, `/work/tmp`) from separate literals. Only `/work/tools` has a constant (`inputs::TOOLS_DOMAIN_PATH`).
- **A4** [hidden] `src/publication.rs:346` allows outputs only under `work`/`out`, a copy of the writable storage assignments at `src/inputs/config.rs:136`.
- **A5** [hidden] `src/retained.rs:317` hard-codes `"/inputs/main.rhai"`, though `src/inputs/discover.rs:26` defines `MAIN_NAME_DOMAIN` and the worker also gets the path via argv (`src/inputs/config.rs:95`).
- **A6** [hidden] `src/retained.rs:144` builds identities as `format!("/inputs/{}")`, `src/worker/protocol.rs:478-485` checks the `"/inputs/"` prefix with a magic `path[8..]`, and the worker produces them from `compiled.recipe_path` (`src/worker/run.rs:549-553`). That is three places that must agree on the `/inputs` mount.
- **A7** [hidden] `src/worker/run.rs:141-147` reads `RECIPE_WORKER_RECIPES` for the module search path, but nothing sets it, and `src/inputs/env.rs:20,89-100` would reject it. It works only because `recipe.parent()` happens to equal `/inputs`.
- **C8** [hidden] `src/core/invocation.rs:60-63` hard-codes `/inputs`, `/out` (twice) and `/work/tmp`. They reappear in `src/inputs/env.rs:99`, `src/inputs/root.rs:146-150`, `src/inputs/config.rs:136` and `src/worker/run.rs:134`.
- **C9** [hidden] `src/core/executor/private/state.rs:36-41` prepends `tools/{usr/bin,usr/sbin,bin,sbin}` to PATH, but `src/inputs/env.rs:91-95` already puts the same dirs in the base PATH.
- **B1** [hidden] `src/inputs/config.rs:136` writes the storage assignments (`memory:/work`, `/work/build`, `/work/tools`, `/work/tmp`, `/out`) as one literal, and `src/inputs/root.rs:145-151` creates the same dirs. The `TOOLS_DOMAIN_PATH` constant (`src/inputs/mod.rs:50`) is bypassed at `config.rs:136`, so the constant does not really own the path.
- **B2** [hidden] `src/inputs/env.rs:155` accepts extra env values only under `/work/`, `/inputs/`, `/out/`, `/usr/`, `/bin/`, another copy of the domain layout.
- **B4** [hidden] The worker file name appears three times: `src/inputs/config.rs:31` `WORKER_INIT = "/inputs/recipe-worker"`, `src/inputs/root.rs:138` (write) and `src/inputs/discover.rs:538` (reserved).
- **B5** [hidden] `.recipe-input-modes.json` is written at `src/inputs/root.rs:164`, reserved at `src/inputs/discover.rs:538` and read at `src/helpers/install/filesystem.rs:235`. No shared name or schema ties them.
- **B7** [hidden] `src/inputs/env.rs:93` builds the worker PATH from the `TOOLS_DOMAIN_PATH` subdirs, a layout that dependency installs must follow without being told.
- **B18** [hidden] `src/inputs/discover.rs:28-30` `MAX_INPUT_NAME = 247` derives from ginux's 256-byte launcher path buffer, a cross-repo constant recorded only in a comment.
- **B63** [needless] `src/core/invocation.rs:55` and `src/core/executor/private/context.rs:57,61` import `inputs::TOOLS_DOMAIN_PATH`, so in-domain code depends on the coordinator's input module.
- **B82** [hidden] The worker entry (`src/bin/recipe.rs:37-43`), the args written at `src/inputs/config.rs:93-96` and the worker lookup at `src/inputs/root.rs:204-215` must all agree, and nothing says so.
- **E1** [hidden] `src/helpers/install/filesystem.rs:233-247` hard-codes `/inputs/.recipe-input-modes.json` and its `BTreeMap<String,u32>` shape, written by `src/inputs/root.rs:158-166`.
- **E2** [special] `src/helpers/install/filesystem.rs:233,269`: `copy_tree_contents` takes modes from the manifest when the source path starts with `/inputs`, a branch for one origin.
- **E3** [special] `src/helpers/install/filesystem.rs:238-242` has a magic 32 MiB manifest bound, written twice.
- **H12** [hidden] Worker argv is checked three times: `src/bin/recipe.rs:37-41`, `src/worker/run.rs:296-298` and `:127-130` (whose usage error is unreachable).
- **I3** [hidden] `src/inputs/discover.rs:30` (`MAX_INPUT_NAME = 247`, sized so `/inputs/` + name ≤ 255) and `src/worker/protocol.rs:479` (`path.len() <= 255`) are independently derived bounds.

### 3. X12: One subprocess runner and one diagnostic sink, owned low in the stack

Define a `DiagnosticSink` trait in a leaf module that depends on nothing. The worker installs a protocol-backed sink; the coordinator CLI installs a stderr sink that decides colour/TTY. All human output goes through it: narration, progress bars, hash progress, helper warnings, the backend summary and the LLM tee. `helpers::internal::cmd` becomes the only way to spawn a child. It owns env application order, bounded capture, routing, command lookup and a typed `ExitStatus` (exited code / signalled / spawn failed). `df`, autofix `git`, the LLM runner (dropping tokio) and every helper use it.

- **Solves:** 37 problems (21 hidden, 4 needless, 1 surplus, 11 special)
- **Done when:** `logging`, `core::output` and `cmd` no longer import `worker::protocol`. No `eprint!`/`eprintln!`/`ProgressDrawTarget::stderr` outside the sink implementations. No `Command::new` outside the runner. `-1` exit-code sentinels are gone. tokio is dropped or confined.
- **Depends on:** X1

- **A74** [needless] `src/logging.rs:34-36,44-53,76`: `logging`, which low-level code imports, depends on `worker::protocol::DiagnosticStream` and `retained::default_directory`.
- **A75** [hidden] `src/logging.rs:36-53` keeps a global `OnceLock` diagnostic sink that only `src/worker/run.rs:62` installs. Whether diagnostics go to stderr or frames depends on hidden process state.
- **A76** [special] `src/logging.rs:58` drops the sink error with `let _ =`, and the worker notices later via `channel.failed` (`src/worker/run.rs:263`).
- **C2** [needless] `src/core/output/logging.rs:75,78` and `src/core/output/narration.rs:12-60` print through `crate::logging::diag!`, while `src/logging.rs:14-30` re-exports and wraps `core::output`. This is a second `core::output`↔`logging` cycle (missed by `deps.txt`), and it drags `worker::protocol` and `retained` into narration.
- **C16** [hidden] `src/core/output/narration.rs:237-279` copies the templates and tick chars from `src/helpers/internal/progress.rs:22-77`.
- **C39** [hidden] `src/core/output/narration.rs:240,254,269` draw progress bars directly to stderr, bypassing the diagnostic sink.
- **D41** [hidden] `src/helpers/acquire/git.rs:22-93` → `src/helpers/internal/cmd.rs:42-59`: git's env scrub is safe only because `apply_environment` re-applies removals after `COMMAND_ENV` ("explicit wins"), and git.rs never says so.
- **D42** [special] `src/helpers/acquire/git.rs:190-192`: any nonzero `cat-file -e` (including corruption or a spawn error) is treated as "missing, fetch".
- **D47** [hidden] `src/helpers/build/extract_api.rs:424-433` re-implements `progress::create_spinner` (`src/helpers/internal/progress.rs:22-33`) verbatim.
- **D48** [hidden] `src/helpers/build/extract_api.rs:443-449` clears the bar by hand on two paths instead of using `ProgressGuard` (`progress.rs:91-103`).
- **D58** [needless] `src/helpers/internal/cmd.rs:11-40`: the per-invocation env state is written by `core` (`src/core/executor/private/state.rs:32-58`) and read by `util/env.rs:5` and `util/process.rs:58`. Writer and readers are split across folders.
- **D59** [hidden] `src/helpers/internal/cmd.rs:11-13,42-59`: `COMMAND_ENV` applies only through `cmd::*`. `src/helpers/install/disk.rs:62-65` spawns `df` directly and misses it, so "use cmd for every spawn" is an unwritten rule.
- **D60** [hidden] `src/helpers/internal/cmd.rs:36-40`: `environment()` falls back to `std::env`, a second global source.
- **D61** [needless] `src/helpers/internal/cmd.rs:88,100,130,145,206-209`: the low-level subprocess runner depends on `worker::protocol::DiagnosticStream` and `logging`.
- **D62** [special] `src/helpers/internal/cmd.rs:100,206-209` swallow diagnostic-forwarding errors.
- **D63** [special] `src/helpers/internal/cmd.rs:191`: `code().unwrap_or(-1)` maps a signal kill to -1, as do spawn failures in `src/helpers/util/shell.rs:29,34`, so callers can't tell them apart.
- **D64** [hidden] `src/helpers/internal/cmd.rs:114-115,180-182`: `description(&command)` is computed twice.
- **D66** [hidden] `src/helpers/internal/hash.rs:100-101,108,139-140,147` print progress with raw `eprint!`, bypassing `logging`/`core::output`.
- **D68** [hidden] `src/helpers/internal/progress.rs:45-50,73-78` copy the byte-progress template.
- **E18** [hidden] `src/helpers/install/disk.rs:36` warns with `eprintln!`, bypassing the worker diagnostic sink.
- **E26** [hidden] `src/helpers/util/env.rs:5`, `src/helpers/util/process.rs:58` and `src/helpers/internal/cmd.rs:36-59` read the thread-local `COMMAND_ENV`, whose correctness depends on an executor guard being alive on the current thread (`src/core/executor/private/state.rs:33-58`).
- **E27** [hidden] `src/helpers/internal/cmd.rs` has three output policies: `stream` (`:113`, live with a bounded tail), `capture` (`:194`, silent, unbounded) and `checked_capture` (`:203`, stderr after exit, unbounded). The rpm/dnf/apk queries (`process.rs:17-21`) use `capture`, so their output is unbounded and invisible.
- **E28** [special] `src/helpers/util/shell.rs:27-35` maps spawn failure to `-1` ("legacy convention"), the same as signal death (`cmd.rs:191`), while `exec` (`process.rs:228-233`) throws on spawn errors.
- **E29** [hidden] `src/helpers/util/shell.rs:39` decodes UTF-8 strictly, while `src/helpers/util/process.rs:269` decodes lossily.
- **E39** [special] `src/helpers/util/process.rs:57-67` `command_path` ignores the exec bit.
- **E67** [hidden] `src/llm/runner.rs:157-162` tee straight to tokio stderr, bypassing the diagnostic sink.
- **E69** [special] `src/llm/runner.rs:74-75` polls the global cancellation every 100 ms.
- **E71** [special] `src/llm/runner.rs:179` maps a signal kill to exit code 1.
- **E72** [hidden] `src/llm/runner.rs:87-194` and `src/helpers/internal/cmd.rs:113-178` are two spawn/drain/bound/report implementations (tokio vs threads) with different bounds, env and routing. tokio is pulled in only for `llm/`.
- **E89** [hidden] `src/core/autofix/prompt.rs:36-51` and `src/core/autofix/patch.rs:230-248` spawn `git` with raw `Command`s, a fourth subprocess path.
- **H30** [special] `src/backend.rs:72-88` prints an unconditional `eprintln!` summary that library callers get too, bypassing `core::output`.
- **J13** [hidden] `src/core/autofix/prompt.rs:37-41`, `patch.rs:235-240`: the coordinator's `git rev-parse`/`git apply` inherit `GIT_DIR`/`GIT_WORK_TREE`/`GIT_INDEX_FILE`/`GIT_CONFIG*`, which `src/helpers/acquire/git.rs:53-84` deliberately strips.
- **J15** [special] `src/helpers/util/shell.rs:22-24` → `src/helpers/internal/cmd.rs:163-170,191`: `shell_status` maps any `run_status` error to -1, including drain failures after a clean exit 0.
- **J16** [special] `src/llm/runner.rs:167-170`: an early provider exit with a large prompt returns EPIPE "Failed to write to subprocess stdin" and drops the provider's real stderr.
- **J20** [hidden] `src/helpers/internal/progress.rs:22-33` (used by `download.rs:203`, `git.rs:244`, `extract_api.rs:425-433`): indicatif hides itself when stderr is not a TTY, so in the worker these bars never render, while the raw `eprint!` hash progress (`hash.rs:100,139`) does reach `supervisor-stderr.log`.
- **J56** [hidden] `src/retained.rs:45` and `src/logging.rs:101` use raw `eprintln!`, and coordinator-side lines never reach the durable log dir.
- **E66** [surplus] `src/llm/runner.rs:47-63` `TeeTarget` has a single variant.

### 4. X8: Shared secure-filesystem, archive and image primitives

One `fsx` module holds the primitives each copied today: open regular file without following links, create or accept a private 0700 dir, make a tree removable, bounded hash-copy, relative-path safety, apply dir modes after fill, hardlink-preserving materialization, reflink-or-copy, and remove-if-exists. One `archive` module owns the tar/zip/apk extraction policy (one stay-inside-dest rule, correct hardlink resolution against dest, one format table, one bounds formula). One `image` module owns the rootfs image contract (limits, entry kinds, links ordering, reserved paths, ownership/mode rules), used by both `recipe-image` (declared as a bin and packaged) and `inputs::root`.

- **Solves:** 34 problems (21 hidden, 4 needless, 2 surplus, 7 special)
- **Done when:** Each primitive has one implementation. The hardlink check and extraction agree. `recipe-image` imports the library's image module, and its limits and validation equal the consumer's. `recipe hash` uses the hashing primitive directly.
- **Depends on:** X1

- **A69** [hidden] `src/publication.rs:352-370`, `src/retained.rs:354-368`, `src/logging.rs:80-89` and `src/temporary.rs:393-401` are four "create a private 0700 dir, accept an existing real dir" implementations.
- **A70** [hidden] `src/publication.rs:13-26` and `src/temporary.rs:491-503` are the same chmod-0700-walk-before-removal.
- **A72** [hidden] `src/retained.rs:331-340` `regular`, `src/publication.rs:65-72` and `src/bin/recipe-image.rs:232` `regular` are copies of the open-nofollow-and-require-a-regular-file helper. `src/retained.rs:369-390` `copy_hash` duplicates `src/inputs/root.rs` `copy_hashed` (`:59,96,139`).
- **B3** [hidden] `src/inputs/root.rs:305` rejects image entries under `inputs`/`work`/`out` (the reserved layout), while `src/bin/recipe-image.rs` never checks them, so the producer can publish an image the consumer rejects.
- **B58** [hidden] "Bounded open-regular-and-hash/copy" is written three times in inputs (`src/inputs/root.rs:219-250`, `src/inputs/discover.rs:598-619`, `src/inputs/mod.rs:144-157`), on top of the copies in `retained.rs` (see A).
- **B77** [needless] `src/bin/recipe/commands.rs:183` reaches into the Rhai helper module (`helpers::acquire::compute_hashes`).
- **B84** [hidden] Producer/consumer size limits disagree: `src/bin/recipe-image.rs:13-15,215` allows a 32 GiB `rootfs.tar`, but `src/inputs/root.rs:16,59` caps the archive at 8 GiB and `:17,336` caps unpacked bytes at 16 GiB.
- **B85** [hidden] `src/bin/recipe-image.rs:13-15` `MAX_ENTRIES`/`MAX_BYTES`/`MAX_TOTAL` copy `src/inputs/root.rs:18` and `src/inputs/discover.rs:32-33` instead of sharing them.
- **B86** [hidden] `src/bin/recipe-image.rs:197-211` writes additions in BTreeMap order and checks only that a hardlink target is in the same archive (`:372-380`). The consumer requires an earlier entry (`src/inputs/root.rs:366-368`), so a later-sorting target, or a base hardlink to a path an addition replaced (skipped at `:182-184`), yields an image the consumer rejects.
- **B87** [hidden] `src/bin/recipe-image.rs:232-241,392-409,272-286` (`regular`, `hash_copy`, `safe_name`) duplicate `root.rs` `regular_file`/`copy_hashed`/`relative_path`, minus cancellation and O_NONBLOCK.
- **B88** [hidden] `src/bin/recipe-image.rs:287-314` `validate_entry` is a second copy of the consumer's entry-kind and link rules (`src/inputs/root.rs:319-383`), with different symlink handling.
- **B89** [needless] `recipe-image` imports nothing from the library, exists only through Cargo autobins (`Cargo.toml:15-17` declares just `recipe`) and is missing from the RPM assets. The feature is spread across `src/bin`, `bootstrap/`, `src/inputs/root.rs` and README.
- **B92** [special] `src/bin/recipe-image.rs:54,60-67` hard-code the manifest limits as unnamed numbers.
- **B93** [special] `src/bin/recipe-image.rs:110,226` ignore the `kill()` and `output.keep()` results.
- **B94** [hidden] Ownership and mode conventions are split: the producer writes uid/gid 0 and mtime 0 (`src/bin/recipe-image.rs:383-388`), the consumer masks modes with 0o555 (`src/inputs/root.rs:331,348`), and `seed_target` writes uid/gid 1000 (`root.rs:432-434`).
- **D46** [hidden] The format list is copied in `src/helpers/build/extract_api.rs:359-378` (detect) and `:435-442` (dispatch, plus aliases), with doc copies at `build/mod.rs:8` (which omits tar and apk) and `extract_api.rs:383`.
- **D49** [hidden] `src/helpers/build/extract_api.rs:136-212` (tar) and `:301-352` (zip) implement the "stay inside dest" policy differently: only tar re-checks symlink parents and validates links, and zip has a redundant `ParentDir` check. A third copy, `src/helpers/internal/fs_utils.rs:104-110`, is dead.
- **D50** [hidden] `src/helpers/build/extract_api.rs:175-181` validates hardlink targets relative to the link's parent, but `entry.unpack(&full_path)` (`:206-208`, tar-rs `unpack(None, …)`) resolves them relative to the process cwd (tar-0.4.46 `entry.rs:203-205,529-532`). Validator and extractor disagree on the path.
- **D51** [special] `src/helpers/build/extract_api.rs:414`: `ensure_no_symlink_components(dest, dest)` passes the same path twice to reuse the ancestor walk.
- **D54** [hidden] `src/helpers/build/extract_api.rs:215-257`: five near-identical `extract_tar_*` functions.
- **D65** [hidden] `src/helpers/internal/hash.rs:76-112,115-151,154-178` are three hash loops (two copies of the progress loop, plus a third with an 8 KiB buffer instead of `CHUNK_SIZE`).
- **D67** [special] `src/helpers/internal/hash.rs:48` swallows a metadata error.
- **D71** [hidden] `src/helpers/internal/fs_utils.rs:17-26` `ensure_parent_dir` is duplicated privately at `src/helpers/install/filesystem.rs:49-64`. The dead `fs_utils::{copy_file,move_file,set_mode}` duplicate `install::filesystem::{copy_file,move_file,chmod_file}` (`:150,358,402`) and already differ (only the fs_utils `move_file` creates the parent).
- **E6** [hidden] "Remove destination, ignore NotFound" appears three times with different errors (`src/helpers/install/filesystem.rs:182-193,302-306,376-385`).
- **E7** [needless] `src/helpers/install/filesystem.rs:68,74`: the shared `copy_file_impl` errors name `copy_file` for all five callers.
- **E8** [needless] `src/helpers/install/filesystem.rs:196,207`: the general `copy_symlink` errors name its one caller, `copy_tree_contents`.
- **E9** [special] `src/helpers/install/filesystem.rs:167-170`: `copy_file_reflink` swallows every reflink error (including permission errors) and falls back to a copy.
- **E10** [surplus] `src/helpers/install/filesystem.rs:158-166`: the reflink path repeats the source check and `ensure_parent_dir`, then `copy_file_impl` (`:67-71`) repeats both and logs a second line.
- **J1** [hidden] `src/helpers/install/filesystem.rs:90-98,167-170`: `copy_file_reflink` truncates the destination before trying FICLONE, so a failed clone plus a failed fallback leaves a truncated file.
- **J2** [hidden] `src/helpers/install/filesystem.rs:294-312`: in `copy_tree_contents`, the symlink and hardlink branches remove the destination first, but the regular-file branch writes through whatever is there (following an existing symlink out of `dst_dir`, or truncating a shared inode).
- **J3** [special] `src/helpers/install/filesystem.rs:182-194,376-387`: `ln_force` and `copy_symlink` remove the old link and then create the new one with no staging, unlike the stage-then-persist in `download.rs:216-263` and `git.rs:237-250`.
- **J7** [special] `src/helpers/install/filesystem.rs:72,91-95`: `copy_file_reflink`'s resulting mode depends on whether the reflink succeeded (umask/old mode vs the source mode).
- **J8** [hidden] `src/helpers/build/extract_api.rs:206-208,332-346`: tar (tar-rs `unpack`: 0o777 mask, mtime and symlinks kept) and zip (`unix_mode()` incl. setuid, symlinks written as text files, no mtime) produce different trees from the same `extract()` call.
- **J27** [surplus] `src/helpers/install/filesystem.rs:70,294`: `ensure_parent_dir` runs twice per file in `copy_tree_contents`, though WalkDir already created the parents.

### 5. X3: Typed coordinator/worker contract

Make `worker::protocol` the single owner of what crosses the process boundary, with types instead of strings:
- `Operation` enum (`Install`, `Remove`, `Cleanup{reason}`, `Check(Phase)`), with aliases parsed once at the CLI edge.
- `FailureReason` (phase × outcome) and `FailureCategory` (startup/protocol/workload/supervisor/deadline/storage/cancelled) enums with `Display`.
- A single failure report (no duplicated frame plus `result.failures`).
- A `ValidatedOptions` newtype built once.
- An `InvocationId` type.
- Shared limits and timeouts carried in the request (ack timeout, diagnostic budget as one shared function).
- `Option<Generation>` instead of 0.
- One state channel: every recipe's state goes via checkpoints, and the final result carries outputs only.
Autofix eligibility becomes a method on `FailureReason`. The worker's exit-code convention is replaced by the typed result.

- **Solves:** 44 problems (25 hidden, 4 needless, 5 surplus, 10 special)
- **Done when:** No operation, reason or category string literals outside `protocol`'s `Display`/`FromStr`. `validate()` runs once. The force-only-install rule exists once. `enrich_stream_error` matches on types. Worker aliases are gone.
- **Depends on:** X2

- **A10** [hidden] `src/worker/run.rs:111` `ACK_TIMEOUT` is a fixed 300 s, while the host frame/ack timeout is configurable (`src/launch.rs:63,600-614`, `RECIPE_FRAME_TIMEOUT_SECS`). Raising the host value does not raise the worker's.
- **A11** [hidden] `src/launch.rs:459-489` re-implements the worker's diagnostic-budget accounting from `src/worker/run.rs:73-93`: byte/frame counts, when a truncation marker is legal, and nothing after truncation.
- **A12** [hidden] `src/worker/protocol.rs:270` writes `"1..=768"` into error text, copying `MAX_DIAGNOSTIC_CHUNK` (`:70`).
- **A20** [special] `src/launch.rs:773-776` `enrich_stream_error` classifies failures by substring-matching the raw result text for `"supervisor-error"` and the prefix `"backend protocol failure"`, even though `read_result_record` already parses the file.
- **A21** [hidden] Failure categories exist only as message prefixes (`backend startup|protocol|workload|supervisor|deadline|storage failure`, `backend cancelled`), across about 50 literals in `src/launch.rs`, `src/backend.rs:44,58,97,101` and `src/cancellation.rs:30`. They are parsed at `launch.rs:775` and asserted in `tests/e2e/{worker_entry,managed_inputs}.rs`.
- **A22** [hidden] `src/cancellation.rs:30` and `src/launch.rs:742-745` each format `"backend cancelled: interrupted by signal {}"`.
- **A23** [special] `src/launch.rs:32-37` documents three failure categories, but the code emits seven. `:4` mentions a "backend config" that doesn't exist.
- **A28** [needless] `src/backend.rs:29-39` takes the operation as `&str`. The operation set is spelled out as strings in `src/lib.rs` (×6), `backend.rs:45`, `src/inputs/mod.rs:53-60,196-211`, `src/worker/protocol.rs:447` and `src/worker/run.rs:372-444`, and the "want install|remove|…" error is duplicated at `inputs/mod.rs:209` and `worker/run.rs:440`.
- **A29** [special] `src/worker/run.rs:404,414,424` accept the aliases `isinstalled`/`isbuilt`/`isacquired` that never arrive, because the request is normalized at `src/inputs/mod.rs:196-207`.
- **A30** [hidden] `src/backend.rs:45` and `src/worker/protocol.rs:447-451` each implement "force options apply only to install", with the same message.
- **A31** [hidden] `ExecutionOptions::validate` runs six times per invocation (`src/bin/recipe/commands.rs:235`, `src/backend.rs:42-44`, `src/inputs/mod.rs:122`, `src/inputs/config.rs:62`, `src/worker/protocol.rs:446`, and worker receive). No single owner validates the options.
- **A32** [hidden] `src/worker/protocol.rs:437-443` `valid_invocation_id` duplicates `src/inputs/env.rs:59-75` `validate_invocation_id` (`[A-Za-z0-9_-]{1,64}`).
- **A33** [surplus] `src/worker/protocol.rs:137-138` `recipe_digest` is sent (`src/launch.rs:411`) but never read or validated.
- **A34** [surplus] `src/worker/run.rs:270-288,639-652` sends each failure twice (a `failure` frame and `result.failures`). `src/backend.rs:90-96` rebuilds `WorkloadFailure` by mixing both, and nothing checks that they agree.
- **A35** [special] `src/backend.rs:93` defaults `fatal` to `true` when no failure frame arrived, which silently disables autofix.
- **A36** [hidden] `src/lib.rs:229-233` decides autofix eligibility by matching the reason strings `auto.build.failure|auto.install.failure` produced in `src/core/executor/private/flow.rs:336-343`. Renaming a reason silently disables autofix.
- **A37** [needless] `src/worker/protocol.rs:149`, `src/lib.rs:349-358`: `cleanup_reason` travels on every request but only cleanup uses it (`src/worker/run.rs:399`).
- **A40** [hidden] `src/worker/run.rs:600-609` `wait_readable` polls fd 0 directly, while `wait_ack` takes a generic `impl Read`. It works only if the input is stdin.
- **A58** [hidden] `src/retained.rs:110-111` sets `state_budget = MAX_FRAME_LEN - (defines JSON + 8192)`. The 8192 is a hand-sized reserve for the request's other fields (`cleanup_reason` ≤ 4096 at `src/worker/protocol.rs:168`).
- **A63** [hidden] `src/retained.rs:16` `MAX_STATE`, `src/logging.rs:177-178` `4*1024*1024` and `src/worker/protocol.rs:60` `MAX_FRAME_LEN` are the same 4 MiB bound in three unconnected places.
- **B13** [hidden] `MAX_ID = 64` is defined twice (`src/inputs/env.rs:14`, `src/inputs/config.rs:15`), and `config.rs:192-200` checks the length and then calls `validate_invocation_id`, which checks it again.
- **B26** [surplus] `src/inputs/mod.rs:199-203` re-searches `SUPPORTED_OPERATIONS` after the match arm already matched, with an unreachable `.unwrap_or("install")`.
- **B27** [needless] `src/inputs/mod.rs:114-124`: `prepare_invocation` takes and normalizes `operation` only to store it for `launch.rs:291`, so input staging changes with the operation vocabulary.
- **B29** [hidden] The cleanup-reason default `"manual"` appears in `src/bin/recipe/cli.rs:142`, `src/bin/recipe/commands.rs:111`, `src/lib.rs:217,277,288,314,328,342` and `src/worker/protocol.rs:157`.
- **B43** [special] `src/inputs/env.rs:37-40` swallows a clock error (millis = 0).
- **B44** [special] `src/inputs/env.rs:34-36,50-52` substitute "recipe" for an empty stem, handled twice.
- **B62** [needless] `src/logging.rs:149` and `src/worker/run.rs:307` import `inputs::validate_invocation_id` for a protocol-owned rule.
- **C11** [hidden] `src/core/executor/private/flow.rs:250,343,402,438` produce `auto.{acquire,build,install}.failure`. `src/lib.rs:232` matches them for autofix, and `tests/e2e/downloads.rs:99` and `git_acquisition.rs:173` match them inside recipe hooks.
- **C31** [hidden] `src/core/executor/private/context.rs:13-15` speaks of "acknowledgement 0", which only means something given `UNGROUNDED_GENERATION = 0` in `src/launch.rs:82`.
- **H1** [hidden] `src/lib.rs:227` can only downcast a `WorkloadFailure` (and so allow autofix) when the worker exits 0 after its failure frame (`src/worker/run.rs:9,207-210`). If the worker exits nonzero, `src/launch.rs:1110-1126` bails with a plain string and autofix eligibility is silently lost. Autofix depends on an exit-code convention in another process.
- **H2** [hidden] `src/worker/run.rs:72`: the diagnostic sink writes to a hard-coded `std::io::stdout()`, while checkpoint and result frames go to the generic `output: &mut impl Write` (`:336,523`). Both must be the same stream, because they share one `FrameSender` sequence counter.
- **H3** [hidden] `src/worker/run.rs:575,600-626` polls raw fd 0 before `rx.recv` on a buffered `StdinLock` (`:215-216`). This is correct only because the host never pipelines frames (one ack per checkpoint, `src/launch.rs:516-526`).
- **H17** [surplus] `src/worker/run.rs:757-766` `payload_name` re-enumerates what `FramePayload::op()` maps (`src/worker/protocol.rs:288-299`).
- **H21** [special] `src/bin/recipe/commands.rs:107-111` trims the cleanup reason and maps empty to `manual` on the CLI path only. `cleanup_with_reason` library callers get no normalization.
- **H27** [special] Generation `0` means "ungrounded" in `src/launch.rs:82,513` and "no record" in `src/retained.rs:157`: the same magic value assumed independently.
- **H28** [special] `src/worker/run.rs:211-230,255`: pre-request failures return `Err` with no frames (seen as a protocol "exit without result"), while bad requests get `abort` frames (a workload failure).
- **H29** [special] `src/worker/run.rs:176,199,438,640`: `EngineFailure` reasons mix `fatal`/`workload`/`startup`/`protocol` with the `auto.*` strings, with no enumeration, and `abort` reports phase `""`.
- **I5** [hidden] State reaches the coordinator through two channels: checkpoint frames carry every recipe's state (`src/core/executor/private/context.rs:159` → `src/worker/run.rs:533-560`), while the final `ctx_summary.state` is committed under the hard-coded main identity (`src/launch.rs:335-344`, `src/retained.rs:317`). Dependency state persists only via checkpoints, and check operations (which never checkpoint) only via the final result.
- **J50** [hidden] The invocation id crosses the boundary three ways (config `[domain] id` `src/inputs/config.rs:90,118`, env `RECIPE_INVOCATION`, request `invocation_id` `src/launch.rs:408`), and the worker compares only env against the frame (`src/worker/run.rs:231`).
- **J51** [hidden] Restored state is split: `ctx.state` travels in `Request.restored_states` (`src/launch.rs:413-416`) and the filesystem as `--restore-file` (`:200-209,259`). They pair only because both read `retained.current` at the same moment, and the worker is never told which restore mode applied.
- **J52** [hidden] `persist_state` is expressed three ways: the request field (`src/launch.rs:410`), `Retained` being `Some` (`:188`) and the presence of `--control-fd` (`:211-215,256`).
- **J59** [hidden] Invocation-id uniqueness is assumed separately: `src/logging.rs:95` fails before the run, while `src/publication.rs:307-318` fails with `RENAME_NOREPLACE` after it. `src/inputs/env.rs:37-41` (millis may be 0, plus pid) is the only source of uniqueness.
- **J60** [hidden] `src/inputs/network.rs:21`: `NetworkPolicy::resolve` calls `options.validate()`, a seventh validation site.
- **A43** [surplus] `src/launch.rs:539-544` is an unreachable `Request`/`Ack` arm, already rejected by `check_direction` at `:447-454`.

### 6. X7: Stores that own their layout: state, logs, scratch, destination scope

`retained` owns its record schema with a single validator, generation layout names and an `Identity` struct hashed whole. The struct includes `protocol::VERSION`, a typed identity subset of options (from X4's split) and the network policy. The destination lock and default-destination become an explicit `Scope` concept (per-recipe default, no leaked tempdirs) instead of a fake `build_dir`. The invocation log store moves out of `logging` into its own module with a named retention constant, a `result.json` status enum and file names it owns. `temporary` and the log store share one leased-directory-pool implementation. One function resolves the state, cache and recipes roots.

- **Solves:** 34 problems (20 hidden, 3 needless, 2 surplus, 9 special)
- **Done when:** One `Record` validator. No generation layout literals outside retained. Adding an option forces an identity decision at the type level. One pool implementation. Default destinations no longer collide.
- **Depends on:** X8, X3

- **A13** [hidden] `src/launch.rs:261,285` create `worker.log`/`supervisor-stderr.log` in the invocation log dir, while `src/logging.rs:157` keeps its own literal list of allowed names to decide prunability. A new file makes every log dir unprunable.
- **A14** [needless] `src/logging.rs:68` exposes `InvocationLog.directory` as a `pub` field, and `launch.rs` creates files in it directly, so callers depend on the internal layout.
- **A15** [hidden] `src/logging.rs:184` prunes by the `result.json` schema, which is written in two places: `src/logging.rs:100` (`running`) and `src/launch.rs:144-153` (`succeeded`/`workload-failed`/`cancelled`/`failed`). The status vocabulary is split across modules.
- **A59** [hidden] `src/retained.rs:126`: the identity tag `"recipe-state-v1-transport-v6"` copies `PROTOCOL_VERSION = 6` (`src/worker/protocol.rs:57`) as text.
- **A60** [hidden] `src/retained.rs:125-138`: the state identity hashes a hand-picked subset of `ExecutionOptions`. A new field in `src/options.rs:7-31` needs an inclusion decision, and nothing prompts it.
- **A61** [hidden] `src/retained.rs:156-163` (strict) and `:398-407` (lenient) are two validators for the same `Record`, both checking version 1.
- **A62** [hidden] The generation layout literals `g-{generation}-`, `files.tar` and `record.json` repeat at `src/retained.rs:165-175,243-245,276,289,417,442,452,504`.
- **A64** [needless] `src/retained.rs:24-56` puts the destination build-dir lock in the state-record module. The host `destination` is really only a lock and scope key (the worker always builds in `/work/build`), but the public `build_dir` (`src/lib.rs:92,109`) hides that.
- **A65** [special] `src/retained.rs:446-458` recognizes a crashed pending snapshot as "a dir containing exactly `files.tar`".
- **A71** [hidden] `src/temporary.rs:477-489` `lease_file` and `src/logging.rs:120-135` `log_lease` are the same leased-file opener. The whole "leased directory, prune what's unlocked" design is implemented twice (`temporary.rs:402-446`, `logging.rs:136-205`).
- **A73** [special] `src/logging.rs:197` `skip(31)` is an inline retention magic number. State retention uses named constants (`src/retained.rs:395-396`).
- **B25** [hidden] Three default roots are resolved three ways: `src/bin/recipe/metadata.rs:45-58` (hand-read XDG), `src/retained.rs:18-21` (`dirs::state_dir`) and `src/inputs/root.rs:44` (`dirs::cache_dir`).
- **B32** [hidden] `src/inputs/network.rs:304` derives `Serialize`, and `src/retained.rs:127` hashes it into the state identity, so adding a field changes the identity.
- **B72** [hidden] `src/bin/recipe/commands.rs:247-253` hard-codes `<state>/default-destination` inside the library-owned state dir, and `src/bin/recipe.rs:25` restates it.
- **B73** [special] `src/bin/recipe/commands.rs:243-246`: `tempdir().keep()` for `--no-persist-state` leaks a host dir on every run.
- **H8** [hidden] The state-root layout is spread across `src/bin/recipe/commands.rs:248-252` (`default-destination`), `src/retained.rs:91-103` (`<scope-hash>/<identity>`) and `src/logging.rs:74-79` (`logs`), each calling `retained::default_directory()` itself.
- **H20** [special] `src/bin/recipe/commands.rs:247-253`: without `--build-dir`, every recipe shares `<state>/default-destination`, so unrelated invocations serialize on one lock (up to 300 s) and share a retained scope.
- **J28** [hidden] `src/retained.rs:295-299,436-470`: `commit` keeps the `g-N-*` dir before persisting `current.json`. If that persist fails, the next `begin()` reuses N, and `prune` may keep the orphan instead of the real previous generation.
- **J29** [special] `src/retained.rs:301-302`: a `prune()` error after the new generation is durable fails the checkpoint/finish (no ack, run errors), yet the next run restores from that generation.
- **J30** [hidden] `src/logging.rs:78` and `src/retained.rs:25` create the state root with `create_dir_all` (umask) before `Retained::open` (`src/launch.rs:133,189`, `retained.rs:96,341-353`), so `durable_directory`'s 0700+fsync intent for the root never applies.
- **J33** [hidden] `src/temporary.rs:14-16,101-104`: the comment says the lease is held until the supervisor releases it, but `Drop` deletes the tree regardless. Safety depends on launch reaping first, which `src/launch.rs:305-307` may skip.
- **J36** [hidden] `src/temporary.rs:47-59` removes lease-less dirs as crashed creations, while `src/logging.rs:163-165` skips lease-less log dirs as foreign. The two pool pruners disagree on what a missing lease means.
- **J37** [special] Three lock policies: `src/temporary.rs:33-35` and `src/logging.rs:91-92` block in `lock_exclusive()` with no timeout or cancellation, while `src/retained.rs:37-55` polls with a deadline, a cancellation check and a message.
- **J40** [hidden] `src/inputs/network.rs:15,22-23,65` + `src/retained.rs:127`: `network_max_bytes` enters the state identity only when hosts are set.
- **J41** [special] Cleaning up earlier invocations' leftovers is fatal to the new invocation (`src/retained.rs:209`, `src/logging.rs:199-201`, `src/temporary.rs:68-75`), while the matching `Drop` paths swallow everything (`temporary.rs:130`, `publication.rs:22`).
- **J43** [special] `src/logging.rs:157,172-183`: a log dir with a lease but a missing, bad or mismatched `result.json`, or a crash-leftover `.tmp*`, is skipped forever and never pruned.
- **J44** [hidden] `src/retained.rs:242-245,296,413-421,500-507`: an empty `g-N-*` dir (crash before `files.tar`) or a leftover `current.json` `.tmp*` makes the identity dir permanently unprunable.
- **J46** [special] `src/retained.rs:194,220,438,450,463`: an empty `archive` string is a second "no generation" sentinel beside `generation == 0`.
- **J48** [hidden] `src/logging.rs:100`: status `"running"` covers both live and crashed invocations, and only the `lease` flock tells them apart, which readers don't check.
- **J53** [hidden] `src/retained.rs:127` hashes the DNS-resolved `NetworkPolicy` endpoints into the identity, so a DNS or CDN change silently discards retained state.
- **J54** [hidden] `src/retained.rs:141-145,257`: the "known recipe" set for checkpoints is every staged input name (data files, dirs), not just recipes.
- **J55** [surplus] `src/retained.rs:263-264,314`: `finish` hashes the export while copying, discards the digest, and then `commit` re-hashes the file.
- **J57** [needless] `src/temporary.rs:20-76` ties the pool sweep to the constructor, so it runs three times per invocation (`src/inputs/mod.rs:134`, `src/launch.rs:171`, `src/inputs/root.rs:66`).
- **A79** [surplus] `src/lib.rs:41-44` `default_state_directory` only forwards to `retained::default_directory` (one caller, `src/bin/recipe/commands.rs:250`).

### 7. X5: One Ginux adapter

Collect everything about talking to Ginux into one `ginux` module:
- Rendering and validating the domain config. Prefer validating with the upstream `ginux-domain` crate over hand-copied limits; see the review question.
- The supervisor result record as typed enums.
- The snapshot-control wire protocol (GXS1/GXA1) as constants with a version.
- The restore-file format and its two uses, made explicit.
- Launcher argv and an fd-inheritance registry that every leased resource registers with.
- `GinuxSetup`, which resolves `GINUX_BIN`, `GINUX_ROOT`, `GINUX_ROOTFS`, `RECIPE_WORKER_BIN`, the image-cache fallback and the worker lookup once, at the edge.
- Network policy as a required input of the config, held once.

- **Solves:** 33 problems (20 hidden, 3 needless, 1 surplus, 9 special)
- **Done when:** No `GXS1`/result-outcome/config literals outside `ginux`. No `std::env::var("GINUX_*")` outside `GinuxSetup::from_env`. The fd list is not hand-maintained. `DomainConfig` cannot be built without its network policy.
- **Depends on:** X2, X8

- **A16** [hidden] `src/launch.rs:176` reads `staging_dir().join("config.toml")` by file name, which `src/inputs/mod.rs:172` writes, although `PreparedInvocation` already carries `config_text`.
- **A18** [hidden] `src/launch.rs:564-583` hand-encodes the Ginux snapshot-control protocol (`GXS1`/`GXA1`, u64 generation, u16 path length, 20-byte reply) with no module, constants or version. Its path bound lives separately at `src/retained.rs:246` (`> 512`).
- **A19** [hidden] `src/launch.rs:1009-1025` hard-codes the supervisor result-file schema (version 1; outcomes `exited`/`signaled`/`supervisor-error`), and the outcomes are compared again as literals at `:1055,1061,1075,1103,1110`. It is a struct of `String`s, not an enum.
- **A47** [needless] `src/launch.rs:200-208`: one `restore` flag/`--restore-file` means either "restore a generation" (`retained`) or "seed a target input" (`inputs::seed_target`), so launch must know both owners' rules.
- **A48** [hidden] `src/launch.rs:222-227` lists by hand each lease fd the supervisor must inherit. A new `TemporaryDirectory` resource left off this list gets deleted by crash recovery (`src/temporary.rs:406-446`) while in use, and `:384-385` assumes Ginux closes those fds.
- **A57** [hidden] `src/launch.rs:1029,1044-1045` parse result `signal`/`generation` as i64 and cast them with `as u64`, borrowing the parser meant for `code`.
- **B11** [hidden] `src/inputs/config.rs:15-21` copies the ginux-domain limits (`tools/ginux/crates/ginux-domain/src/lib.rs:110-121`: `Text<64>`, `Text<128>`, `TextList<7,128>`, `TextList<8,256>`, `TextList<8,64>`) with no link across repos.
- **B12** [hidden] `src/inputs/config.rs:180-244` (`validate_reference`, `validate_domain_path_value`, `quote`) hand-copy the ginux parser's charset and escapes ("mirror", per the comments).
- **B14** [hidden] The env limits (8 entries × 256 B) are enforced three times: `src/inputs/env.rs:115-131`, `src/inputs/config.rs:68-82` and `quote_list` (`config.rs:125-128`).
- **B15** [hidden] `src/inputs/mod.rs:24-29` docs restate every limit and claim a storage/devices 8×64 B limit that is never checked (`config.rs:136,141` are unchecked literals).
- **B17** [special] `src/inputs/config.rs:147` hard-codes `max_connections = 16`, which is not an option.
- **B19** [hidden] Image, worker and launcher selection are env reads buried in four modules: `GINUX_ROOT` (`src/inputs/config.rs:181`, inside `DomainConfig::for_invocation`), `GINUX_ROOTFS` (`src/inputs/root.rs:41`), `RECIPE_WORKER_BIN` (`root.rs:196`) and `GINUX_BIN` (`src/backend.rs:181`). None is in `ExecutionOptions`, yet the root ref and worker digest feed the state identity (`src/retained.rs:127-128`).
- **B20** [hidden] `src/inputs/root.rs:177-193` requires `GINUX_ROOT` = `oci:NAME@sha256:<hex64>` to equal the rootfs tar's hash. Only `README.md:34-41` links it to recipe-image's `provenance.json`.
- **B21** [hidden] `src/inputs/config.rs:28` `DEFAULT_ROOT_REF` embeds the Rocky digest `1026f8d3…`, also pinned in `bootstrap/ginux-{build,acquisition}-image.json:4` and the superproject `docs/ginux-recipe-implementation-plan.md`. It also silently defaults to a base image with no compiler.
- **B22** [special] `src/inputs/root.rs:44-47` falls back to `$XDG_CACHE_HOME/recipe/images/<digest>.tar`, which nothing writes.
- **B23** [special] `src/inputs/root.rs:204-215` uses the current exe as the worker only if it is named exactly `recipe`, otherwise it searches PATH. That ties lookup to the Cargo bin name (`Cargo.toml:16`) and the dispatch at `src/bin/recipe.rs:37-43`.
- **B24** [hidden] `src/inputs/mod.rs:250` reads `HOME` from the process env inside `reject_host_leak`.
- **B30** [hidden] `src/inputs/config.rs:102` builds with `NetworkPolicy::default()`, and callers must remember to chain `.with_network(NetworkPolicy::resolve(..))` (`mod.rs:130-131`, `config.rs:172-173`). Forgetting it silently disables networking while `/etc/hosts` is still installed.
- **B31** [hidden] The network policy is held twice, in `DomainConfig` and in `PreparedInvocation.network` (`src/inputs/mod.rs:129-131,190`). The second copy drives `root.rs:70` and `retained.rs:127`, and nothing checks the two agree.
- **B33** [needless] `src/inputs/config.rs:142-149` reads `NetworkPolicy`'s `pub` fields directly (`hosts` stays private).
- **B36** [hidden] `src/inputs/config.rs:211,215` check `init` against `MAX_ARG` instead of `MAX_PATH`. It works because both are 128.
- **B37** [surplus] `src/inputs/mod.rs:237-300` `reject_host_leak` re-parses the TOML just rendered and repeats the per-field checks, so it changes with every config-shape change.
- **B38** [special] `src/inputs/mod.rs:259` skips leak markers shorter than 2 bytes.
- **B39** [special] `src/inputs/network.rs:389-405` polls `getent` (10 ms sleep, 10 s deadline), writes the 64 KiB bound three times (`:399,411,412`) and hard-codes `/usr/bin/getent` (`:380`).
- **B46** [special] `src/inputs/root.rs:402-470` `seed_target` special-cases the `target` input again.
- **B54** [hidden] `src/inputs/root.rs:428-434` writes the restore tar (`out/` names, ustar, uid/gid 1000), which must match ginux's `--restore-file` contract and `retained::restore_input` (`src/launch.rs:200-208`).
- **B55** [hidden] `src/inputs/root.rs:402-406` `seed_target` relies on `prepare_root` having filled `root/inputs` first, and it takes a raw path rather than `PreparedRoot` (`launch.rs:184,207`).
- **B56** [needless] `src/inputs/root.rs:402-470` puts restore-state logic in `inputs`, while its sibling (`retained::restore_input`) and its only caller (`launch.rs`) live elsewhere.
- **J34** [special] `src/launch.rs:200-209`: once a retained generation exists, a user-selected `target` input is silently ignored (`restore_input` wins).
- **J45** [special] `src/inputs/config.rs:181`: a non-UTF-8 `GINUX_ROOT` silently becomes the default Rocky pin, while `GINUX_ROOTFS`/`RECIPE_WORKER_BIN` reject bad values (`src/inputs/root.rs:43,197`).
- **J58** [hidden] `src/publication.rs:128-129,346` vs `src/inputs/root.rs:299-301`: publication rejects a `./` root entry that root preparation tolerates. The Ginux exporter's archive shape is an unstated cross-repo contract.
- **A67** [special] `src/publication.rs:99` `raw_count >= max_entries * 3` assumes at most two GNU long-name headers per entry, tied implicitly to `src/options.rs:345` and the 256-byte limit at `publication.rs:111`.
- **A68** [hidden] `src/options.rs:342-347` `archive_bound` (`*3072 + 1024`) encodes tar header overhead that `publication.rs` and `src/retained.rs:159,229,264` rely on.

### 8. X4: One Invocation value from CLI to worker

Replace the 8-argument `launcher.run` (×6 in lib.rs), the 9-argument `create_engine` (×6 in the CLI) and the per-op worker dispatch with a single `Invocation { recipe, operation, options, defines, inputs }` and `RecipeEngine::run(Operation)` (thin convenience methods optional). Split `ExecutionOptions` by reason to change: identity-relevant settings, resource bounds, presentation (`machine_events`) and install-only force flags (which move into `Operation::Install`). Derive the clap flags from the options struct (`#[command(flatten)]`/`default_value_t = Default`) so defaults and ranges live once, and drop the fixed `--target-*` knobs. `GinuxLauncher` becomes the coordinator entry and stops being a path wrapper. The public result is JSON (`serde_json::Value`), not a `rhai::Map`, which removes the hand-written JSON↔Rhai round trip.

- **Solves:** 27 problems (16 hidden, 6 needless, 2 surplus, 3 special)
- **Done when:** Adding an option means editing one struct. No JSON→Rhai converter in backend. `lib.rs` and `commands.rs` have no six-way repeated calls. Global flags apply only to the subcommands that use them.
- **Depends on:** X3

- **A24** [hidden] `src/launch.rs:975-984` `json_value_kind` and `src/backend.rs:165-174` `json_kind` are identical copies.
- **A25** [hidden] `src/backend.rs:113-162` hand-converts JSON to `rhai::Map`, while `src/core/executor/private/context.rs:111` uses `rhai::serde::to_dynamic` for the same direction. The two have different number semantics (u64 overflow at `backend.rs:134-139`).
- **A26** [needless] `src/backend.rs:99` turns the ctx JSON into a `rhai::Map`, only for `src/bin/recipe/commands.rs:205-208` to turn it back into JSON. `src/worker/run.rs:696` claims to serialize "exactly like the CLI's JSON output", so three places are tied to one format.
- **A27** [hidden] `src/lib.rs:212-221,272-281,295-304,309-318,323-332,337-346` repeat the same 8-argument `launcher.run(...)` call six times. Any new invocation parameter means six edits plus `backend.rs:29-39` and `launch.rs:124-132,158-167`, which explains the `lib.rs`↔`backend.rs`/`bin/recipe.rs` co-change.
- **A39** [needless] `src/worker/run.rs:339`: `defines` travels positionally while `options.execution` goes through `exec.set_options`, giving two routes for invocation options.
- **A84** [hidden] `src/options.rs:291-309` (`Default`) and `src/bin/recipe/cli.rs:30-66` (clap `default_value`s) each hold a copy of every default, and `src/bin/recipe/commands.rs:13-27` maps each field by hand. A new option means three edits.
- **A86** [hidden] `src/options.rs:337` (≤ 8 hosts) duplicates `src/inputs/config.rs:146` (`quote_list(…, 8, …)`), and `src/options.rs:326` duplicates `src/launch.rs:608` (`1..=86400`).
- **A87** [hidden] `src/lib.rs:79-87` `AutoFixConfig` default `attempts: 2` duplicates `src/bin/recipe/cli.rs:109`, and the range `1..=8` (`lib.rs:206`) and CLI `.max(1)` (`commands.rs:54`) are separate partial validations.
- **B16** [hidden] The cap of 8 hosts/endpoints appears in `src/options.rs:76`, `src/inputs/network.rs:351` and `src/inputs/config.rs:148`. The 64-byte endpoint length exists only at `config.rs:148`.
- **B28** [hidden] The CLI declares `--force-*` as global flags on every subcommand (`src/bin/recipe/cli.rs:38-46`), but they apply only to install (`src/backend.rs:45-47`).
- **B34** [hidden] `src/inputs/config.rs:88` sets `vcpus` from `jobs`, which `cli.rs:35` and `src/core/invocation.rs:66,86` treat as a recipe-facing value. One flag, two meanings.
- **B57** [hidden] The native linux/x86_64 check appears at `src/inputs/root.rs:37`, the ELF check at `root.rs:131`, `src/options.rs:51-60` and `src/core/invocation.rs:140`.
- **B64** [hidden] `src/bin/recipe/cli.rs:30,33` default the target to the literals "linux"/"x86_64", while `src/options.rs:33-34` uses `std::env::consts`.
- **B65** [hidden] Adding an option touches `cli.rs`, `commands.rs:13-27` (hand mapping), `options.rs` (field, Default, validate), the identity decision at `retained.rs:123-138` and the docs.
- **B66** [special] `src/bin/recipe/commands.rs:54` `.max(1)` silently turns `--autofix-attempts 0` into 1.
- **B67** [hidden] `src/bin/recipe/commands.rs:62-162` repeats a nine-arg `create_engine` call six times, the CLI-side twin of the `lib.rs` sextuple.
- **B69** [needless] `src/bin/recipe/commands.rs:70,87,104,126,143,160` pass the global `--llm-profile` to every operation, but it matters only for `install --autofix`.
- **B70** [hidden] The global flags (`src/bin/recipe/cli.rs:13-92`) are silently ignored by `list`, `info` and `hash` (`commands.rs:167-199`).
- **B71** [special] `src/bin/recipe/commands.rs:31-38` creates the recipes dir on every command, which makes `:321-324` unreachable.
- **B79** [hidden] `src/bin/recipe/metadata.rs:45-58` reads `RECIPE_PATH` by hand (clap `env` is enabled but unused), and the shipped OS sets it (`leviso/src/component/custom/packages/files/recipe.sh:1`).
- **H13** [needless] `src/backend.rs:29-107`: `GinuxLauncher::run` holds the whole coordinator (validation, input prep, domain run, error mapping, ctx conversion, stderr summary) while its type holds only a `PathBuf` (`:7-9`), so any orchestration change edits the "launcher".
- **H14** [needless] `src/backend.rs:69-70` and `src/launch.rs:131,141,164,192` thread the raw host recipe path through two layers solely for `Retained::open`'s scope hash (`src/retained.rs:98`).
- **H16** [needless] `src/options.rs:7-29`: `ExecutionOptions` mixes logging (`machine_events`), main-recipe-only force flags, domain resource bounds and network policy. Identity picks fields one by one (`src/retained.rs:132-137`), and `deny_unknown_fields` turns every new knob into a protocol schema change.
- **H31** [special] `src/lib.rs:210-264`: the retry loop uses `unreachable!` and `.expect("attempts require explicit autofix")`, relying on `attempts > 0 ⇒ autofix.is_some()`.
- **J47** [hidden] `src/options.rs:81-90` vs `src/inputs/config.rs:138-140`: `storage_max_bytes: None` means "memory_mib bytes" for the recipe's own bounds but "omit `max_bytes`" (the launcher default) for Ginux.
- **A83** [surplus] `src/options.rs:269-270,312-322`: `target_os`/`target_arch` are CLI knobs (`src/bin/recipe/cli.rs:29-34`) that accept only native linux/x86_64. `src/core/invocation.rs:67-70` uses `std::env::consts` anyway.
- **B68** [surplus] `src/bin/recipe/commands.rs:228,264`: `recipes_path: Option` is always `Some`, so the `None` branch is dead.

### 9. X6: Explicit coordinator run pipeline

Restructure `launch::run_domain` as an ordered pipeline with typed step results:
- The steps run lock → open state → prepare root → restore/seed → spawn → pump → reap → verify → publish → commit → record.
- A per-`Operation` policy decides whether a run commits state and publishes. Checks don't fabricate state.
- One `Deadlines` value (frame, lock wait, shutdown grace, drain grace) comes from options, not re-read env.
- A cancellation token gives a typed `Cancelled` outcome and is watched by the waits, instead of a global atomic read at labelling time.
- Teardown collects secondary errors next to the primary error instead of swallowing them or masking it.
- The stderr drain is independent of cancellation.
- The output location comes from the invocation, not `current_dir()`.

- **Solves:** 28 problems (11 hidden, 1 needless, 0 surplus, 16 special)
- **Done when:** No `let _ =`/`.ok()` in teardown without the error being recorded. The publish/commit order and the check-operation policy are explicit in one place. No `RECIPE_FRAME_TIMEOUT_SECS` env read inside launch. `signal()` is not read to relabel errors. `library_backend` no longer writes into the crate root.
- **Depends on:** X3, X5, X7

- **A46** [special] `src/launch.rs:169-170`: the destination-lock wait reuses `frame_timeout()`, a transport knob, which also re-reads the env var (`:169`, `:399`).
- **A50** [special] `src/launch.rs:305-308` swallows reap and sync errors on the error path (`.ok().flatten()`, `let _ =`).
- **A51** [special] `src/launch.rs:473-474,488` swallow stderr write errors with `let _ =`.
- **A53** [special] `src/launch.rs:835-836` (25 ms sleep loop), `src/retained.rs:51` (25 ms sleep loop) and `src/launch.rs:660` (100 ms poll cap) poll so the global cancellation atomic gets noticed.
- **A54** [special] `src/launch.rs:879,896-903`: the stderr drain uses magic 250 ms read timeouts and a 1 s grace, unrelated to `SHUTDOWN_GRACE` (`:76`).
- **A55** [special] `src/launch.rs:151,741-746` label an error "cancelled" by reading the global signal atomic at record time, so any error after a signal gets relabelled.
- **A56** [special] `src/launch.rs:337-341` requires `ctx_summary.state` as an object even for remove/cleanup/checks, because `src/retained.rs:306-318` always writes the main state.
- **A66** [special] `src/publication.rs:74-77` publishes to `current_dir()/.artifacts/ginux-export/<invocation>`, so the output location depends on the process cwd, not on `destination`/`state_dir`. Nothing prunes it, and tests assert the path (`tests/e2e/managed_inputs.rs:1515,1620`).
- **A89** [hidden] `src/cancellation.rs:6`: the global `SIGNAL` atomic is read implicitly by `launch`, `retained`, `inputs::network`, `llm::runner` and `lib.rs:211`, and it changes error text (`launch.rs:151,741`).
- **A90** [hidden] `src/bin/recipe.rs:56-57` maps `signal()` to exit `128+signal`, relying on `launch.rs:818-825` having forwarded it.
- **F32** [needless] `tests/e2e/managed_inputs.rs:1323` and `managed_cancellation.rs:44` rely on `RECIPE_FRAME_TIMEOUT_SECS` (`src/launch.rs:601-612`), a production env hook that exists for tests.
- **F36** [hidden] `tests/library_backend.rs:14-15` runs with the crate root as cwd, so `src/publication.rs:73` creates a root-owned `tools/recipe/.artifacts/ginux-export` in the checkout (it exists now, untracked, and makes `find`/`bfs` print "Permission denied").
- **H4** [hidden] `src/launch.rs:170` must call `lock_destination` (which creates the dir, `src/retained.rs:25`) before `Retained::open`, which canonicalizes and fails on a missing dir (`src/retained.rs:97`). The ordering is invisible.
- **H5** [hidden] `src/launch.rs:333-353` commits the new generation (`state.finish`) before `publication::publish`. A publication failure returns an error, but the next run resumes from a state whose outputs were never delivered.
- **H6** [hidden] `src/launch.rs:879,640,929`: the stderr drain's `DeadlinePipe` calls `cancellation::check_io()`, so on Ctrl-C the drain stops just as teardown begins. A launcher writing stderr during teardown can block on a full pipe until SIGKILL, and the teardown log tail is lost.
- **H7** [hidden] `src/launch.rs:333-353` runs `finish()` and `publish()` for every operation, including `is_*` checks. A manual check therefore creates a retained generation (copying the export) and publishes outputs. distro-builder depends on that (`distro-builder/tests/recipe_outputs.rs:180-189`), but nothing on the check path says checks write state.
- **H23** [special] `src/launch.rs:154`: a failure in `diagnostics.record(&record)?` replaces the real outcome, masking the original error or turning a persisted and published success into an error.
- **H24** [special] `src/launch.rs:890,913-915,948-952,957`: drain log create, write, tail and join failures are swallowed (`.ok()`, `file = None`, `unwrap_or_default`).
- **H25** [special] `src/launch.rs:435-443,455`: the deadline rule switches on `worker_started`, which any frame (even a diagnostic) flips.
- **H26** [special] `src/launch.rs:1094-1127` checks the launcher exit code only when the worker succeeded, so a failed workload plus a nonzero launcher exit plus an `exited/0` record passes unchecked.
- **J31** [hidden] `src/publication.rs:36-44` vs `src/launch.rs:343-346`: the export is validated only in `publish`, and only when `ctx.outputs` is non-empty, after `finish()` has already stored the unvalidated export as the next restore archive.
- **J32** [special] `src/publication.rs:308-333`: after the irreversible `renameat2`, the base fsync and the UTF-8 conversion can still return `Err`, so outputs are published while the caller sees a failure.
- **J35** [hidden] `src/retained.rs:369-390` `copy_hash` never checks cancellation, while `src/inputs/root.rs:235` checks every chunk. GiB-sized restore and commit copies cannot be cancelled.
- **J38** [hidden] `src/cancellation.rs:1,9-27`: only `install_cli_handlers` can set `SIGNAL`, so for library callers every cancel check and poll is a permanent no-op.
- **J39** [special] `src/inputs/network.rs:20-68` resolves every grant with `getent` for every operation (`src/inputs/mod.rs:129`), so a DNS outage fails a read-only `is_*` check.
- **J49** [hidden] `src/launch.rs:343,365`, `src/retained.rs:306`: the committed generation from `finish()` is discarded, and `LaunchReport` mixes retained generations with Ginux's `result_generation` under one name.
- **J61** [special] `src/cancellation.rs:12-21` latches the first signal with `sa_flags = 0`: no second-signal escalation and no `SA_RESTART`.
- **J42** [special] `src/publication.rs:13-26`: after a successful rename, `Staging::drop` walks a missing path, and `TempDir` drop fails, so success relies on swallowed errors. Failed runs leave `.pending-*` dirs that nothing prunes.

### 10. X9: One recipe and input resolver, one static analyzer

A single `recipes` module resolves `//! extends:`, imports and `deps`/`build_deps` (naming, validation, recursion and depth policy). It produces a manifest of recipe-name → domain-path that discovery stages and that the worker's module resolver and dependency lookup read directly, so the worker never re-derives paths. The CLI name lookup, `--input` name rules, autofix's base lookup and metadata listing use the same module. All static AST walking (static references, `declared_metadata`, the API marker) and all Rhai-internals use live in one place next to the `rhai` pin rationale. Input kinds (`rootfs`, `target`) are declared properties, not name checks.

- **Solves:** 30 problems (16 hidden, 3 needless, 1 surplus, 10 special)
- **Done when:** One `parse_extends`, one resolver, one AST walker. Anything discovery stages is exactly what the worker resolves. The `rhai =1.19.0` comment lists every internals dependency.
- **Depends on:** X2

- **B6** [hidden] `src/inputs/mod.rs:141` names staged files `input-{index}` by position in `discovered.inputs`, and `src/inputs/root.rs:91` rebuilds the names from positions in the `pub` `invocation.inputs` (`mod.rs:79`). Reordering pairs the wrong bytes, and the error only says "changed after discovery".
- **B8** [hidden] Import naming at `src/inputs/discover.rs:383-401` mirrors `src/worker/run.rs:462-476` `PreparedModules::path`. It works only because `main.rhai` sits at the `/inputs` root.
- **B9** [hidden] `src/inputs/discover.rs:379-381,411-412` follows `extends:` recursively, while `src/core/executor.rs:110-117` rejects a base that itself extends, so discovery stages chains the executor refuses.
- **B10** [needless] `src/inputs/discover.rs:636-685` `static_references` compiles with its own bare `rhai::Engine` (no limits, no helpers), separate from `core::invocation::engine()`.
- **B45** [special] `src/inputs/discover.rs:122` gives the input names `rootfs` and `target` special tree handling. They exist for specific superproject callers (`distro-builder/src/recipe/linux.rs:63`, `distro-builder/src/pipeline/kernel.rs:55`, `distro-builder/tests/recipe_outputs.rs:245`).
- **B47** [hidden] The `.git | target` exclusion is repeated at `src/inputs/discover.rs:131,220` and restated in `cli.rs:73` and the `lib.rs` `add_input` docs. The help leaves out the rootfs/target exception.
- **B48** [special] `src/inputs/discover.rs:200` (40 symlink hops) and `:326` (100_000 files) are unnamed limits next to the named constants at `:28-35`.
- **B52** [special] `src/inputs/discover.rs:165-167` swallows a canonicalize failure, so that path never becomes a leak marker.
- **B53** [special] `src/inputs/discover.rs:688-693` `same_file` falls back to comparing raw paths.
- **B74** [hidden] `--input` parsing (`src/bin/recipe/commands.rs:276-284`) is re-implemented in `examples/prepare_inputs_demo.rs:15-21`, and the real name rules live in `src/inputs/discover.rs:525-547`. `--define` parsing (`commands.rs:268-274`) is separate from the protocol limits (`src/worker/protocol.rs:168-180`).
- **B75** [hidden] `src/bin/recipe/commands.rs:289-316` `resolve_recipe_path` is a third recipe-name resolver (next to `src/core/build_deps.rs:95-122` and discovery).
- **B78** [special] `src/bin/recipe/metadata.rs:36-39` swallows `read_dir`/entry errors, while `commands.rs:328` aborts `list` on one bad recipe.
- **C3** [needless] `src/inputs/discover.rs:21` (host-side preparation) imports `core::executor::parse_extends` (`src/core/executor.rs:24`), so the coordinator depends on the worker engine module for a comment parser.
- **C4** [needless] `src/core/executor.rs:155-231`: `DeclaredMetadata`/`declared_metadata` is a static parser used only by `src/bin/recipe/metadata.rs:13`, and it shares a file with compilation. CLI listing and the lifecycle executor change for different reasons.
- **C5** [hidden] `src/core/executor.rs:24-38` `parse_extends` and `:43-70` `resolve_base_path` are copied at `src/core/autofix/prompt.rs:92-106,108`. A third resolver at `src/inputs/discover.rs:420-460` diverges (it uses `is_file` and rejects `/` and `..`, while the executor uses `exists()` and checks nothing).
- **C6** [hidden] `src/core/executor/private/flow.rs:16-32` reads `deps`/`build_deps` scope variables at runtime, while `src/inputs/discover.rs:654-679` parses the same two names statically to stage dependency recipes.
- **C7** [hidden] `src/core/build_deps.rs:106-118` builds `format!("{name}.rhai")` and searches parent, then `recipes_path`. `src/inputs/discover.rs:662` builds the same name independently, and the validation differs (`build_deps.rs:97-104` allows only `Component::Normal`, `discover.rs:437-447` rejects only `..` and leading `/`).
- **C15** [hidden] `src/core/executor.rs:184,213` list `["name","version","description"]` twice in one parser.
- **C58** [special] `src/core/executor.rs:173-175,195-197`: the metadata parser handles `ctx` redeclaration and op-assignments pattern by pattern. It is a third static AST walker, after `src/inputs/discover.rs:636-685` and `src/core/invocation.rs:387-406`.
- **C59** [special] `src/core/executor/private/context.rs:89-91` caps dependency depth at 64, which matches `set_max_call_levels(64)` (`src/core/invocation.rs:13`) by coincidence. `src/inputs/discover.rs` `MAX_IMPORT_DEPTH` is another limit on the same graph.
- **E76** [special] `src/core/autofix/prompt.rs:108-126` returns `None` for a missing base, so autofix builds a prompt without it, while the executor (`src/core/executor.rs:62-67`) fails.
- **G43** [hidden] `Cargo.toml:20-21` pins `rhai =1.19.0` citing only `extends`' `AST +=` (which still exists, `src/core/executor.rs:24-37,131`). It leaves out the heavier version-sensitive internals use: `src/core/invocation.rs:4-7,110-112,168-202` (`GlobalRuntimeState`, `Caches`, `Stmt::Var` layout, deprecated `eval_expression_tree_raw`), `src/core/executor.rs:162-200` and `src/inputs/discover.rs:646-661`.
- **H22** [special] `src/bin/recipe/metadata.rs:15-21,23-28`: a `"<dynamic>"` sentinel string and a file-stem fallback for `name`.
- **I1** [hidden] `src/inputs/discover.rs:656-663` stages `deps = ["./x"]` (via `resolve_related`), but `src/core/build_deps.rs:95-123` rejects any non-`Normal` component, so a dependency that prepares fine then fails in the worker.
- **I2** [hidden] `src/inputs/discover.rs:391-393` recomputes `beside_referrer` with the same `parent.join(target).is_file()` probe that `resolve_related` used at `:447-451`, to guess which branch the resolver took.
- **I4** [hidden] `src/core/executor.rs:171-207` `declared_metadata` statically mimics `Runtime::new`'s "newest ctx wins" rule (`src/core/invocation.rs:206-212`). It misses `ctx["name"] = …` and writes inside functions, so it can report a stale literal.
- **I15** [hidden] `src/inputs/discover.rs:112` renames the main recipe to `main.rhai`, so the file-stem name fallback (`flow.rs:79-85`) is always `"main"` in the domain, and it collides with a dependency that is really named `main`.
- **I16** [hidden] `src/inputs/discover.rs:35` `MAX_IMPORT_DEPTH = 16` also bounds dependency nesting (`:373-376`), so the executor's 64 (`context.rs:89`, `src/core/invocation.rs:13`) is unreachable on the managed path.
- **I27** [special] `src/inputs/discover.rs:113-115` inserts the main recipe into `visited` before `collect_related`, which does the same for every other file (`:411`).
- **I21** [surplus] `src/core/executor.rs:78-79`: `CompiledRecipe.base_dir` is just `base_path.parent()`, stored separately.

### 11. X10: Lifecycle as data with explicit recipe roles

Describe phases with one table (`Phase` → check name, optionality, missing-hook rule, force implication, success/failure reasons, env scope). One phase-block function replaces the three copies, and one recipe-preparation function replaces the four preambles. `Readiness` is kept as an enum end to end, forced runs carry a reason, and the double `is_installed` verify is unified. An explicit `RecipeRole { Main, Dependency }` decides force, failure eligibility, work dirs (dependency dirs outside the main build dir), the output policy (dependencies may not write `/out`) and env guards. It replaces stack depth. Dependencies get a visited set, and a defined policy covers dependencies for remove/cleanup. Dependency tool env vars come from the dependency recipe's declared exports, not hard-coded bison/m4. `ExecutionContext` is built complete, with no call-order setup. The checkpoint payload holds only what the host reads. `has_fn` moves to break the invocation↔runner cycle.

- **Solves:** 62 problems (27 hidden, 6 needless, 11 surplus, 18 special)
- **Done when:** One phase block and one preamble. No `active_recipes.len() > 1`. No BISON/M4 literals in the engine. The checkpoint sink takes `(recipe_path, state)`. No module cycle in `deps.txt`.
- **Depends on:** X3, X9

- **A38** [needless] `src/worker/run.rs:343-347`, `src/core/executor/private/context.rs:14-15`: `CheckpointSink` has a `persist: bool` that its only implementation ignores, while `persist_state` is also passed separately to `restore_states`, `install_with_context`, etc.
- **C1** [needless] `src/core/invocation.rs:328` calls `runner::has_fn`, while `src/core/runner.rs:3,48-53` imports `invocation::{Runtime,Readiness}`. The `invocation`↔`runner` cycle exists only to share a one-line AST query.
- **C12** [hidden] `src/core/runner.rs:29-34` maps phase→check. `src/core/executor/private/flow.rs:102,117,125,262,411` spells the same pairings out again, and `context.rs:142-145` hard-codes `install`/`check:is_installed`.
- **C13** [hidden] The force implication acquire⇒build⇒install is written at `src/options.rs:91-93` and `src/core/executor/private/flow.rs:104,120,128,273`.
- **C14** [hidden] The display-name fallback from the file stem appears at `src/core/executor/private/flow.rs:76-85` and `actions.rs:49-52,122-125,189-192`, with different fallbacks (`"package"`) that have already diverged.
- **C17** [hidden] `src/core/invocation.rs:205-215,237-241` find `ctx` via `iter_raw()` with a write lock (to handle shadowing and shared captures). `flow.rs:71` and `actions.rs:44,117,184` use `scope().get_value("ctx")`, so the subtle rule lives in only some of the copies.
- **C19** [hidden] `src/core/executor/private/flow.rs:198-256,293-349,352-408` are three near-identical acquire/build/install blocks (run_phase→report→checkpoint→cleanup success/failure), and `actions.rs:63-75` (remove) is a fourth variant.
- **C20** [hidden] `src/core/executor/private/actions.rs:19-47,91-120,164-187` and `flow.rs:43-73` repeat the same preamble (canonicalize→compile→scope→Runtime→restore→ctx→name) four times, and the copies already differ.
- **C21** [hidden] The recipe path is canonicalized at `src/core/executor/private/attempt.rs:26-28` and `actions.rs:19-21,91-93,164-166`, then again in `src/core/executor.rs:94-96`. State lookup (`context.rs:125`) and the host's `valid_recipe_identity` (`src/worker/protocol.rs:478`) silently depend on it producing `/inputs/...`.
- **C22** [hidden] `src/core/executor/private/flow.rs:67` (and `actions.rs:42,115,182`) restores `ctx.state` after `Runtime::new` (`src/core/invocation.rs:120-234`) has already run top-level setup with an empty state. Only hooks see restored state, which recipe authors are never told.
- **C23** [hidden] `src/core/runner.rs:35-37` re-verifies `is_installed` inside `run_phase("install")`, and `src/core/executor/private/flow.rs:411-443` verifies it again with different rules (`has_fn` vs `has_fn_arity(…,1)`; phase `install` vs `is_installed`). A 2-arg `is_installed` passes the first filter and is then called with one arg.
- **C24** [hidden] `src/core/executor/private/context.rs:40-45` picks the main or dependency policy from `active_recipes.len() > 1`, which requires `enter_recipe` before `options()`. `actions.rs:32,105,176` call it with an empty stack and get the main policy only by luck.
- **C25** [hidden] `src/core/executor/private/context.rs:26-38,104-109`: `ExecutionContext::worker` starts with defaults and `resumable=false`. It becomes correct only after `set_options` and `restore_states(…, enabled)`, which sets `resumable` as a side effect (`src/worker/run.rs:366-369`).
- **C26** [hidden] `src/core/executor/private/context.rs:21,160-167`: `restored_states` doubles as the restored input and a cache of the latest checkpointed state (updated only when `resumable`). A recipe entered twice sees different state depending on that flag.
- **C27** [hidden] `src/core/executor/private/context.rs:50-54,97-101`, `src/core/build_deps.rs:42,45`: `work_root` is set lazily by the first recipe entered and cleared when the stack empties. Dependency build dirs (`build_deps.rs:57-60`) depend on that order.
- **C28** [hidden] `src/core/invocation.rs:18-100` (`scope()`) builds most of `RUNTIME`, and `:158-162` (`Runtime::new`) adds `operation`/`options.persist_state` afterwards. `:143` `runtime["paths"].clone().cast::<Map>()` panics if the shape changes.
- **C29** [hidden] `src/core/executor/private/actions.rs:181` uses `exec.resumable()` as `persist_state` for manual checks, while the other operations pass an explicit `persist_state_enabled` (`flow.rs:63`, `actions.rs:40,113`). One flag is stored in two places.
- **C30** [hidden] `src/core/executor/private/attempt.rs:17-25` takes `(…, recipe, defines, persist, search_path, …)`, while `actions.rs:10-18,81-90,155-163` take `(…, recipe, search_path, defines, …)`. The sibling signatures order same-typed args differently.
- **C33** [hidden] `src/core/executor/private/actions.rs:224` ("return ctx on success path") and `flow.rs:96` ("throw means needs this step") describe the pre-API-2 check contract. `src/core/invocation.rs:327-358` now requires `ready`/`needs_work`.
- **C40** [needless] `src/core/executor/private/context.rs:14-15,152-159`: the checkpoint sink receives the whole `&CompiledRecipe` (`&mut` in `persist_checkpoint`), but the only consumer (`src/worker/run.rs:549-553`) reads `recipe_path`.
- **C41** [needless] `src/core/executor/private/flow.rs:71,157-159` reach through `Runtime::scope()` (`src/core/invocation.rs:252`) to the raw Rhai scope, although `Runtime` exists to confine low-level API use (`:109-111`).
- **C42** [needless] `src/core/runner.rs:19-40`: `run_phase` takes a general `fn_name` but hard-codes the check mapping for three names, and `"remove"` (`actions.rs:63`) silently gets no verification.
- **C43** [special] `src/core/executor/private/state.rs:46-57` hard-codes `BISON_PKGDATADIR`, `M4`, `LIBRARY_PATH` (lib64), `C_INCLUDE_PATH`, `CPLUS_INCLUDE_PATH` and `PKG_CONFIG_PATH` for the kernel's `linux-deps` toolchain. The superproject's `distro-builder/recipes/linux.rhai:30-55` derives the same values itself.
- **C44** [special] `src/core/executor/private/flow.rs:138,258`: only `build` is optional. A missing `acquire` fails, and a missing check means "needs work" (`src/core/invocation.rs:328-332`), so the absent-hook rule differs for each phase.
- **C45** [special] `src/core/invocation.rs:328-332` treats a missing `is_*` as `NeedsWork` during install, while manual checks (`src/core/executor/private/actions.rs:194-208`) treat it as an error.
- **C46** [special] `src/core/executor/private/flow.rs:104-110` + `reporting.rs:42-58`: a forced phase gets `reason: None`, which reaches a `None` branch that reports "check returned failure" (wrong). Forcing was never modelled in reporting.
- **C47** [special] `src/core/executor/private/flow.rs:258-273` re-checks `is_built` after acquire (with its own `friendly_reason`, `reporting.rs:4-17`), so the plan printed at `flow.rs:133-143` can announce a build that never runs.
- **C48** [special] `src/core/executor/private/flow.rs:411-443`: post-install verification reuses `auto.install.failure` with phase `is_installed` so that autofix (`lib.rs:232`) covers it.
- **C49** [special] `src/core/executor/private/actions.rs:141-149` always records `reason: "manual"`, ignoring the actual cleanup reason (`:87`). `src/worker/protocol.rs:200` documents `cleanup:manual`, which matches neither.
- **C50** [special] `src/core/executor/private/context.rs:133-150`: `checkpoint_reused` checkpoints only when `resumable`, while `persist_checkpoint` always fires. It hard-codes `persist_enabled: true` and runs for the main recipe too (`flow.rs:146`), though its doc says "reused dependency".
- **C51** [special] `src/core/build_deps.rs:84-89` flattens a dependency's `Phase` error into a plain error, which `flow.rs:188,287` wraps as `Fatal`, so dependency failures are never autofix-eligible. This is an accident of the error plumbing.
- **C52** [special] `src/core/invocation.rs:194-198,298-301` treat `Return`/`Exit` as success, so `exit(x)` in a hook makes `x` the new ctx.
- **C54** [special] `src/core/invocation.rs:144-150` bails on an empty build path only because `scope()` takes `Option<&Path>` (`:22`), and every caller passes `Some`.
- **C55** [special] `src/core/invocation.rs:134-137,152-157` reject a `RUNTIME` define or redeclaration with dedicated checks, and the literal `"RUNTIME"` appears at `:135,154,175`.
- **C56** [special] `src/core/invocation.rs:200-202,216-221,363-378`: the API2 namespace rules (no const `ctx`, forced `state`/`outputs`) are spread across setup and `validate_namespaces`, which re-serializes the whole runtime on every hook return.
- **C57** [special] `src/core/executor.rs:94-96,105-107`, `attempt.rs:26-28` and `actions.rs:19-21,91-93,164-166` swallow `canonicalize()` errors with `unwrap_or_else(|_| path)`, producing identities that won't match.
- **C60** [surplus] `src/core/executor/private/context.rs:14-15,152-159`, `flow.rs:39`, `actions.rs:16,88`: `persist_enabled` is threaded everywhere, and the only sink ignores it (`src/worker/run.rs:346-352`).
- **C61** [surplus] `src/core/executor/private/context.rs:7-11`: `CheckpointPoint.{phase,reason}` are built at about 12 sites and never read by the host (`src/launch.rs:491-527,586`, `src/worker/protocol.rs:487-494`).
- **C64** [surplus] `src/core/executor/private/state.rs:11-20`: `check_phase_with_reason` flattens `Readiness` into a `(bool, Map, Option<String>)` tuple, losing the guarantee that causes the bogus `None` branch above.
- **C65** [surplus] `src/core/executor/private/attempt.rs:17-43`: `install_with_context` only brackets `install_once`. The name is a leftover of in-engine retry (now `src/lib.rs:205-266`).
- **C66** [surplus] `src/core/executor.rs:13-18`, `src/core/executor/private/mod.rs:1-10`: the `private` layer only re-exports, and its submodule names (`state`, `attempt`) don't match their contents.
- **C67** [surplus] `src/core/build_deps.rs:14-35`: `BuildDepsResolver` is a `pub` struct that only stores the args for one call (`state.rs:34-35`).
- **E77** [hidden] The lifecycle check names are hand-written in `src/core/autofix/parser.rs:159-163`, `src/core/autofix/patch.rs:139-145`, `src/core/autofix/prompt.rs:13`, `src/core/runner.rs:30-32` and `src/lib.rs:310-338`.
- **I6** [hidden] `src/core/executor/private/context.rs:76-95` detects only cycles in the active stack, with no visited-set dedupe. A dependency listed in both `deps` and `build_deps` (`src/core/executor/private/flow.rs:177-192,276-291`), or reached twice through a diamond, is recompiled, re-checked and re-checkpointed each time.
- **I7** [hidden] `src/core/build_deps.rs:57-60` puts dependency work dirs at `work_root()/.deps` inside the main recipe's `/work/build` (`context.rs:50-54`), so a main build or cleanup hook that clears `RUNTIME.paths.build` deletes the dependencies' work.
- **I8** [hidden] Dependencies share `RUNTIME.paths.output` (`/out`) and `temp` with the main recipe (`src/core/invocation.rs:60-63`). Only the force flags differ (`src/options.rs:94-101`). "Dependencies install into `paths.tools`, never `/out`" is unwritten and unenforced, and a dependency writing to `/out` pollutes the product that `publication` validates.
- **I9** [hidden] `EnvironmentGuard` (`src/core/executor/private/state.rs:58`, `src/helpers/internal/cmd.rs:16-33`) restores a whole snapshot on drop, so guards must drop in strict LIFO order. `flow.rs:177` `_env_guard` and `:276` `_build_env_guard` rely on Rust drop order and block scoping.
- **I10** [hidden] The same check runs in different environments depending on where it is called: initial checks (`flow.rs:101-129`) run before `deps` are on PATH, the post-acquire `is_built` (`:261`) runs with `deps` only, the verify inside `run_phase("build")` runs with `build_deps`, and the final `is_installed` (`:411`) runs without `build_deps`.
- **I11** [hidden] Only install resolves `deps`. remove (`src/core/executor/private/actions.rs:10-79`) and cleanup (`:81-153`) never get dependency tools or the `BISON_PKGDATADIR`/`M4` env, even for recipes that declare `deps`.
- **I12** [hidden] `src/core/executor/private/flow.rs:145-152` returns before `dependency_names` (`:157-159`) when already installed, so a malformed `deps` is reported only when work is needed.
- **I13** [hidden] `src/core/executor/private/flow.rs:216,221-222,311,315-316,370,374-375` map a success-cleanup or checkpoint/ack failure to `Fatal`, but after a phase failure `checkpoint_failure_cleanup` (`:462-474`) downgrades the same failures to warnings. The worker then still reports a non-fatal Phase result although the ack channel may be broken (`src/worker/run.rs:589-603` doesn't set `channel.failed` on ack timeout).
- **I17** [needless] `src/core/executor/private/context.rs:152`, `flow.rs:146,207`: `CompiledRecipe` is passed `&mut` to `persist_checkpoint`/`checkpoint_reused` but never mutated (a leftover of source rewriting). This forces `compiled.ast.clone()` at `flow.rs:45` and `actions.rs:24,96,169`.
- **I18** [surplus] `src/core/executor/private/state.rs:33-42` prepends the tool dirs again at every dependency nesting level (`environment("PATH")` reads the parent's pushed value), on top of the domain PATH that already has them.
- **I22** [surplus] `src/core/executor/private/context.rs:26`: `ExecutionContext::worker` is the only constructor, so the name implies modes that no longer exist.
- **I23** [special] `src/core/invocation.rs:135-137` rejects only `RUNTIME` as a define. Other engine-owned names (`ctx`, `RECIPE_API`, `deps`, `build_deps`) can be injected as constants.
- **I24** [special] `src/core/invocation.rs:275`: `global.num_operations = 0` before every call makes the operation limit per call, not per invocation.
- **I25** [special] `src/core/executor/private/flow.rs:261-270`: the post-acquire `is_built` recheck reports only through `friendly_reason` (no `hook_event`/`report_check_result`), and prints "check function is missing" noise when `is_built` is absent.
- **I26** [special] `src/core/executor/private/flow.rs:416-443`: post-install verification failure runs failure cleanup on the post-install ctx (`:429`), whereas every other failure uses `ctx_before`.
- **C69** [surplus] `src/core/executor/private/flow.rs:352`: `if needs_install {` is always true (it already returned at `:145-152`).
- **I19** [surplus] `src/core/executor/private/flow.rs:71-73` and `actions.rs:44-47,117-120,184-187`: the "Recipe missing ctx" branches are unreachable (`src/core/invocation.rs:212,215` already bail).
- **I20** [surplus] `src/core/runner.rs:78` `run_cleanup` re-checks `cleanup_available`, which `flow.rs:46` and `actions.rs:97` already computed.
- **I14** [hidden] `src/core/build_deps.rs:63,78` name a dependency's hook events by the `deps` string, while its own events use `ctx.name` (`flow.rs:76-88`).

### 12. X13: Typed events and narration; split logging

Hook events become an enum (`Hook` × `Status`) emitted by the lifecycle and rendered by narration with an exhaustive `match`. Machine events get their own channel (a protocol frame kind or a dedicated fd) instead of being mixed into human lines. The machine-events flag lives only in options. `logging.rs` is reduced to the sink plumbing from X12; the log store moves to X7. In-process event-sink setters are removed from the public API, since events happen in the worker.

- **Solves:** 8 problems (5 hidden, 1 needless, 2 surplus, 0 special)
- **Done when:** No hook/status string literals outside the event enum's serialization. Narration has no unreachable arms. `logging.rs` is gone or holds only the sink.
- **Depends on:** X12, X10

- **A77** [needless] `src/logging.rs:1-118` mixes hook-event re-exports, the worker sink global and the durable diagnostics directory, three concerns that change for different reasons.
- **C18** [hidden] `src/core/events.rs:37-46,50-59` copy `RecipeHookEvent`'s five fields by hand into `json!` fallbacks.
- **C35** [hidden] `src/core/output/logging.rs:10,17-19,74`: the global `MACHINE_EVENTS_ENABLED` is set by both the CLI (`src/bin/recipe/commands.rs:29`) and the worker (`src/worker/run.rs:256`), and it also travels in `ExecutionOptions.machine_events` (`src/options.rs:12`). The host-side set does nothing.
- **C37** [hidden] `src/core/output/narration.rs:69-132` matches hook/status strings emitted from `flow.rs`, `actions.rs`, `reporting.rs` and `build_deps.rs`, and they are deliberately untyped (`src/core/events.rs:4-5`). `actions.rs:215`'s `"manual"` status falls to the generic sentence.
- **C76** [surplus] `src/core/events.rs:16,27`: `RecipeHookEvent.event` is always the constant `RECIPE_HOOK_EVENT`.
- **C78** [surplus] `src/core/output/narration.rs:75-99`: the `"running"` arms for `dependency.{prepare,acquire,build,install}` and `check.is_*` are unreachable, given what `src/core/build_deps.rs:63,79` emits.
- **H9** [hidden] `src/lib.rs:53-56` re-exports event-sink setters that act on in-process statics (`src/core/output/logging.rs:10-38`), but hook events are emitted in the worker process, so a library caller's sink never receives them.
- **J14** [hidden] `src/core/output/logging.rs:91,94` → `src/worker/run.rs:73-93`: hook events and machine JSON share the 16 MiB/32768-frame diagnostic budget with subprocess output, so a noisy build silently drops the final install event after truncation (`src/launch.rs:458-475`).

### 13. X11: Colocated helper registry with generated docs

Each helper family module declares its own registrations through one macro that takes the Rhai name once and uses it for `register_fn`, tracing and error labels. `register_all` only calls family registrars, and the `pub use` relay blocks go away. The readiness constructors (`ready`/`needs_work`) live with `core::invocation`, which decodes them. Helper configuration (trace flag, HTTP timeout, GitHub token) travels in the invocation instead of process env that never reaches the worker. Helpers that cannot work in the worker (`llm_*`) are removed from it. Shared helper policies are written once: glob semantics, digest validation, the network, proxy and redirect policy for HTTP and git, privilege escalation, not-available vs failure, cwd/env options and package-manager file listing. `HELPERS_AUDIT.md` and `recipe-helpers.7` are generated from the registry (a `recipe helpers --list` mode or an xtask), so they cannot drift.

- **Solves:** 63 problems (36 hidden, 2 needless, 5 surplus, 20 special)
- **Done when:** Each helper name appears once in code. The docs are generated, with a check that fails on drift. No `std::env::var` in helpers. One glob function. One network policy function used by HTTP and git.
- **Depends on:** X12, X10

- **C10** [hidden] `src/helpers/mod.rs:74-88` builds the readiness maps (`status`/`ctx`/`reason`, `ready`/`needs_work`), and `src/core/invocation.rs:336-358` parses that exact shape. They share only string literals.
- **C32** [hidden] `src/core/executor/private/reporting.rs:24` and `actions.rs:224` say "rerun with RECIPE_TRACE_HELPERS=1", but `src/inputs/env.rs:20,84-120` never forwards that variable into the domain, so the advice cannot work.
- **D1** [hidden] `src/helpers/mod.rs:94-514`: each helper name is typed up to three times (`register_fn`, `trace_helper`, `helper_result`), and the copies disagree in nine places. The error label is the Rust name, not the Rhai name: `rm`→`rm_files` (:257), `mv`→`move_file` (:261), `ln`→`symlink` (:265), `ln_force`→`symlink_force` (:269), `chmod`→`chmod_file` (:273), `exec`/3→`exec_with_options` (:382), `exec_checked`/3 (:396), `exec_output`/3 (:410), `dnf_download`/4→`dnf_download_with_resolve` (:485).
- **D2** [hidden] `HELPERS_AUDIT.md:15-94` and `docs/man/recipe-helpers.7:9-243` are two hand-kept copies of the same 78-name helper table, and both must change with every `register_fn` in `src/helpers/mod.rs:73-515`. They are already being edited apart (71cdfcf changed only one, 814dc06 both).
- **D3** [hidden] `HELPERS_AUDIT.md:17-94`: doubled and broken backticks (`:35,72`) suggest generator output that is now hand-maintained, and no generator exists.
- **D4** [hidden] `HELPERS_AUDIT.md:54,55,71` cite owner `helpers/acquire/http.rs`, which is gone (now `http/api.rs`). `:49-50` cite the `build/extract.rs` shim instead of `extract_api.rs`.
- **D5** [hidden] `HELPERS_AUDIT.md:54-55` and `docs/man/recipe-helpers.7:123,126` say `github_latest_*` "strips common prefixes", but the code (`src/helpers/acquire/http/api.rs:114,153`) only does `trim_start_matches('v')`. The real prefix rule is `parse_version` (`api.rs:158-166`), so there are two version-prefix rules.
- **D6** [hidden] `src/helpers/mod.rs:18-37` holds a third, stale helper inventory (it lists `debug` as a helper and `url_utils` as in use, and omits many helpers).
- **D7** [hidden] `src/helpers/mod.rs:6-10` claims helpers are "PURE … no hidden state", but subprocess helpers read the thread-local `COMMAND_ENV` (`src/helpers/internal/cmd.rs:11-13`) set by `core::executor::private::state`.
- **D9** [hidden] `src/helpers/mod.rs:74-88` ↔ `src/core/invocation.rs:336-357`: the readiness map protocol is split across folders (see C). `HELPERS_AUDIT.md:70,75` says "core readiness" owns it.
- **D10** [hidden] `src/helpers/mod.rs:73-515`: helper metadata (name, signature, error label, trace name) lives in a central registry away from each helper. Co-change: `helpers/mod.rs`↔`util/mod.rs` 5×, ↔`util/process.rs` 5×, ↔`install/filesystem.rs` 4×, ↔`install/io.rs` 3×.
- **D11** [hidden] `src/helpers/mod.rs:329-370`: the "Environment utilities" section also registers `http_get`, `github_*`, `parse_version` and `check_disk_space`, and `git_checkout` (:492) sits far from the other acquire helpers (:280-309).
- **D12** [special] `src/helpers/mod.rs:363-370`: `check_disk_space` alone skips `helper_result` and hand-converts `anyhow`, and it reaches through `install::disk::` despite the re-export at `install/mod.rs:30`.
- **D13** [special] `src/helpers/mod.rs:367`: `required as u64` turns a negative int into a huge byte count.
- **D14** [special] `src/helpers/mod.rs:54-64` reads `RECIPE_TRACE_HELPERS` from the process env. Helpers run only in the worker, whose env is the allowlist in `src/inputs/env.rs:20,89-99`, so the variable can never be set. It is still advertised at `src/core/executor/private/reporting.rs:24` and `actions.rs:224`.
- **D15** [special] `src/helpers/mod.rs:497-514` registers `llm_*` into the worker-only engine, though the docs (`HELPERS_AUDIT.md:62-64`, `recipe-helpers.7:146-153`) say they are unavailable in the worker. They fail at call time (`src/helpers/llm.rs:85`).
- **D18** [hidden] Three URL validators: `src/helpers/acquire/mod.rs:20-27` (http/https), `src/helpers/acquire/git.rs:9-20` (http/https/file) and `src/helpers/internal/url_utils.rs:31-47` (http/https/ssh/git@/magnet, dead).
- **D19** [hidden] The no-proxy/redirect policy is stated twice with different results: HTTP uses `src/helpers/acquire/mod.rs:35-37` (5 redirects), Git uses `src/helpers/acquire/git.rs:33-36,76-81` (none). The doc at `acquire/mod.rs:29-31` claims one policy.
- **D20** [hidden] Two timeout policies on one client builder: `src/helpers/acquire/download.rs:86` uses 300 s, `src/helpers/acquire/http/api.rs:20-30` uses `RECIPE_HTTP_TIMEOUT` (30, clamped 5..300), and `acquire/mod.rs:38` adds a 30 s connect timeout.
- **D21** [hidden] `src/helpers/acquire/download.rs:57-62` wraps `src/helpers/acquire/verify.rs:24-26`, and `http/api.rs:188` reaches it through `super::super::download::validate_sha256`. It is then re-run by `download()` (`:36`), and `git.rs:261` has its own inline hex check.
- **D22** [hidden] `src/helpers/acquire/verify.rs:71-90`: `verify_*` never check the digest format, so a bad digest reports "integrity check failed" (`src/helpers/internal/hash.rs:61`), whereas `download` rejects it up front.
- **D23** [needless] `src/helpers/acquire/http/api.rs:42-43,75,258` reach into the private `super::super::http_request`/`download::download`, and `download.rs:37,86` into `super::validate_http_url`/`http_request`. The submodules are coupled through private helpers.
- **D25** [special] `src/helpers/acquire/download.rs:136-140`: resume with no prefix repeats the plain download path but drops the "downloaded … bytes" line (`:76`).
- **D30** [hidden] `src/helpers/acquire/http/api.rs:99-108,136-145,202-210,270-278` are four copies of the GitHub error mapping. `:97,201,269` are three copies of the `releases/latest` URL, and `:215-218,282-285` repeat the assets extraction.
- **D31** [special] `src/helpers/acquire/http/api.rs:101,138,203,271` map every 403 to "rate limit exceeded".
- **D32** [special] `src/helpers/acquire/http/api.rs:20-30,37-39` read `RECIPE_HTTP_TIMEOUT`/`GITHUB_TOKEN` from `std::env`, which the worker allowlist never fills (the module's own doc at `:6` says so). These are dead knobs.
- **D36** [needless] `src/helpers/acquire/http/api.rs:158-166`: `parse_version`, a pure string function, lives in the HTTP module.
- **D37** [hidden] `src/helpers/acquire/http/api.rs:168-181`: the `github_download_release` doc omits the required `sha256`.
- **D40** [hidden] `src/helpers/acquire/git.rs:56-82`: a hand-kept list of 26 env vars to strip, which overlaps the `-c` overrides (`:26-49`) and the ureq proxy policy.
- **D44** [hidden] `src/helpers/acquire/git.rs:163,185` run `rev-parse --verify HEAD` twice, and `validate_checkout` must precede `select_commit` (`:218-219,247-248`) with nothing enforcing the order.
- **E5** [hidden] Glob expansion is written five times with different error policies: `src/helpers/install/filesystem.rs:30` (fails), `:45` (entry errors = non-match), `:133` (fails), `src/helpers/install/io.rs:89` (drops both), and the dead `fs_utils.rs:36`.
- **E13** [special] `src/helpers/install/filesystem.rs:405,409-413`: `chmod` casts `mode as u32` unchecked, and is a no-op on non-unix.
- **E14** [hidden] `src/helpers/install/filesystem.rs:335-341` repeats the array normalization from `src/helpers/util/process.rs:341-355,357-374`.
- **E15** [special] `src/helpers/install/io.rs:15-17`: `read_file_or_empty` swallows every error, so `append_line_if_missing` (`:45`) appends after a permission error.
- **E16** [special] `src/helpers/install/io.rs:89-96`: `glob_list` returns empty for an invalid pattern, while `glob_exists` (`filesystem.rs:45`) errors.
- **E17** [special] `src/helpers/install/disk.rs:34-42` turns a failed `df` into a warning and returns Ok.
- **E19** [hidden] `src/helpers/install/disk.rs:74-81` parses `df -k` column layout (line 2, field 4) with no contract.
- **E20** [special] `src/helpers/install/disk.rs:49-60` falls back to the process cwd when the path and its parent don't exist, so it can measure the wrong filesystem.
- **E23** [surplus] The `pub use` blocks in `src/helpers/install/mod.rs:16-30` and `src/helpers/util/mod.rs:21-43` exist only so `register_all` can write `install::x`. Each helper is listed in four places (impl, re-export, registry, docs).
- **E30** [hidden] Two cwd mechanisms: `shell_in` (`src/helpers/util/shell.rs:8-15`) is unvalidated, while `exec(…, #{cwd})` (`src/helpers/util/process.rs:201-208`) validates. Shell helpers have no `env` option.
- **E31** [hidden] `src/helpers/util/process.rs:40-49`: the Sudo and Doas arms are identical except for the binary name.
- **E32** [surplus] `src/helpers/util/process.rs:341-355` `normalize_packages` is `normalize_strings` (`:357-374`) with a fixed message.
- **E33** [hidden] `src/helpers/util/process.rs:437-448` and `:450-461` (`list_rpm_files`/`list_apk_files`) are copies, and the before/after diff in `dnf_download_impl` (`:476-496`) is copied in `apk_download` (`:525-537`).
- **E34** [hidden] `src/helpers/util/process.rs:306,320` repeat `"lib/apk/db/installed"`, and `parse_apk_policy` (`:128-157`) parses `apk policy`'s human-readable output.
- **E35** [special] `src/helpers/util/process.rs:516-528`: `apk_download` runs unprivileged while `dnf_download` (`:470`) requires sudo/doas/root.
- **E36** [special] `src/helpers/util/process.rs:163-165,292-296,330-332` treat any nonzero exit (including network failures) as "not available".
- **E37** [hidden] `src/helpers/util/process.rs:470,274,281,293…`: the helper-name strings passed to `require_*` are hand-copies of registry names.
- **E40** [hidden] `src/helpers/util/log.rs:8` says `debug` shows "only in verbose mode", but it emits unconditionally (`:10`). HELPERS_AUDIT routes `log` "via core/output.rs", but it actually goes via `logging::diag`.
- **E42** [surplus] `src/helpers/llm.rs` (all three helpers) is registered (`src/helpers/mod.rs:497-514`) into the worker-only engine, where there is no HOME, XDG config or provider binary. It is dead in practice.
- **E43** [hidden] `src/helpers/llm.rs:17-18` and `src/bin/recipe/cli.rs:85` promise that `--llm-profile` covers "LLM features", but `src/helpers/llm.rs:86` passes `None`. Only autofix gets the profile (`src/lib.rs:257` → `src/core/autofix/patch.rs:252`).
- **E73** [special] `src/helpers/llm.rs:30-41` scrapes `recipe_file: "…"` from content, a key nothing produces.
- **E75** [hidden] `src/helpers/llm.rs:6-8` claims no interpretation of output, but `:107` strips fences.
- **B76** [hidden] `src/bin/recipe/commands.rs:195-198` tells users to paste `verify_sha256("<hash>")`, but the helper is `verify_sha256(path, expected)` (`src/helpers/acquire/verify.rs:71`). Confirmed in `.untangle/out/baseline-cli/hash-hello.txt`.
- **J5** [hidden] `src/helpers/util/process.rs:476-496,525-537`: `dnf_download`/`apk_download` return a before/after directory diff, so a package already present in `dest_dir` is downloaded but not returned.
- **J6** [special] `src/helpers/acquire/download.rs:52-54` vs `src/helpers/acquire/git.rs:209-222`: `git_checkout` validates and reuses an existing destination, while `download(resume=false)` always refetches. The two acquire helpers have opposite reuse rules.
- **J9** [special] `src/helpers/util/process.rs:273-288`: `rpm_installed` maps any nonzero exit to `false`, while its sibling `rpm_version` errors.
- **J10** [special] `src/helpers/install/io.rs:20-37`, `filesystem.rs:358-368`: `write_file`, `append_file`, `mv` and `ln` fail on a missing parent, while the `copy_*` helpers, `extract` and `download` create parents. Parent creation is not a stated contract.
- **J11** [special] `src/helpers/install/filesystem.rs:29-41,142-144`, `io.rs:86-99`: zero glob matches is an error for `copy_into_dir` but a silent no-op for `rm`/`glob_list`.
- **J19** [special] `src/helpers/acquire/verify.rs:36-48`: the `ALGO (file) = hash` checksum branch never checks that ALGO is SHA256.
- **J21** [hidden] `src/helpers/util/process.rs:38-49,73-116`: `command_exists`/`require_dnf` find tools via `COMMAND_ENV` PATH, but `sudo -n dnf` resets env (`secure_path`), so the binary checked and the binary run can differ.
- **J22** [hidden] `src/helpers/acquire/mod.rs:37` (`redirects(5)`) ↔ `src/inputs/network.rs:20-67`: redirects work only if every redirect host is also a pinned `--network-host` grant. `github_download_release` (`http/api.rs:248-258`) needs three unnamed grants.
- **E38** [surplus] `src/helpers/util/process.rs:175-177` `apk_repositories_file` returns a constant. `:412-414` `apk_install` forwards. `:499-514` `dnf_download`/`dnf_download_with_resolve` both forward to `_impl`.
- **E74** [surplus] `src/helpers/llm.rs:22-28` `current_working_dir`/`to_eval_err` are one-line wrappers.

### 14. X14: One LLM interface; autofix built on shared parts

`llm::complete(config, prompt) -> String` hides provider dispatch, fence stripping and codex's tempfile lifetime behind a `Provider` trait that also enforces the no-tools policy per provider. Config is one serde struct merged generically: either provider is optional, invalid values are rejected rather than clamped, and XDG order is correct. Autofix owns `AutoFixConfig` and states its patch policy once, in a validator from which the prompt rules are generated. It uses the X9 resolver for bases, the X12 runner for git, and a `Phase` table for protected function names. Kernel-specific context goes away in favour of a generic "declared extra context" rule.

- **Solves:** 45 problems (22 hidden, 5 needless, 6 surplus, 12 special)
- **Done when:** Exactly one provider match, one fence stripper, one diff-header parser and one path validator. No `linux-deps` literal in autofix. `git apply` doesn't touch coordinator stdout.
- **Depends on:** X9, X12

- **E44** [hidden] `src/helpers/llm.rs:85-108` `run_provider` and `src/core/autofix/patch.rs:251-272` `run_llm` are the same provider dispatch (match, build, `run_call`, finalize, strip fences).
- **E45** [hidden] `src/helpers/llm.rs:43-63` and `src/core/autofix/prompt.rs:19-34` are identical `strip_outer_code_fence` copies.
- **E46** [needless] `src/llm/mod.rs:5-12` exports per-provider internals (`codex::build/finalize`, `claude::build/finalize`, `run_call`) instead of one invoke call, so every caller knows each provider's steps.
- **E47** [hidden] `src/llm/providers/codex.rs:31,78,108-131`: callers must keep `inv.last_message_file` alive across `run_call` and pass it to `finalize` (`src/helpers/llm.rs:93-97`, `src/core/autofix/patch.rs:257-261`).
- **E48** [hidden] Adding a provider touches about eight places: `src/llm/provider.rs:4-27,47-48`, `src/llm/config.rs:31-35,132-145,245-246`, `src/llm/mod.rs:9-12` and both dispatch matches.
- **E49** [hidden] "Pure text transform, no tools" is enforced only for codex (`src/llm/providers/codex.rs:42-44,64-76`); `claude.rs:14-65` disables nothing.
- **E51** [special] `src/llm/providers/codex.rs:10-25` swallows read/parse errors, so an unreadable file leaves MCP servers enabled.
- **E53** [hidden] `src/llm/providers/claude.rs:50-64` and `codex.rs:90-105` build identical `ResolvedCall`s, and the finalize functions (`claude.rs:67-82`, `codex.rs:108-131`) repeat the exit-code error.
- **E54** [surplus] `src/llm/provider.rs:51-65`: `ResolvedCall` re-copies config fields. `provider`/`prompt` (`:53-56`) are dead (`#[allow(dead_code)]`).
- **E57** [needless] `src/llm/provider.rs:30-38` `ProviderConfig` carries codex-only `config_overrides` (ignored by claude), and `effort` means different things per provider.
- **E58** [hidden] `src/llm/providers/claude.rs:26-37` detects user flags inconsistently, so `--output-format=json` gets a second `--output-format text`.
- **E59** [hidden] `src/llm/config.rs:48-174` repeats the field list four times (`LlmToml::merge`, `apply_profile`, `ProfileToml::merge`, `ProviderToml::merge`) plus the structs (`:10-46`).
- **E61** [special] `src/llm/config.rs:245-246` require both `[providers.codex]` and `[providers.claude]` even when only one is used. README's "minimal" example therefore fails (see G).
- **E62** [special] `src/llm/config.rs:250-258` silently clamp `timeout_secs` (≥ 1) and the byte limits (≥ 1024).
- **E63** [special] `src/llm/config.rs:192` falls back to `./.config` relative to the cwd.
- **E64** [hidden] `src/llm/config.rs:204-207,269-276` merge `XDG_CONFIG_DIRS` in listed order, so lower-priority dirs win, the reverse of the XDG spec.
- **E65** [hidden] The path string `recipe/llm.toml` is repeated in `src/llm/config.rs:205,216,235,243,299`, `src/helpers/llm.rs:11-13` and `src/bin/recipe/cli.rs:85`.
- **E70** [needless] `src/llm/runner.rs:75`: the general runner says "coordinator repair cancelled" (autofix vocabulary).
- **E78** [hidden] The patch policy is stated twice and has drifted: the prompt (`src/core/autofix/prompt.rs:10-16`) gives fewer rules than the validator enforces (`src/core/autofix/patch.rs:111-220`: a/ b/ headers, existing regular files, 1–32 paths, no whitespace or quotes).
- **E79** [hidden] `src/core/autofix/patch.rs:138-146` (a line ban) and `:9-109` (range-based) enforce "don't touch lifecycle fns" twice.
- **E80** [hidden] `src/core/autofix/patch.rs:30-47` re-parses the diff headers that `src/core/autofix/parser.rs:3-33` parses, with different rules.
- **E81** [surplus] `src/core/autofix/patch.rs:11,174,190` call `extract_patch_paths` three times on the same patch.
- **E82** [hidden] `src/core/autofix/patch.rs:175-188` and `:201-215` validate the same paths twice against the same allow-roots.
- **E83** [hidden] `src/core/autofix/patch.rs:12` resolves against the raw `cwd`, while `:168,181,208` use the canonical `cwd_abs`.
- **E84** [special] `src/core/autofix/patch.rs:13-15` skip the protected-fn check for unreadable files.
- **E85** [special] `src/core/autofix/patch.rs:168,171` swallow canonicalize errors.
- **E86** [special] `src/core/autofix/patch.rs:191` has a magic limit of 32 paths.
- **E87** [special] `src/core/autofix/patch.rs:305-320` hard-code kernel context: `linux-deps.rhai` is added to the prompt when "linux-deps" is mentioned.
- **E88** [hidden] `src/core/autofix/patch.rs:234` runs `git apply` with inherited stdout on the coordinator, whose stdout is the ctx JSON (`src/bin/recipe/commands.rs:215-216`). This breaks the stdout-is-JSON rule (`src/helpers/util/log.rs:3`, `src/llm/runner.rs:155-156`).
- **E90** [hidden] The default autofix `cwd` is documented three ways (`src/bin/recipe/cli.rs:113`, `src/lib.rs:69-70`), and the code (`src/core/autofix/prompt.rs:53-73`) matches neither.
- **E91** [special] `src/core/autofix/prompt.rs:63-67` prefers the process cwd's git root, so a recipe outside that repo gets allow-roots that exclude itself (`:75-78`).
- **E92** [needless] `src/core/autofix/prompt.rs:169-184` `build_prompt` takes 13 positional params (`too_many_arguments` allowed), mirroring `run_and_apply`'s locals.
- **E93** [hidden] Module names hide their contents: `src/core/autofix/prompt.rs` also holds git-root detection, allow paths, extends resolution and file reads, and `parser.rs` mixes diff parsing with Rhai fn-range scanning (`:67`).
- **H15** [needless] `src/lib.rs:61-88`: `AutoFixConfig` lives at the crate root, but only `core::autofix` uses it (`src/core/autofix/patch.rs:275`, `prompt.rs:53,75`).
- **J12** [special] `src/llm/providers/codex.rs:181-182` vs `claude.rs:59-60`: whether model output is teed to stderr is hard-coded per provider.
- **J17** [special] `src/llm/config.rs:270-273`: `path.exists()` silently skips an unreadable or dangling `llm.toml`, while an invalid but readable one fails hard.
- **J18** [special] `src/core/autofix/prompt.rs:37-41,63-78`: a `git` spawn failure becomes `None` (`.ok()?`), which silently changes the autofix cwd and allow-roots.
- **J23** [hidden] `src/llm/providers/codex.rs:5-7` reads `~/.codex/config.toml`, but Codex honours `CODEX_HOME` (which `[providers.codex].env` can set, `src/llm/runner.rs:94`).
- **J24** [hidden] `src/core/autofix/parser.rs:136-140,168-171,187`: the brace scanner is a partial copy of Rhai's lexer (no backtick strings or `${}`, LF-only offsets), so protected ranges can be wrong silently.
- **J25** [hidden] `src/llm/config.rs:147-173`: within one merge, `args`/`config` are replaced wholesale while `env` is merged per key, and this is undocumented.
- **E60** [surplus] `src/llm/config.rs:12,50-52`: `version` is parsed and merged but never read.
- **E94** [surplus] `src/core/autofix/prompt.rs:128-130,160-167` are near-duplicate `read_to_string` wrappers.
- **E95** [surplus] `src/core/autofix/parser.rs:159-163`: `(name, label)` pairs where every label equals its name.
- **E96** [surplus] `src/core/autofix/parser.rs:193-206` returns a new-file start that its only caller discards (`patch.rs:49`).
- **E25** [hidden] Path-safety ("not absolute, no `..`") is copied in `src/helpers/internal/fs_utils.rs:104` (dead), `src/core/autofix/parser.rs:35`, `src/core/autofix/patch.rs:175` and `src/publication.rs:337`.

### 15. X16: Test harness on the contract, not the internals

Build the e2e suite on a few shared pieces:
- One `ManagedCommand` builder in `tests/e2e.rs` that owns the env and flags: GINUX_* and RECIPE_* set or stripped once, TMPDIR isolated, cwd set.
- Shared fixtures for tar building, marker waiting, zombie scanning and state reading.
- Assertions that use the crate's X2/X3 constants and error `Display` instead of copied literals. Read state through the CLI or a small public inspection API, not private file layouts.
- A single preflight that reports clearly which prerequisites are missing. This keeps the hard-fail policy.
- Pins and patterns read from the distro-builder recipes rather than copied, with every `String::replace` patch asserting that it matched.
For the **Tests** addition, remove or fix the assertions listed under "Test quality (a)" and decide on the near-unit tests in (b).

- **Solves:** 53 problems (29 hidden, 7 needless, 3 surplus, 14 special)
- **Done when:** One command builder. No private-layout reads in tests. No duplicated literals from src. The vacuous assertions are fixed or gone. The suite still passes at the privileged baseline.
- **Depends on:** X2, X3, X7

- **C34** [hidden] `tests/e2e/query.rs:131` asserts the exact text from the format string at `src/core/runner.rs:51`.
- **F1** [hidden] `tests/e2e/managed_target.rs:48-61` `invoke` is a copy of `tests/e2e/kernel_recipes.rs:33-49` `invoke_kernel`.
- **F2** [hidden] Ten builders produce the same "managed" command with different env and flags: `tests/e2e/managed_inputs.rs:212-223`, `managed_state.rs:5-22,443-447`, `managed_target.rs:28-46`, `managed_cancellation.rs:7-17`, `managed_autofix.rs:75-100`, `managed_storage.rs:53-67,147-157`, `managed_retention.rs:27-40` and `downloads.rs:492-512`. Some re-inject `GINUX_BIN` and strip `RECIPE_WORKER`/`RECIPE_INVOCATION`; others inherit them.
- **F3** [hidden] `tests/e2e/downloads.rs:128-133,403-408,503-507` and `preseed_recipes.rs:176` re-derive the `--network-host` authority two ways.
- **F4** [hidden] Five hand-rolled tar-header builders: `tests/image_producer.rs:9-15,130-134`, `tests/e2e/archives.rs:74-87`, `kernel_acquisition.rs:20-26` and `managed_inputs.rs:48-72`.
- **F5** [hidden] `tests/e2e/preseed_recipes.rs:112-117` rebuilds what `create_test_env` (`tests/e2e.rs:17-25`) provides.
- **F6** [hidden] The "source not rewritten" assertion appears 54 times across 13 files (e.g. `cli.rs:118-121`, `managed_state.rs:86-89`), and the bundling helper `query_context` (`query.rs:3-16`) is used only in `query.rs`.
- **F7** [hidden] The `walkdir … "current.json"` walk is duplicated 10 times (`managed_state.rs:91,217,334,371,429,458,556`, `managed_cancellation.rs:185`, `managed_retention.rs:94`, `managed_autofix.rs:122`).
- **F8** [hidden] The stderr-marker watcher is written three times (`managed_inputs.rs:1362-1381`, `managed_cancellation.rs:114-133`, `managed_state.rs:113-130`), and the `/proc/<pid>/stat` zombie scan twice (`managed_inputs.rs:1420-1440`, `managed_cancellation.rs:159-174`).
- **F9** [hidden] `CARGO_MANIFEST_DIR/../../distro-builder/recipes` is hard-coded at `tests/e2e/kernel_recipes.rs:5`, `preseed_recipes.rs:20,206` and `examples/prepare_inputs_demo.rs:8`.
- **F10** [hidden] `tests/e2e/managed_inputs.rs:5,14` hard-code `recipe-scratch-{euid}`/`invocation-` from `src/temporary.rs:22,41,78`.
- **F11** [hidden] `<root>/.artifacts/ginux-export` is hard-coded seven times (`failures.rs:52`, `archives.rs:146`, `api2.rs:86`, `managed_inputs.rs:1515,1620`, `managed_cancellation.rs:286`, `managed_autofix.rs:163`) and relies on `recipe_command` setting `current_dir(root)` (`tests/e2e.rs:51`). If publication moves, the negative checks pass vacuously.
- **F12** [hidden] `"/inputs/main.rhai"` is hard-coded in tests (`managed_state.rs:106,341,343,344`, `managed_cancellation.rs:192`, `managed_autofix.rs:139`, `managed_inputs.rs:1087`), although `inputs::MAIN_NAME_DOMAIN` is `pub`.
- **F13** [hidden] Domain-path literals are asserted at `tests/library_backend.rs:18-23`, `tests/e2e/invocation.rs:55-56`, `managed_inputs.rs:460-462,583,589`, `command_options.rs:75` and `managed_target.rs:12,83`.
- **F14** [hidden] `tests/e2e/managed_inputs.rs:420` hard-codes the 16 GiB input limit (`src/inputs/discover.rs:32`).
- **F15** [hidden] `tests/e2e/process.rs:48` uses 101 MiB to exceed `PROGRESS_THRESHOLD` (`src/helpers/internal/hash.rs:13`), and `:106` asserts the `"checksum:"` text (`hash.rs:100`).
- **F16** [hidden] `tests/e2e/managed_retention.rs:93,99,107` copy the retention bounds (`src/logging.rs:196` `skip(31)`, `src/retained.rs:395-396`).
- **F17** [hidden] `tests/e2e/managed_cancellation.rs:226` uses `sleep 12` to exceed `SHUTDOWN_GRACE` = 10 s (`src/launch.rs:76`).
- **F18** [hidden] `tests/e2e/managed_state.rs:548,552,597` and `managed_inputs.rs:436` copy the target, jobs default and jobs range from `src/bin/recipe/cli.rs:30-36` and `src/options.rs:52,62`.
- **F19** [hidden] Exact src diagnostic strings asserted in tests: `"launcher exit 0; result exited"` (11× in `managed_inputs.rs`, from `src/backend.rs:73`), `"N checkpoint(s) acked"` (`managed_inputs.rs:584,607,651,689`), `"diagnostic limit reached"` (`:826,859`, from `src/launch.rs:474`), `"waiting for shared destination"` (`managed_state.rs:270`, from `src/retained.rs:45`), `"leaf already installed"` (`dependencies.rs:107`), `"recipe: diagnostics: "` (`process.rs:101`), `"backend startup failure"` (`worker_entry.rs:28`), `"complete worker frame deadline expired"` (`managed_inputs.rs:1330`) and `query.rs:131` (`src/core/runner.rs:51`).
- **F20** [hidden] `tests/e2e/preseed_recipes.rs:40-51` and `kernel_acquisition.rs:69` copy SHA-256 pins from `distro-builder/recipes/sources/{alpine,fedora}.rhai` and `linux-prebuilt.rhai:47`, and `preseed_recipes.rs:162-171` patches recipes by literal text from the same files.
- **F21** [hidden] `tests/e2e/kernel_recipes.rs:53,95,108,118,173` and `kernel_acquisition.rs:10,15,82` hard-code kernel releases, paths and errors from distro-builder recipes. A kernel bump in another repo breaks this crate's tests.
- **F22** [hidden] `tests/image_producer.rs:19-31,109-112` hand-write the manifest schema and the `toolchain/rustc`→`usr` rewrite owned by `src/bin/recipe-image.rs:36-69,153,216`.
- **F23** [hidden] `tests/e2e/managed_autofix.rs:57-71` writes a full `llm.toml` schema and assumes its location (`src/llm/config.rs:205-243`).
- **F24** [hidden] `tests/e2e/managed_inputs.rs:822` adds an unexplained `+4096` slack over `MAX_DIAGNOSTIC_BYTES`.
- **F25** [hidden] `tests/e2e/managed_inputs.rs:785,901,1070,1158,1250,1288` assert the absence of `"ginux: audit"` (an external prefix), which passes vacuously if ginux renames it.
- **F26** [needless] Tests read the private state-record schema (`current.json`, `generation`, `states[...]`, `archive`): `tests/e2e/managed_state.rs:96-110,222-234,371-378`, `managed_cancellation.rs:190-198`, `managed_autofix.rs:122-139`, `managed_retention.rs:100-110`.
- **F27** [needless] `tests/e2e/managed_state.rs:207-245` corrupts the private `archive` by seeking into it.
- **F28** [needless] `tests/e2e/managed_retention.rs:44-65` fabricates 40 log dirs in the private format. If `validate_invocation_id` or the allowlist (`src/logging.rs:157`) drifts, pruning skips the fakes and `:93` still passes.
- **F29** [needless] `tests/e2e/managed_cancellation.rs:18-26,51-58,84-91` and `process.rs:103-104` read `logs/<id>/result.json` and `worker.log`/`supervisor-stderr.log`.
- **F30** [needless] `tests/e2e/managed_inputs.rs:1389-1398` asserts one launcher child and exactly three scratch dirs (internal topology).
- **F31** [needless] `tests/e2e/managed_inputs.rs:1573-1601` uses a Python wrapper that relies on the private `--export-file` argv (`src/launch.rs:253`) and rewrites the export tar.
- **F33** [needless] `tests/e2e/worker_entry.rs:58` and `managed_inputs.rs:801,836` import `WORKER_ENTRY_ARG`/`MAX_DIAGNOSTIC_*` from the test build, while `recipe_bin()` (`tests/e2e.rs:10-14`) may run a different binary via `RECIPE_TEST_BIN`.
- **F34** [hidden] `tests/e2e.rs:52-55` overrides `XDG_CACHE_HOME`, which disables the default image lookup, so `GINUX_ROOTFS` becomes silently mandatory.
- **F35** [hidden] `tests/library_backend.rs:8` requires `RECIPE_WORKER=1` in the test env, which leaks into e2e children built by `recipe_command` (`tests/e2e.rs:45-57`), because only some builders strip it.
- **F37** [special] There is no `#[ignore]` anywhere. Missing prerequisites are hard failures by policy (`tests/e2e/managed_inputs.rs:209-211`), so a default `cargo test` cannot pass and there is no host-only subset.
- **F38** [special] `tests/e2e.rs:106,116` gate only `managed_inputs`/`managed_target` on linux, and `archives.rs:65,120,149` add `cfg(unix)` gates that can never matter.
- **F39** [special] `tests/e2e/downloads.rs:29-37` busy-polls `accept` (10 ms sleep, 120 s deadline).
- **F40** [special] `tests/e2e/downloads.rs:447` `let _ = server.join();` swallows the server's panic.
- **F41** [special] `tests/e2e/managed_state.rs:113-130` polls a log every 20 ms for 45 s.
- **F42** [special] `tests/e2e/managed_state.rs:155,163,252` use `sleep 3` in fixtures to hold race windows open, and `:248-290` is timing-dependent.
- **F43** [special] `tests/e2e/managed_inputs.rs:1308-1316,1327`: a Python launcher sleeps 60 s, and the test asserts completion in under 20 s of wall clock.
- **F44** [special] `tests/e2e/managed_inputs.rs:1376-1381,1420-1440` and `managed_cancellation.rs:129-133,159-174` use 120 s marker timeouts and 5 s reap polling.
- **F45** [special] `tests/e2e/managed_cancellation.rs:28-92,202-247` depend on sleep-vs-deadline budgets (3/1/15, 30/2, 12/10/30 s).
- **F46** [special] `tests/e2e/managed_inputs.rs:1621-1623` asserts only `if exports.exists()`.
- **F47** [special] `tests/e2e/git_acquisition.rs:118` and `managed_inputs.rs:327`: `catch (error) { rejected = true; }`, so any error counts as the expected rejection.
- **F48** [special] `tests/e2e/preseed_recipes.rs:65,162-171` and `kernel_acquisition.rs:68-71` patch fixtures with `String::replace` and never assert that it matched.
- **F49** [special] `tests/e2e/managed_state.rs:357` loops over a one-element array.
- **F50** [special] `tests/e2e/managed_state.rs:10`, `managed_target.rs:34` and `managed_inputs.rs:214` `expect` `GINUX_BIN` only to re-set it, so `managed_state.rs:574-617` panics without it although it never launches. `managed_cancellation.rs:233,278`, `managed_state.rs:444` and `managed_inputs.rs:1557` bare-`unwrap` it.
- **F52** [surplus] The dev-dep `leviso-cheat-test` (`Cargo.toml:53`) is used by one attribute (`tests/e2e/cli.rs:2,49-56`), yet every test build needs the sibling checkout and the podman scripts must bind-mount it.
- **F53** [surplus] `tests/e2e.rs:69-70` has a stale "CLI Help and Version Tests" banner.
- **F55** [hidden] `examples/hello.rhai`, which README/WRITING_RECIPES tell users to run, is exercised by no test. `tests/e2e/cli.rs:68-85` `LIFECYCLE` is a parallel fixture.
- **F56** [surplus] `tests/e2e/archives.rs:128` re-parses stdout already parsed by `extracted_root` (`:59-63`).

#### Test quality: worklist for the Tests addition

(a) Tests or assertions that would not catch a real bug:
- `tests/e2e/invocation.rs:126` checks `!recipes.join("unexpected-runtime-setup").exists()`, a path that can never be written. Verified.
- `tests/e2e/downloads.rs:171-174` checks `!recipes.join("payload.dat").exists()`, but the payload goes to `RUNTIME.paths.build`.
- `tests/e2e/managed_inputs.rs:982,990`: the recipe sets the literal `131072`, and the test asserts `131072`.
- `tests/e2e/query.rs:112,151,183` re-assert fixture constants.
- `tests/e2e/git_acquisition.rs:149`: the bare repo's HEAD cannot change.
- `tests/e2e/git_acquisition.rs:118`, `managed_inputs.rs:327`: any error passes.
- `tests/e2e/worker_entry.rs:31-36`: clap's generic "unexpected argument".
- `tests/e2e/cli.rs:7-43`: help/version substrings (`"recipe"` matches the binary name). `cli.rs:65` has a redundant disjunct.
- `tests/e2e/query.rs:209-214`: success only. `:217-247,253-280` use legacy fixtures and substrings only.
- `tests/e2e/command_options.rs:113,135`: `contains("17")`/`contains("29")` match any digits.
- `tests/e2e/managed_inputs.rs:510-513`, `managed_target.rs:195-198`: loose OR conditions.
- `tests/e2e/managed_inputs.rs:785,901,1070,1158,1250,1288`: assert absence of foreign strings.
- `tests/e2e/managed_inputs.rs:1621-1623`: a conditional assertion.
- `tests/e2e/managed_retention.rs:93`: upper bound only.
- `tests/e2e/downloads.rs:447`: swallowed join.

(b) Near-unit tests: integration-shaped (through the CLI), but each pins one helper's argument validation behind a privileged Ginux launch:
- `tests/e2e/command_options.rs:173-198`
- `tests/e2e/downloads.rs:292-325,327-367`
- `tests/e2e/git_acquisition.rs:189-230`
- `tests/e2e/api2.rs:4-50`
- `tests/e2e/managed_state.rs:472-493,574-617`
- `tests/e2e/query.rs:333-347`

No test calls a crate function in isolation, and there are no `#[test]`s in `src/`. The nine `ignore`d doctests in `src/helpers/internal/{fs_utils,hash,progress}.rs` are assertion-free `ignore` examples. They are listed as surplus in D/E.

(c) Environment dependence (nothing gates itself):
- About 120 tests need `GINUX_BIN`, `GINUX_ROOTFS`, `GINUX_ROOT` and sudo. The ones that pass without them are listed in Baseline.
- `tests/library_backend.rs` additionally needs `RECIPE_WORKER_BIN` and a spoofed `RECIPE_WORKER=1`.
- Host tools: `git`, `/usr/bin/python3`, `/bin/sh`, `/bin/false`.
- Image tools: bash, sleep, touch, gcc, cc, true, rm, sh, cp, test, pwd, wc, tail, git.
- Sibling checkouts: `distro-builder/recipes`, `testing/cheat-test`.
- Timing-sensitive tests are listed above.
- Large resources: a 2 GiB payload with `--memory-mib 4096`, 16 GiB sparse files, 101 MiB, 5000 files.
- `/proc` parsing and errno literals (`"os error 18"`, `"os error 13"`).
- Shared `$TMPDIR/recipe-scratch-<euid>` across parallel tests unless `TMPDIR` is isolated (only `managed_inputs.rs:26,1355,1408,1443` and `managed_state.rs:12` isolate it).

### 16. X15: Docs with single sources

Delete the obsolete design docs (`RECIPE_DEPENDENCY_MANAGEMENT.md`, `SYSTEM_VS_USER_RECIPES.md`, `OS_UPGRADES_BRAINDUMP.md`, `APOLOGY.md`, and the stale `CHANGELOG` content). Rewrite `CLAUDE.md` as a short pointer document. Make the man pages the reference, with README linking rather than restating. Generate the option and helper sections from code (clap help and the X11 registry) where practical. `WRITING_RECIPES` includes `examples/hello.rhai` by reference. Fix the Cargo metadata (repository URL, bin declarations, rpm paths) and the packaging scripts, or delete the scripts that cannot work. The bootstrap README points at the manifests instead of copying them.

- **Solves:** 59 problems (52 hidden, 0 needless, 5 surplus, 2 special)
- **Done when:** No DRIFTED item is left. Each fact named in G has one home. `podman-test.sh`/`build-apk.sh` either work or are gone.
- **Depends on:** X11, X4

- **A92** [hidden] `CLAUDE.md` ("No database - State written directly to .rhai files"; script constants `RECIPE_DIR`, `BUILD_DIR`…) contradicts `src/lib.rs:1-26` and `src/worker/run.rs:30-32` (state lives in `retained` JSON records; the worker never rewrites recipes; setup reads `RUNTIME`).
- **B80** [hidden] `src/bin/recipe.rs:3-11` usage docs omit `hash`.
- **B81** [hidden] `src/bin/recipe.rs:25` help footer restates the llm.toml path owned by `src/llm/config.rs:186-207` and the build-destination rule.
- **B91** [special] `src/bin/recipe-image.rs:97-103` depends on host `rpm2archive`, which is not in CLAUDE.md's external-command list.
- **D8** [hidden] `CLAUDE.md:68-69,109` names `git_clone*`, `extract_from_tarball()` and `acquire/torrent`, none of which exist. Torrent leftovers live in `src/helpers/internal/url_utils.rs:19,55-58,87-103`.
- **D56** [hidden] `src/helpers/build/extract_api.rs:9-15`, `src/helpers/acquire/download.rs:10` and `src/helpers/mod.rs:13` hold doc examples that copy the runtime-map shape from `src/core/invocation.rs:28-90`. `CLAUDE.md:58` names yet other constants.
- **E97** [hidden] `src/core/autofix/prompt.rs:136` defines the `// AUTOFIX_PROMPT:` recipe convention, which is documented nowhere.
- **F57** [hidden] `scripts/podman-test.sh:42` runs `cargo test --test e2e` with no Ginux environment, so nearly all scenarios fail by the suite's own policy.
- **G1** [hidden] DRIFTED `CLAUDE.md:51-53` lists the script constants `RECIPE_DIR`, `BUILD_DIR`, `ARCH`, `NPROC`, `RPM_PATH`, `BASE_RECIPE_DIR`, `TOOLS_PREFIX`. None exist; the only constant is `RUNTIME` (`src/core/invocation.rs:28-93`).
- **G2** [hidden] `CLAUDE.md:54` copies the cleanup arity rule from `src/core/runner.rs:58-66` (still accurate).
- **G3** [hidden] DRIFTED `CLAUDE.md:41,121` ("state in recipe files / .rhai files") is false. State lives in retained records (`src/retained.rs:18-22`).
- **G4** [hidden] DRIFTED `CLAUDE.md:68-69,110` names `git_clone*`, `extract_from_tarball()` and torrent, none of which exist.
- **G5** [hidden] DRIFTED `CLAUDE.md:71` (LLM helpers use codex/claude) contradicts `HELPERS_AUDIT.md:62-64`, and neither matches the code.
- **G6** [hidden] DRIFTED `CLAUDE.md:103-115`: the code tree omits most modules (backend, launch, publication, retained, options, temporary, cancellation, logging, inputs, worker, `bin/recipe-image.rs`), and `:107` puts locks in core.
- **G7** [hidden] DRIFTED `CLAUDE.md:94-95,62`: `cargo run -- install ripgrep` implies host execution (it now needs Ginux, and there is no ripgrep recipe), and `./target/debug/recipe` is the wrong path in the workspace.
- **G8** [hidden] DRIFTED `CLAUDE.md:48` calls REQUIREMENTS.md "broader than the implementation", while `REQUIREMENTS.md:3` claims to be the implemented contract.
- **G9** [hidden] The runtime map (`src/core/invocation.rs:28-93`) is copied into `WRITING_RECIPES.md:62-75`, `docs/man/recipe-recipe.5:12-37`, `REQUIREMENTS.md:44-45` and `HELPERS_AUDIT.md:125-127`. DRIFTED: `runtime.host` and `runtime.operation` are missing, and `base_recipe_dir` is called conditional although it is always present.
- **G10** [hidden] `WRITING_RECIPES.md:13-49` is a byte-for-byte copy of `examples/hello.rhai:1-35`.
- **G11** [hidden] DRIFTED `WRITING_RECIPES.md:232-235` and `recipe-recipe.5:83-86` say only "cycles fail" for extends, but the code allows only one level (`src/core/executor.rs:111-118`).
- **G12** [hidden] The cleanup reasons are documented in `WRITING_RECIPES.md:242`, `PHASES.md:14`, `REQUIREMENTS.md:33`, `recipe-recipe.5:49-50` and `recipe.1:36`, with scattered literals in code.
- **G13** [hidden] The `deps`/`build_deps` names are copied in `WRITING_RECIPES.md:222-224` and `recipe-recipe.5:78`.
- **G14** [hidden] `const RECIPE_API = 2` (`src/core/invocation.rs:387-403`) is restated in `README.md:8`, `WRITING_RECIPES.md:5,14`, `HELPERS_AUDIT.md:3`, `REQUIREMENTS.md:7`, `recipe.1:16` and `recipe-recipe.5:5,86`.
- **G15** [hidden] `docs/man/recipe-helpers.7` contains markdown backticks inside troff (`:39,162`), copied from `HELPERS_AUDIT.md`.
- **G16** [hidden] DRIFTED `HELPERS_AUDIT.md:67` and `recipe-helpers.7:162` cite `core/output.rs` (it is now `core/output/mod.rs`).
- **G17** [hidden] DRIFTED `HELPERS_AUDIT.md:62-64` and `recipe-helpers.7:146-153` say the `llm_*` helpers are "unavailable inside the worker", but there is no such check (`src/helpers/llm.rs:85-110`). They only fail for lack of config and binaries.
- **G18** [hidden] DRIFTED `HELPERS_AUDIT.md:17-22,37-41,79-80` say the package helpers run "inside the domain", while `src/helpers/util/process.rs:113` speaks of sudo/doas "on the host".
- **G19** [hidden] Option defaults and ranges are copied into `README.md:168-190,182` and `docs/man/recipe.1:73-101`: timeout 3600 and 1..86400 (`src/options.rs:37,65`, `cli.rs:49`), frame timeout 300 (`src/launch.rs:63,601-609`), memory, objects and network bytes, and jobs 1..16.
- **G20** [hidden] DRIFTED `README.md:189` and `recipe.1:97` say "at most eight" hosts but omit the separate endpoint cap of 8 (`src/inputs/network.rs:59-61`). A hostname with more than 8 addresses fails.
- **G21** [hidden] DRIFTED `README.md:231` and `recipe.1:116` give `--autofix-attempts` as 1–8, but 0 is silently raised to 1, and the default of 2 is documented nowhere.
- **G22** [hidden] Retention is restated in `README.md:153-155` and `recipe.1:129-130` with different wording from `src/retained.rs:392-396`. "Up to 32" diagnostics (`README.md:160-161`, `recipe.1:135`) corresponds to an unnamed `skip(31)` at `src/logging.rs:196`.
- **G23** [hidden] The log file names (`README.md:158-159`, `recipe.1:133-134`) are code literals at `src/launch.rs:261,285` and `src/logging.rs:114,157`. The state root and `logs/` subdir (`README.md:139`, `recipe.1:60`) copy `src/retained.rs:18-22` and `src/logging.rs:79`. The image-cache fallback (`README.md:40-41`) copies `src/inputs/root.rs:44-47`.
- **G24** [hidden] The env var names in `README.md:34-37` and `recipe.1:21-23` copy four code sites. `RECIPE_HTTP_TIMEOUT`, `RECIPE_TRACE_HELPERS` and `RECIPE_WORKER_RECIPES` are read by code but undocumented (and unreachable in the worker; see D).
- **G25** [hidden] DRIFTED `README.md:36-41` presents `GINUX_ROOT` as required, but `src/inputs/config.rs:28,181` silently defaults it (see B).
- **G26** [hidden] The recipes-path default (`README.md:177`, `recipe.1:52-53`) copies `src/bin/recipe/metadata.rs:45-58`. The import exclusion (`README.md:121-122`) copies `src/inputs/discover.rs:131,220`. The linux/x86_64 rule (`README.md:43,181`, `recipe.1:24,69-70`) copies four code sites.
- **G27** [hidden] DRIFTED `README.md:218-226`: the "minimal" llm.toml with only `[providers.codex]` fails, because `src/llm/config.rs:245-246` requires both providers.
- **G28** [hidden] The llm.toml location and profile convention are restated in `README.md:216`, `recipe.1:114`, `cli.rs:85` and `src/helpers/llm.rs:10-18`. The owner is `src/llm/config.rs:177-207`.
- **G29** [hidden] `README.md:244` `RECIPE_TEST_BIN` mirrors `tests/e2e.rs:11`.
- **G30** [hidden] `README.md:207` copies memory values from the superproject `distro-variants/{levitate,acorn}/ring3/sources.toml:4-5`, and quotes an ISO byte count with no findable source.
- **G31** [hidden] `bootstrap/README.md:41-43` copies the `recipe-image` bounds (`src/bin/recipe-image.rs:13-15`). `:8-14,51-57` restate the manifest schema (`recipe-image.rs:28-66`). `:84-93,73-74` re-list the RPMs and tarballs from `bootstrap/ginux-acquisition-image.json`. `:104,107-108` hard-code the output digest `c089f95c…`, also copied into two superproject docs.
- **G32** [hidden] `bootstrap/ginux-build-image.json` and `ginux-acquisition-image.json` duplicate the base, the rust tarball and 6 RPM pins.
- **G33** [hidden] `Cargo.toml:56-64` (generate-rpm) and `scripts/build-apk.sh:136-142` list the same seven asset pairs independently. `build-apk.sh:108-110,147-151` scrape the version, description, license and URL from `Cargo.toml`.
- **G34** [hidden] DRIFTED `Cargo.toml:8-9` and `build-apk.sh:151` give the repo URL `github.com/levitate-os/levitate-recipe`, but the real remote is `LevitateOS/recipe`.
- **G35** [hidden] The podman/apk scripts mount `cheat-test` at `/testing/cheat-test` and the crate at `/workspace` (`podman-test.sh:162-166,184,203,222`, `build-apk.sh:119`) so that `Cargo.toml:53`'s path resolves.
- **G36** [hidden] `Cargo.toml:57` `target/release/recipe` is wrong inside the workspace (the binary goes to `../../target/release`), and a standalone checkout cannot resolve the `cheat-test` path dep.
- **G37** [hidden] `Cargo.toml:15-17` declares only the `recipe` bin, while `recipe-image` exists via autobins, and `README.md:17` and `bootstrap/README.md:22` build it explicitly.
- **G38** [hidden] DRIFTED There are three product descriptions: `cli.rs:6` "Local-first package manager" (stale, shown in `--help`), `Cargo.toml:6` and `recipe.1:3`.
- **G39** [hidden] `recipe.1:153`, `recipe-helpers.7:258` and `README.md:21,103-104` reference markdown docs that packaging doesn't install.
- **G40** [hidden] The three man pages hand-set the same `.TH` date, untied to `Cargo.toml:4`.
- **G41** [hidden] The core contract is restated in up to eight docs (`README.md:101-117`, `WRITING_RECIPES.md:55-113`, `PHASES.md:1-15`, `REQUIREMENTS.md:24-40`, `HELPERS_AUDIT.md:8-11`, `recipe.1:16-19`, `recipe-recipe.5:4-63`, `recipe-helpers.7:5-8`). The failure-cleanup paragraph appears five times, and the git_checkout redirect/cache paragraph four times.
- **G42** [hidden] The crate `AGENTS.md:1-10` restates the superproject `AGENTS.md`.
- **G46** [surplus] `RECIPE_DEPENDENCY_MANAGEMENT.md:1-861` describes a removed architecture (DepGraph, `.rhai.lock`, `installed` vars, `recipe deps`) plus a proposal built on it.
- **G47** [surplus] All of `SYSTEM_VS_USER_RECIPES.md` is obsolete (`/etc/recipe/repos`, `recipe update`/`upgrade`, leviso `RpmExtractor`).
- **G48** [surplus] `OS_UPGRADES_BRAINDUMP.md:35-52` cites `check_update()`/`upgrade`, which exist neither in REQUIREMENTS nor in src.
- **G49** [surplus] `APOLOGY.md:1-37` is a one-off 2026-01-23 incident note about a layout that no longer exists.
- **G50** [surplus] `CHANGELOG.md:23-40` (0.1.0) describes the removed S-expression parser and actions, with no entry recording their removal.
- **G51** [hidden] DRIFTED `CHANGELOG.md:8` `[Unreleased]` is empty and `Cargo.toml:4` is still 0.1.4 despite the Ginux rewrite. `:14-15` advertise Podman runners that cannot pass.
- **G52** [hidden] DRIFTED `scripts/podman-test.sh:166,198,217,237` run e2e in plain containers with no Ginux, image, worker or privileges, and without the README's `--test-threads=1`. `podman-test.sh:196,215,235` and `build-apk.sh:129` use rolling `stable` with `Cargo.lock` gitignored (`.gitignore:3`).
- **G53** [special] `scripts/build-apk.sh:155` prints a hard-coded `…x86_64.apk` path although the arch comes from `apk --print-arch`.
- **H11** [hidden] `src/lib.rs:1-26,194-203` docs restate lifecycle semantics owned by `core::executor::private::flow`.

### Leave as is

- **A49** [special] `src/launch.rs:241-243` uses a magic `_exit(125)` if the parent died between fork and exec.
  - **Why leave:** `_exit(125)` in the post-fork child is the correct async-signal-safe way to report a pre-exec failure; there is nothing to share it with.
- **A52** [special] `src/launch.rs:828-831` kills the process group after a clean exit "because a malformed launcher may leave a child holding its pipes".
  - **Why leave:** Killing the launcher's process group after exit is a real safety requirement (stray pipe holders). Keep it, but name it in X6's teardown.
- **B40** [special] `src/inputs/network.rs:443-458` temporarily chmods the image `/etc` to swap in `/etc/hosts`, working around `root.rs:331`'s read-only modes.
  - **Why leave:** Temporarily relaxing the image `/etc` mode to install `/etc/hosts` follows from the read-only image requirement. It is the cheapest correct implementation.
- **B83** [hidden] Cross-repo: `distro-builder/src/recipe/mod.rs:311-339` and `xtask/src/tasks/kernels/common.rs:156-232` hard-code the CLI flag spellings.
  - **Why leave:** Out of scope: distro-builder and xtask spell the public CLI flags. Those flags are a public contract, not a hidden internal link.
- **B90** [special] `src/bin/recipe-image.rs:123` keeps only RPM `usr/` paths, and `:135-149` keeps only Rust component `bin`/`lib`/`libexec`, remapped to `usr/`.
  - **Why leave:** The RPM `usr/` and Rust component filters are the image builder's product rules, not accidental special cases.
- **D16** [special] `src/helpers/mod.rs:138-147`: `print`/`debug` are wired through `on_print`/`on_debug` to work around Rhai's formatter.
  - **Why leave:** Wiring `print`/`debug` through `on_print`/`on_debug` is how Rhai exposes them. No alternative exists.
- **D17** [surplus] Registered helpers with no in-tree caller (all 22 `.rhai` in the superproject plus the crate's tests and examples were searched): `github_latest_release`, `github_latest_tag`, `github_download_release`, `parse_version`, `http_get`, `fetch_sha256`, `verify_sha512`, `check_disk_space`, `llm_*`. They are public recipe API but unexercised.
  - **Why leave:** Unexercised registered helpers are public recipe API. Removing them is a product decision, not an untangle; ask the user.
- **D26** [special] `src/helpers/acquire/download.rs:131-150` special-cases HTTP 416 by parsing `bytes */N`.
  - **Why leave:** HTTP 416 with `bytes */N` is the standard way a server says a resumed download is already complete.
- **D27** [special] `src/helpers/acquire/download.rs:170`: a resume that gets 200 is rejected rather than restarted.
  - **Why leave:** Rejecting a 200 on resume (instead of silently restarting) is a deliberate safety choice that keeps a partial file's provenance.
- **D39** [hidden] `src/helpers/acquire/git.rs:129-139`: the allowed-config-key list must match what `git clone --template=` writes (`:242`). A git version that writes another key breaks every checkout.
  - **Why leave:** The allowed `.git/config` keys must track what git writes. That is an external contract that can only be checked, not shared.
- **D43** [special] `src/helpers/acquire/git.rs:106-112`: per-path special cases (`alternates`, `commondir`, `config`) inside one loop.
  - **Why leave:** The per-path git checks (`alternates`, `commondir`, `config`) are distinct security requirements.
- **D52** [special] `src/helpers/build/extract_api.rs:161-164` skips a `"."` entry.
  - **Why leave:** Archives legitimately contain a `.` root entry. Skipping it is correct.
- **D53** [special] `src/helpers/build/extract_api.rs:264-298`: APK reads each gzip member into memory, breaks on an empty member and has its own zero-member error. It is the one format that doesn't stream.
  - **Why leave:** APK is a concatenation of gzip members, so per-member buffering is required by the format.
- **E50** [hidden] `src/llm/providers/codex.rs:5-26` parses Codex's private `~/.codex/config.toml` `mcp_servers` table.
  - **Why leave:** Disabling Codex MCP servers requires reading Codex's own config. This is an external contract.
- **E52** [special] `src/llm/providers/codex.rs:66` works around a missing `--no-mcp` flag in one Codex version.
  - **Why leave:** This is a workaround for a missing upstream Codex flag. Revisit when Codex adds `--no-mcp`.
- **F51** [special] `tests/e2e/kernel_recipes.rs:25` must pass `--define KERNEL_TARGET=…`, because `distro-builder/recipes/kernel/metadata.rhai:4-5` throws without it (this conflicts with the workspace rule "Normal public recipes run without required defines").
  - **Why leave:** Out of scope: the required `KERNEL_TARGET` define belongs to `distro-builder/recipes/kernel/metadata.rhai`.
- **F58** [hidden] The test counts in the superproject `docs/recipe-local-validation.md` ("132-case", "134 scenarios") are hand-maintained. There are 136 now.
  - **Why leave:** Out of scope: the hand-maintained test counts live in the superproject `docs/recipe-local-validation.md`.
- **G54** [special] `README.md:60-61` and `bootstrap/README.md:35-37,110-112` record per-workload compiler workarounds (`LD_LIBRARY_PATH=/usr/lib`, one job, `-Wl,--threads=1`, musl `rust-lld` flags) as standing instructions captured nowhere in code.
  - **Why leave:** The compiler workarounds are real current Ginux task-bound requirements, recorded as operator guidance.
- **G55** [special] `bootstrap/README.md:63-66` and `bootstrap/*.json:4`: the base digest is of a tar Ginux assembles locally (via `fetch-rocky-oci.sh`/`prepare-rocky-oci.sh` with `GINUX_OCI_CACHE_DIR_OVERRIDE`), tied to the pin only by prose.
  - **Why leave:** The base digest is produced by Ginux's own OCI scripts. The cross-repo producer is owned upstream.
- **H10** [hidden] `src/lib.rs:210-263`: autofix retry depends on the state identity hashing every input (`src/retained.rs:110-138`), so each patched attempt redoes acquire and build. Only a comment at `src/lib.rs:260-261` notes this.
  - **Why leave:** Redoing acquire and build after an autofix patch is correct: a patched recipe is a new identity.
- **A85** [hidden] `src/bin/recipe/cli.rs:58` says `storage_max_bytes` defaults to "memory minus filesystem metadata", but `src/options.rs:348-351` uses all of memory. They have drifted.
  - **Why leave:** Not a drift: with no explicit cap the Ginux launcher applies its own default (memory minus filesystem metadata), which the help text describes. The recipe's own bound using all of memory is the separate J47 issue.
- **C36** [hidden] `src/core/output/logging.rs:74-78` writes machine JSON and human lines to the same diagnostic stream, which the host copies to stderr (`src/launch.rs:486-487`), so consumers have to pick out the JSON lines.
  - **Why leave:** Machine JSON lines alongside human lines are the documented `--machine-events` contract that callers and tests parse; separating them is a CLI change, not an untangle.
- **C38** [hidden] `src/core/output/narration.rs:11-60` always emits ANSI colour, even into `worker.log` via the protocol sink.
  - **Why leave:** Always-ANSI narration is the current CLI output contract (baseline snapshots capture it byte-for-byte); terminal detection would be a deliberate output change.
- **A81** [surplus] `src/lib.rs:130-136`: `launcher()` falls back to `GinuxLauncher::from_env()`, though the CLI always sets one (`commands.rs:236,257`).
  - **Why leave:** `RecipeEngine::launcher()`'s `GINUX_BIN` fallback is the library default; `tests/library_backend.rs` exercises it (the audit only checked the CLI).
- **E4** [special] `src/helpers/install/filesystem.rs:276` masks input modes `& 0o777` (dropping setuid/sticky), while `:278` keeps full permissions for other sources. One helper, two permission policies.
  - **Why leave:** Staged inputs carry only `& 0o777` modes by construction (discovery masks them), so masking input-derived modes and keeping full permissions for build-produced sources is the same rule applied to different origins.
- **E11** [special] `src/helpers/install/filesystem.rs:88` hard-codes `FICLONE = 0x4004_9409` instead of using libc.
  - **Why leave:** `libc` 0.2.191 does not export `FICLONE` for this target; the literal is the Linux UAPI value.
- **E98** [hidden] "Apply dir permissions after filling the tree" is implemented in `src/helpers/install/filesystem.rs:290,323-326`, `src/publication.rs:291-301` and `src/inputs/root.rs:167-172`.
  - **Why leave:** The three directory-mode passes take modes from different sources (input manifest, export headers with mtimes, image headers with implicit-parent defaults); the shared part is only the deepest-first order.
- **E99** [hidden] Hardlink-preserving tree materialization exists twice: `src/helpers/install/filesystem.rs:248,300-312` (inode map) and `src/publication.rs:197,243` (tar), with different replace rules.
  - **Why leave:** Hardlink-preserving copy (inode map over a host walk) and export materialization (tar link records) start from different representations.
- **J4** [special] `src/helpers/build/extract_api.rs:136-209,301-349`: `extract` writes straight into `dest`, so a bad entry mid-archive leaves earlier entries behind. The acquire helpers stage and publish atomically.
  - **Why leave:** `extract()` targets existing, recipe-owned directories by contract; atomic staging would change its semantics. Recipes clean partial work through their cleanup hooks.
- **E41** [surplus] `src/helpers/util/string.rs:4-33` (`trim`, `starts_with`, `ends_with`, `contains`, `replace`, `split`) are pass-throughs of Rhai's own string methods.
  - **Why leave:** The string helpers are not pass-throughs: Rhai's built-in `trim`/`replace` mutate in place and return `()`, while these return new strings that recipes rely on.

## Progress

Commits land on `ginux-recipe/r06-launch` (per project rule: no new branches). Each fix is checked with `cargo build --all-targets` (no new warnings), the CLI snapshots (`bash .untangle/cli-snapshots.sh <bin> <dir>` vs `.untangle/out/baseline-cli/`) and the privileged suite (`bash .untangle/priv-test.sh <dir>`, ~30 min; baseline 134/134 e2e + 1/1 library + 1/1 image).

Special cases: **168** at audit start.

- [x] **X1** Shrink the public surface and delete dead code. Commit: see `git log --grep "X1:"`.
  - The library API is now `RecipeEngine`, `AutoFixConfig`, `ExecutionOptions`, `GinuxLauncher`, `declared_metadata`/`DeclaredMetadata`, `default_state_directory` and a `#[doc(hidden)] bin_support` facade for the crate's own binaries and tests. Every other module is private, so rustc reports dead code; after the deletions the build has 0 warnings.
  - Deleted: `url_utils.rs`, most of `fs_utils`/`progress`, dead narration/progress/sink/event APIs, the `logging` hook wrappers, unused `inputs` re-exports, `config_text`, unread staging files (`manifest.txt`, `sources.txt`, `env.txt`) and `referenced_as`, unused `PreparedInvocation` fields, the extra-env allowlist machinery, unused `LaunchReport` fields and `read_log_tail`, `github_release_assets` and the test-only `*_with_base` seams, the `http/` and `extract` pass-through layers, non-unix stubs, `RECIPE_WORKER_RECIPES`, the off-linux runtime check, the empty `PreparedScope.bindings`, the constant tools-prefix indirections and no-op `prepare_work_root` calls, unused `with_network_hosts`/`cleanup`/`recipes_path`, `examples/prepare_inputs_demo.rs`, ticket-lineage labels in docs, the `sha3`/`num_cpus`/`semver` deps and the clap `env`/ureq `json` features.
  - Moved to later fixes because those fixes rewrite the same code: A43→X3, A79→X7, A83/B68→X4, C69/I19/I20→X10, I21→X9, E38/E74→X11, E66→X12, E60/E94–E96→X14. Moved to leave-as-is with reasons: A81, E41.
  - Checks: build clean, CLI snapshots byte-identical. Privileged suite: pending (started with the commit).
  - Bugs fixed: none.
- [x] **X2** Name the Ginux domain layout once. Commit: `git log --grep "X2:"`.
  - New `src/domain.rs` owns every in-domain fact: mount points, writable dirs and export roots, reserved image roots and input names, the main recipe/worker/modes-manifest names and paths, the `InputModes` schema and bound, `MAX_PATH`/`MAX_INPUT_NAME`, the worker argv and env names, and `tool_bin_dirs()`. `inputs/{config,env,root,discover}`, `publication`, `retained`, `worker::{run,protocol}`, `core::{invocation,executor::state}` and `helpers::install::filesystem` read it. No domain path literal remains outside `domain.rs`.
  - The worker now checks its whole argv against `domain::WORKER_ARGS` and takes the recipe and search path from `domain`, instead of trusting argv[2]. The modes-manifest bound is checked by both writer and reader.
  - Checks: build clean, CLI snapshots byte-identical. Privileged suite: pending.
  - Bugs fixed: none.
- [x] **X12** One subprocess runner and one diagnostic sink, owned low in the stack. Commit: `git log --grep "X12:"`.
  - New leaf `src/diagnostics.rs` (no crate imports) owns where output goes: the diagnostic stream (`write`/`line`/`diag!`, with the worker installing a protocol sink) and out-of-band stderr (`out_of_band`/`trace`/`progress_target`) for progress redraws and worker traces. `logging.rs` keeps only the invocation log store. `worker::protocol` maps `diagnostics::Stream` to its wire enum, so low-level code no longer imports the protocol.
  - New `src/process.rs` is the single child runner (the Ginux launcher is the documented exception). A `Run` states its stdin, routes, keep policy (all/tail/limit), deadline and own process group, and returns a typed `Exit` (code/signal) or `Error` (spawn/failed). It also owns the thread env overlay (moved from `helpers::internal::cmd`), `find_program` (now checks the exec bit) and `GIT_REPOSITORY_LOCATORS`. `helpers::internal::cmd` is now only the recipe-facing policy: streamed tail, capture, checked capture, and the documented `-1` status contract.
  - Moved onto `process`: helper commands, the LLM runner (tokio removed from the crate), autofix `git rev-parse`/`git apply` (which now strip repository locators, and `git apply` output goes to diagnostics instead of the CLI's JSON stdout), and `getent`. `check_disk_space` uses `statvfs` instead of spawning `df`.
  - Coordinator `eprintln!`s now go through the sink. Hash progress and worker traces use the explicit out-of-band channel, as before. Progress bars are one `Spinner` type that clears on drop; extraction's private copy is gone.
  - Not done here: C38 (always-ANSI narration) moves to X13 with the narration rewrite.
  - Behavior changes (recorded as fixes):
    - (1) `git apply` output no longer reaches stdout (it could corrupt `--json-output`-less JSON).
    - (2) Autofix git calls ignore inherited `GIT_DIR`/`GIT_WORK_TREE`/… (J13).
    - (3) `command_exists`/`require_*` reject non-executable files on PATH (E39).
    - (4) `shell_status` reports drain/IO failures after a clean exit as errors, not `-1` (J15).
    - (5) The disk-space "could not check" warning is visible in the diagnostic stream (it was lost in the supervisor log).
    - (6) A cached git checkout whose `cat-file -e` fails with anything other than "missing" (exit 1) is an error, not a refetch (D42).
    - (7) LLM provider failures report `signal: N` instead of `exit code 1` (E71).
  - Checks: build clean (0 warnings), CLI snapshots byte-identical. Privileged suite: pending.
  - Follow-up commit: the runner blocks in `wait()` unless a run has a deadline. Bug introduced by X12 and caught by the privileged git test: the Ginux domain does not emulate `nanosleep`, so the polling loop panicked inside the worker.
- [x] **X8** Shared secure-filesystem, archive and image primitives. Commit: `git log --grep "X8:"`.
  - New `src/fsx.rs` holds the one copy of `open_regular`, `open_lease`, `private_dir(durable)`, `make_removable` and `copy_hashed` (bounded, cancellable). `retained`, `publication`, `temporary`, `logging`, `inputs::root`, input staging and `recipe-image` use it. Input staging now verifies the staged copy's digest against discovery.
  - New `src/image.rs` holds the image contract: the archive, unpacked and entry bounds, `member_path`, `installed_mode`, `normalize_header` and a `Validator` that applies reserved roots, duplicates, kinds, symlink escape and hardlink-to-earlier-file-with-same-mode. `inputs::root::unpack_image` and `recipe-image` both use it. The producer validates every member in consumer order and defers a hardlink only until its target is written, so it can no longer publish an image the consumer rejects.
  - Producer behavior preserved: rebuilding `compiler-with-git-musl.json` gives the same digest, `9f220633…`.
  - Archive extraction has one tar function (decoder by format) and one `FORMATS` table (detection and aliases). Zip follows tar's rules: symlink members become links validated inside dest, and permission bits are masked to `0o777`.
  - Hashing uses one chunk reader for verify and for `recipe hash`.
  - Helper filesystem: one `ensure_parent_dir`, `remove_existing` and an atomic `replace_symlink`. `ln_force` and tree symlink copies replace atomically, tree copies replace a non-directory destination instead of writing through it, and `copy_file_reflink` clones without truncating first, falls back to copying only on "cannot clone" errors and gives the copy the source mode. Copy errors no longer name the wrong helper.
  - Moved: A67/A68 (export bounds) and J58 to X5; J42 to X6; E25 to X14. Left as is, with reasons: E4, E11, E98, E99, J4.
  - Bugs fixed:
    - (1) D50: tar hardlinks were resolved against the process cwd and failed ("No such file … when hard linking"). New integration test `archives::cli_tar_hardlink_targets_resolve_inside_the_destination` fails on the X1 binary and passes now.
    - (2) J8: zip symlinks were extracted as text files, and setuid bits were kept.
    - (3) J1/J2/J3: no truncate-before-clone, no write-through, no remove-then-create window.
    - (4) J35: state copies are cancellable.
  - Checks: build clean, CLI snapshots byte-identical, archive suite passes privileged (5/5). Full privileged suite: pending.
- X1 privileged result: **133/134**. `git_acquisition::managed_pinned_git_selects_exact_commits_and_preserves_invalid_checkouts` failed once with `protocol frame oversize: declared 1195463749 bytes`: stray bytes on the worker's protocol stdout, interleaved during git output. The same test passed alone on the X12 build, and nothing in X1 touches stdout. Re-running it with the exact X1 binaries to decide between an X1 regression and a pre-existing race (see the X3 note on H2, two stdout writers).
- X1 git failure follow-up: the exact X1 binaries pass that test 3/3 in isolation, so this is an **intermittent failure under parallel load**, not a deterministic X1 regression. Recorded as an open item. Candidate cause: the worker's protocol stdout writers (H2/H3, X3b). Re-check at final verification.
- [~] **X3 (part a: no wire change)** Typed coordinator/worker contract. Commit: `git log --grep "X3a:"`.
  - New leaf `src/failure.rs`: a `Category` enum with a typed `Failure` error that displays the same `backend … failure:` text. All 42 coordinator sites (`backend`, `launch`, `cancellation`) build typed errors through it, and `enrich_stream_error` classifies with `Category::of` instead of substring matching. Cancellation has one constructor. The launch module doc points at the enum instead of listing three of the seven categories.
  - The protocol now owns:
    - `Operation`, which serializes to the same wire strings and is used by lib, backend, launch and worker dispatch. The CLI aliases and the coordinator-side `normalize_operation`/`SUPPORTED_OPERATIONS` are gone, and inputs no longer know the operation.
    - `Phase`/`Reason`: one table of reason strings, with the autofix rule as `Reason::autofix_eligible`. The lifecycle, actions, worker and lib use it.
    - `InvocationOptions::validate_for(op)`: the one force-only-install rule, called at the coordinator entry and on the wire. Downstream re-validation in inputs/config/network is removed. The CLI check stays because the CLI creates directories before the engine runs (X4/X7).
    - `DiagnosticBudget`, used by the worker to chunk and by the host to verify.
    - `valid_invocation_id`/`MAX_INVOCATION_ID`, which inputs, logging and the worker use.
    - `restored_state_budget(options)`, computed from a real worst-case request, which replaces the hand-sized 8192 reserve.
    - `Op::name()`.
  - The worker's checkpoint-ack wait is bounded by the invocation's execution deadline, not a fixed 300 s.
  - Left for **X3b** (wire change plus a protocol version bump): A33 `recipe_digest`, A34/A35/H1 duplicated failure frame and the exit-0 convention, A37 `cleanup_reason` on every request, I5/J50–J52 multi-channel state/ids/persist, H2/H3 worker stdout/stdin coupling, A43, C31/H27 generation sentinel, H28.
  - Checks: build clean, CLI snapshots byte-identical. Privileged suite: pending.
  - X8 follow-up commit: the shared image validator had reworded user-facing errors ("unsafe rootfs image path", "unsafe archive path", …), which broke `image_producer` and two `managed_inputs::cli_image_*` tests in the X8 privileged run. The original texts are restored, and those three tests pass again.
- [x] **X3 (part b: protocol v7)** Commit: `git log --grep "X3b:"`.
  - `PROTOCOL_VERSION` 6 → 7. The retained identity tag now derives from it (A59), so a protocol change invalidates saved state on purpose.
  - The request drops the unread `recipe_digest` (A33) and carries `cleanup_reason: Option<String>` exactly for cleanup (A37, validated). `InvocationOptions` has no cleanup reason.
  - The separate failure frame is gone: `ResultPayload.failure: Option<FailureInfo{fatal, phase, reason, message}>` is present exactly when `!ok` (validated both ways). The backend builds `WorkloadFailure` from that one record (A34/A35: no more "fatal defaults to true" when a frame was missing). `abort` sends the same shape.
  - `AckPayload.generation: Option<u64>` replaces the `UNGROUNDED_GENERATION = 0` sentinel (C31/H27).
  - Still open in X3: H1 (exit-code convention), H2/H3 (worker stdout/stdin writer coupling, the suspect in the intermittent git test), I5/J50–J52, H28, A43. These are concentrated in `launch`/`worker::run` and are taken up with X6.
  - Checks: build clean, CLI snapshots byte-identical. Privileged suite: pending.
- [~] **X7** Stores that own their layout (first part). Commit: `git log --grep "X7:"`.
  - `logging.rs` is replaced by `src/invocation_log.rs`, which owns the log directory layout (file-name constants, with `file()` instead of a `pub directory`), a `Status` enum for `result.json`, retention (`RETAINED_INVOCATIONS`) and the staging name of the record temp file. Crash leftovers (a missing or bad record, or a staging temp file) in a leased directory of ours are now pruned by age instead of being kept forever (J43). Cancelled status uses the typed category.
  - `retained::state_root(Option<&Path>)` is the one state-root resolver (A79/B25/H8), and `retained::default_destination` gives each recipe its own default destination under `<state>/destinations/<hash>` (H20/B72). It is exposed as `levitate_recipe::default_destination`, replacing `default_state_directory`.
  - `RecipeEngine::with_temporary_destination()` owns a temp destination removed with the engine (B73: `--no-persist-state` no longer leaks a directory per run).
  - Identity is an exhaustive destructure of `ExecutionOptions`, so a new field must be classified to compile (A60). It hashes the requested hosts and the byte budget, not the DNS-resolved endpoints (J53/J40/B32).
  - One `Record::validate` serves loading and pruning (A61). The layout names `current.json`, `g-`, `files.tar` and `record.json` are constants (A62).
  - Pruning old state, logs and scratch is best effort with a warning and never fails the new invocation (J29/J41, requirement 13).
  - Behavior changes:
    - Saved state from before this change is not reused: the identity changed, and X3b bumped the protocol.
    - The default destination path changed.
    - Unrelated recipes no longer share a lock.
  - Still open in X7: A65/J44/J46 crash-recovery rules, A71/J36/J37/J57 one leased-pool implementation, J28 commit ordering, J30 root permissions, J33, J54, J55.
  - Checks: build clean, CLI snapshots byte-identical. Privileged suite: pending.
- [~] **X13** Typed events and narration. Commit: `git log --grep "X13:"`.
  - `HookStatus` enum: every `hook_event` call passes a typed status, and narration's sentence and colour tables match it exhaustively (C37). The unreachable `dependency.*` and `check.is_*` running arms are removed (C78). The coordinator's no-op `set_machine_events` call is removed; the flag only travels in `ExecutionOptions` to the worker (C35).
  - `logging.rs` was already split by X12 and X7 (A77), and the unused in-process sinks were removed in X1 (H9).
  - Not changed, with reasons:
    - C36 (machine JSON shares the stream with human lines) is the documented `--machine-events` CLI contract, which tests parse.
    - C38 (always-ANSI narration) is the current CLI output contract, captured byte-for-byte in the baseline snapshots.
    - I14 moves to X10.
  - Checks: build clean. Privileged suite: pending.
- [~] **X4** One Invocation value (library part). Commit: `git log --grep "X4:"`.
  - New `backend::Invocation` (operation, cleanup reason, recipe, search path, persist, options, inputs, destination, state dir) is the single argument of `GinuxLauncher::run` (now `pub(crate)`) and `launch::run_domain`. `RecipeEngine` builds it in one `invoke()`, so the six 8-argument call sites are gone (A27/H13/H14/A39).
  - The public result is `serde_json::Value`: the hand-written JSON→Rhai converter, its `json_kind` copy and the CLI's Rhai→JSON round trip are removed (A24/A25/A26). The non-object check stays.
  - The autofix retry is a plain loop with no `unreachable!`/`expect` (H31).
  - CLI defaults come from `ExecutionOptions::default()`/`AutoFixConfig::default()` (A84/A87/B64); the help output is byte-identical.
  - `tests/library_backend.rs` is updated to read the JSON context.
  - A85 moved to leave-as-is: the help text was correct.
  - Still open in X4 (CLI-visible): flatten the options into one clap `Args` (B65), remove the fixed `--target-*` flags (A83), make `--force-*` install-only (B28), scope `--llm-profile` (B69), stop global flags leaking into list/info/hash (B70), stop creating the recipes dir everywhere (B71), `RECIPE_PATH` via clap env (B79), split `ExecutionOptions` by concern (H16), plus B16, B34, B57, B66, B67, B68, J47.
  - Checks: build clean, CLI snapshots byte-identical. Privileged suite: pending.
- [~] **X15** Docs with single sources (first part). Commit: `git log --grep "X15:"`.
  - Deleted the obsolete `RECIPE_DEPENDENCY_MANAGEMENT.md`, `SYSTEM_VS_USER_RECIPES.md`, `OS_UPGRADES_BRAINDUMP.md` and `APOLOGY.md` (G46–G49), plus `scripts/podman-test.sh`, which ran the managed e2e suite without Ginux (F57/G52).
  - `CLAUDE.md` is rewritten as a short pointer document with the current design rules and source map (A92, D8, G1–G8).
  - `CHANGELOG.md`: the stale 0.1.0 feature list is replaced by a pointer (G50), and `[Unreleased]` records this branch's behavior changes and fixes (G51).
  - `Cargo.toml`: the repository/homepage URL is fixed (G34), `recipe-image` is declared as a bin (G37/B89), and the rhai pin comment lists every `internals` user (G43). `build-apk.sh` gets the URL fix and stops printing a hard-coded arch (G53).
  - `README.md`/`WRITING_RECIPES.md`:
    - the runtime map gains `runtime.host`/`runtime.operation`, and `base_recipe_dir` is noted as empty when unused (G9);
    - extends supports one level (G11);
    - the `GINUX_ROOT` default is documented (G25);
    - the llm.toml example is accurate (G27): after X14 only the default provider's block is required;
    - the autofix-attempts default is documented (G21).
  - `HELPERS_AUDIT.md`/`recipe-helpers.7`: the `log` owner is corrected (G16), and `github_latest_*` "strips a leading `v`" (D5). The owner paths `helpers/acquire/http.rs` and `helpers/build/extract.rs` are accurate again after X1 (D4).
  - Still open in X15: generate the helper tables from the registry (X11), the remaining restated-contract copies (G12–G14, G19–G24, G26, G28–G33, G38–G42), D56, B80, B81, B91, E97, H11.
- [~] **X16 / Tests addition** (first part). Commit: `git log --grep "X16:"`.
  - Removed assertions that cannot fail:
    - `invocation.rs`: a host path the domain cannot write.
    - `downloads.rs`: a recipes-dir file the worker cannot create.
    - `managed_inputs.rs`: a literal the recipe sets itself, echoed back.
    - `git_acquisition.rs`: the HEAD of a read-only origin.
  - Made checks specific:
    - The worker-entry test names the rejected flag.
    - `--version` must contain the crate version.
    - The not-found check drops its redundant disjunct.
    - The exit-code checks match `exit status: N`, not any digits.
    - The failed-publication check is one unconditional assertion.
    - Retention checks a range rather than only an upper bound.
    - The download fixture's thread is joined, not swallowed.
  - Kept, with reasons:
    - The "write is denied" catch-all and the loose-OR message checks: the denial is the behavior, and the exact errno text varies.
    - `query.rs` state assertions: they verify the check returns the ctx with its state.
    - The `ginux: audit` absence checks: they guard an external contract on purpose.
  - No ignored doctests remain: the nine `ignore` examples were removed in X1 and X8. There are no unit tests.
  - Still open in X16: the shared `ManagedCommand` builder and fixtures (F1–F8), tests reading private layouts (F26–F33), literals copied from src (F10–F25), the environment preflight (F37), timing-dependent fixtures (F39–F45), and F52 `cheat-test`.
- [~] **X11** Helper registry (first part). Commit: `git log --grep "X11:"`.
  - The `llm_*` helpers are removed from the worker, which has no provider, config or credentials (E42/E43/E73/E75/D15; approved default). `helpers/llm.rs` is deleted, and the catalog rows and README are updated.
  - Knobs that cannot reach the worker are removed (D14/D32/C32):
    - the `RECIPE_TRACE_HELPERS` trace (with every `trace_helper` call and the "rerun with RECIPE_TRACE_HELPERS=1" advice);
    - `RECIPE_HTTP_TIMEOUT`, now a fixed 30 s metadata timeout;
    - `GITHUB_TOKEN`: GitHub requests are unauthenticated, as they effectively already were in the worker.
  - The stale check advice ("return ctx on success path") now names `ready`/`needs_work` (C33).
  - `recipe hash` prints `verify_sha256(path, "<hash>")` (B76). This is the one intended CLI snapshot difference (`hash-hello.txt`).
  - Still open in X11: per-family registration with one name per helper (D1/D10/D11/E23), readiness constructors owned by core (C10/D9), generated catalog docs (D2/D3/D6/D37), and the consistent helper policies (E5, E13–E20, E30–E37, E40, D18–D23, D25, D30, D31, D36, D40, D44, J5, J6, J9–J11, J19, J21, J22).
- [~] **X14** One LLM interface (first part). Commit: `git log --grep "X14:"`.
  - `llm::complete(prompt, cwd, profile)` is the only entry point: it hides provider dispatch and codex's last-message-file lifetime. Provider internals are private (E46/E47/E44). The fence-stripper copy went with `helpers/llm.rs` in X11 (E45), and the runner's autofix-specific vocabulary went with the X12 rewrite (E70).
  - Config requires only the selected provider's block (E61). Limits below their minimum are rejected instead of silently clamped (E62).
  - Still open in X14: the provider trait with a per-provider no-tools policy (E49), generic config merge (E59, E60, J25), XDG order (E64), the `recipe/llm.toml` string copies (E65), the cwd fallback (E63), autofix's single patch validator and prompt rules (E78–E86), generic extra context (E87), `git apply`/cwd docs (E90, E91), `build_prompt` parameters (E92), module naming (E93), `AutoFixConfig` placement (H15), E25, E94–E96, J12, J17, J18, J23, J24.
- [~] **X5** Ginux adapter (first part). Commit: `git log --grep "X5:"`.
  - `DomainConfig::for_invocation` takes the network policy as a required argument; the `NetworkPolicy::default()` plus chained `with_network` trap is gone (B30).
  - `inputs` owns the staged config's file name (`PreparedInvocation::config_path()`), and launch no longer re-derives it (A16).
  - A non-UTF-8 `GINUX_ROOT` is an error instead of silently selecting the default pin (J45).
  - The result-record `signal` rejects negative values instead of casting (A57).
  - `init` validation against the wrong bound is moot: the init is the `domain` constant (B36).
  - Still open in X5:
    - A `ginux` module owning the result-record and snapshot-control wire formats (A18, A19).
    - Two-meaning restore-file handling (A47/J34/B46/B54–B56).
    - The fd-inheritance registry (A48).
    - Resolving `GINUX_*`/`RECIPE_WORKER_BIN` once in a setup value (B19–B24).
    - The ginux-domain limit copies (B11, B12, B14, B15, B17).
    - Single-held network policy (B31), its field access (B33), the leak re-parse (B37/B38), getent polling (B39), and the exporter contract (J58, A67, A68).
- X8 privileged result: **e2e 133/135** (135 = 134 baseline tests + the new hardlink test). The 2 failures were the image error-text regression fixed in the X8 follow-up commit. library_backend 1/1. image_producer failed on the same message regression and is fixed (verified unprivileged after the fix). The intermittent git test passed in this run.

