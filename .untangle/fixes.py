#!/usr/bin/env python3
"""Fix plan for the recipe untangle: every problem ID from UNTANGLE.md assigned to exactly one fix
or to LEAVE. Run `python3 -I .untangle/apply_fixes.py UNTANGLE.md` to regenerate `## Fixes`."""

def ids(spec):
    out = []
    for part in spec.split():
        if "-" in part:
            a, b = part.split("-")
            p = a.rstrip("0123456789")
            for n in range(int(a[len(p):]), int(b[len(p):]) + 1):
                out.append(f"{p}{n}")
        else:
            out.append(part)
    return out

# Order = execution order (foundations first, then highest problem-count per unit of risk).
FIXES = [
    dict(key="X1", title="Shrink the public surface and delete dead code",
         summary="Make every module except the deliberate library API (`RecipeEngine`, options, errors) private or `pub(crate)`, so rustc reports dead code. Then delete it: unused functions and re-exports, dead fields, unreachable branches, non-unix fallbacks for a linux-only crate, transitional ticket scaffolding and stale doc references, and unused Cargo deps and features.",
         done="`cargo build --all-targets` passes with no `dead_code`/`unused` allows added. None of the listed items exist. `lib.rs` exports only the intended API. `sha3`, `num_cpus` and `semver` and the clap `env`/ureq `json` features are gone or used.",
         depends="none",
         ids="A8 A9 A17 A41-A45 A78-A83 A88 A91 C53 C62 C63 C68-C75 C77 C79 D24 D28 D29 D33-D35 D38 D45 D55 D57 D69 D70 D72-D74 "
             "B35 B41 B42 B49-B51 B59-B61 B68 E12 E21 E22 E24 E38 E41 E55 E56 E60 E66 E68 E74 E94-E96 F54 G44 G45 H18 H19 I19-I21 I28"),
    dict(key="X2", title="Name the Ginux domain layout once",
         summary="Add a small, dependency-free `domain` module that owns every in-domain fact: the mount points (`/inputs`, `/work`, `/work/build`, `/work/tools` and its bin dirs, `/work/tmp`, `/out`), the writable roots, the main recipe name, the worker file name, `.recipe-input-modes.json` and its schema, the reserved input names, the worker argv shape and its env names (`RECIPE_WORKER`, `RECIPE_INVOCATION`), and derived length bounds. The coordinator (inputs, config, root, env, publication, retained) and the worker (run, invocation, executor, helpers) both import it, so neither side spells a path.",
         done="A grep for the domain path literals and reserved names outside `domain` finds only tests asserting via the constants. The worker entry, the args written by config and the worker lookup all use one definition.",
         depends="X1",
         ids="A1-A7 C8 C9 B1 B2 B4 B5 B7 B18 B63 B82 E1-E3 H12 I3"),
    dict(key="X12", title="One subprocess runner and one diagnostic sink, owned low in the stack",
         summary="Define a `DiagnosticSink` trait in a leaf module that depends on nothing. The worker installs a protocol-backed sink; the coordinator CLI installs a stderr sink that decides colour/TTY. All human output goes through it: narration, progress bars, hash progress, helper warnings, the backend summary and the LLM tee. `helpers::internal::cmd` becomes the only way to spawn a child. It owns env application order, bounded capture, routing, command lookup and a typed `ExitStatus` (exited code / signalled / spawn failed). `df`, autofix `git`, the LLM runner (dropping tokio) and every helper use it.",
         done="`logging`, `core::output` and `cmd` no longer import `worker::protocol`. No `eprint!`/`eprintln!`/`ProgressDrawTarget::stderr` outside the sink implementations. No `Command::new` outside the runner. `-1` exit-code sentinels are gone. tokio is dropped or confined.",
         depends="X1",
         ids="A74-A76 C2 C16 C38 C39 D41 D42 D47 D48 D58-D64 D66 D68 E18 E26-E29 E39 E67 E69 E71 E72 E89 H30"),
    dict(key="X8", title="Shared secure-filesystem, archive and image primitives",
         summary="One `fsx` module holds the primitives each copied today: open regular file without following links, create or accept a private 0700 dir, make a tree removable, bounded hash-copy, relative-path safety, apply dir modes after fill, hardlink-preserving materialization, reflink-or-copy, and remove-if-exists. One `archive` module owns the tar/zip/apk extraction policy (one stay-inside-dest rule, correct hardlink resolution against dest, one format table, one bounds formula). One `image` module owns the rootfs image contract (limits, entry kinds, links ordering, reserved paths, ownership/mode rules), used by both `recipe-image` (declared as a bin and packaged) and `inputs::root`.",
         done="Each primitive has one implementation. The hardlink check and extraction agree. `recipe-image` imports the library's image module, and its limits and validation equal the consumer's. `recipe hash` uses the hashing primitive directly.",
         depends="X1",
         ids="A67-A70 A72 B3 B58 B77 B84-B89 B92-B94 D46 D49-D51 D54 D65 D67 D71 E4 E6-E11 E25 E98 E99"),
    dict(key="X3", title="Typed coordinator/worker contract",
         summary="Make `worker::protocol` the single owner of what crosses the process boundary, with types instead of strings:\n"
                 "- `Operation` enum (`Install`, `Remove`, `Cleanup{reason}`, `Check(Phase)`), with aliases parsed once at the CLI edge.\n"
                 "- `FailureReason` (phase × outcome) and `FailureCategory` (startup/protocol/workload/supervisor/deadline/storage/cancelled) enums with `Display`.\n"
                 "- A single failure report (no duplicated frame plus `result.failures`).\n"
                 "- A `ValidatedOptions` newtype built once.\n"
                 "- An `InvocationId` type.\n"
                 "- Shared limits and timeouts carried in the request (ack timeout, diagnostic budget as one shared function).\n"
                 "- `Option<Generation>` instead of 0.\n"
                 "- One state channel: every recipe's state goes via checkpoints, and the final result carries outputs only.\n"
                 "Autofix eligibility becomes a method on `FailureReason`. The worker's exit-code convention is replaced by the typed result.",
         done="No operation, reason or category string literals outside `protocol`'s `Display`/`FromStr`. `validate()` runs once. The force-only-install rule exists once. `enrich_stream_error` matches on types. Worker aliases are gone.",
         depends="X2",
         ids="A10-A12 A20-A23 A28-A37 A40 A58 A63 B13 B26 B27 B29 B43 B44 B62 C11 C31 H1-H3 H17 H21 H27-H29 I5"),
    dict(key="X7", title="Stores that own their layout: state, logs, scratch, destination scope",
         summary="`retained` owns its record schema with a single validator, generation layout names and an `Identity` struct hashed whole. The struct includes `protocol::VERSION`, a typed identity subset of options (from X4's split) and the network policy. The destination lock and default-destination become an explicit `Scope` concept (per-recipe default, no leaked tempdirs) instead of a fake `build_dir`. The invocation log store moves out of `logging` into its own module with a named retention constant, a `result.json` status enum and file names it owns. `temporary` and the log store share one leased-directory-pool implementation. One function resolves the state, cache and recipes roots.",
         done="One `Record` validator. No generation layout literals outside retained. Adding an option forces an identity decision at the type level. One pool implementation. Default destinations no longer collide.",
         depends="X8, X3",
         ids="A13-A15 A59-A62 A64 A65 A71 A73 B25 B32 B72 B73 H8 H20"),
    dict(key="X5", title="One Ginux adapter",
         summary="Collect everything about talking to Ginux into one `ginux` module:\n"
                 "- Rendering and validating the domain config. Prefer validating with the upstream `ginux-domain` crate over hand-copied limits; see the review question.\n"
                 "- The supervisor result record as typed enums.\n"
                 "- The snapshot-control wire protocol (GXS1/GXA1) as constants with a version.\n"
                 "- The restore-file format and its two uses, made explicit.\n"
                 "- Launcher argv and an fd-inheritance registry that every leased resource registers with.\n"
                 "- `GinuxSetup`, which resolves `GINUX_BIN`, `GINUX_ROOT`, `GINUX_ROOTFS`, `RECIPE_WORKER_BIN`, the image-cache fallback and the worker lookup once, at the edge.\n"
                 "- Network policy as a required input of the config, held once.",
         done="No `GXS1`/result-outcome/config literals outside `ginux`. No `std::env::var(\"GINUX_*\")` outside `GinuxSetup::from_env`. The fd list is not hand-maintained. `DomainConfig` cannot be built without its network policy.",
         depends="X2, X8",
         ids="A16 A18 A19 A47 A48 A57 B11 B12 B14 B15 B17 B19-B24 B30 B31 B33 B36-B39 B46 B54-B56"),
    dict(key="X4", title="One Invocation value from CLI to worker",
         summary="Replace the 8-argument `launcher.run` (×6 in lib.rs), the 9-argument `create_engine` (×6 in the CLI) and the per-op worker dispatch with a single `Invocation { recipe, operation, options, defines, inputs }` and `RecipeEngine::run(Operation)` (thin convenience methods optional). Split `ExecutionOptions` by reason to change: identity-relevant settings, resource bounds, presentation (`machine_events`) and install-only force flags (which move into `Operation::Install`). Derive the clap flags from the options struct (`#[command(flatten)]`/`default_value_t = Default`) so defaults and ranges live once, and drop the fixed `--target-*` knobs. `GinuxLauncher` becomes the coordinator entry and stops being a path wrapper. The public result is JSON (`serde_json::Value`), not a `rhai::Map`, which removes the hand-written JSON↔Rhai round trip.",
         done="Adding an option means editing one struct. No JSON→Rhai converter in backend. `lib.rs` and `commands.rs` have no six-way repeated calls. Global flags apply only to the subcommands that use them.",
         depends="X3",
         ids="A24-A27 A39 A84-A87 B16 B28 B34 B57 B64-B67 B69-B71 B79 H13 H14 H16 H31"),
    dict(key="X6", title="Explicit coordinator run pipeline",
         summary="Restructure `launch::run_domain` as an ordered pipeline with typed step results:\n"
                 "- The steps run lock → open state → prepare root → restore/seed → spawn → pump → reap → verify → publish → commit → record.\n"
                 "- A per-`Operation` policy decides whether a run commits state and publishes. Checks don't fabricate state.\n"
                 "- One `Deadlines` value (frame, lock wait, shutdown grace, drain grace) comes from options, not re-read env.\n"
                 "- A cancellation token gives a typed `Cancelled` outcome and is watched by the waits, instead of a global atomic read at labelling time.\n"
                 "- Teardown collects secondary errors next to the primary error instead of swallowing them or masking it.\n"
                 "- The stderr drain is independent of cancellation.\n"
                 "- The output location comes from the invocation, not `current_dir()`.",
         done="No `let _ =`/`.ok()` in teardown without the error being recorded. The publish/commit order and the check-operation policy are explicit in one place. No `RECIPE_FRAME_TIMEOUT_SECS` env read inside launch. `signal()` is not read to relabel errors. `library_backend` no longer writes into the crate root.",
         depends="X3, X5, X7",
         ids="A46 A50 A51 A53-A56 A66 A89 A90 F32 F36 H4-H7 H23-H26"),
    dict(key="X9", title="One recipe and input resolver, one static analyzer",
         summary="A single `recipes` module resolves `//! extends:`, imports and `deps`/`build_deps` (naming, validation, recursion and depth policy). It produces a manifest of recipe-name → domain-path that discovery stages and that the worker's module resolver and dependency lookup read directly, so the worker never re-derives paths. The CLI name lookup, `--input` name rules, autofix's base lookup and metadata listing use the same module. All static AST walking (static references, `declared_metadata`, the API marker) and all Rhai-internals use live in one place next to the `rhai` pin rationale. Input kinds (`rootfs`, `target`) are declared properties, not name checks.",
         done="One `parse_extends`, one resolver, one AST walker. Anything discovery stages is exactly what the worker resolves. The `rhai =1.19.0` comment lists every internals dependency.",
         depends="X2",
         ids="B6 B8-B10 B45 B47 B48 B52 B53 B74 B75 B78 C3-C7 C15 C58 C59 E76 G43 H22 I1 I2 I4 I15 I16 I27"),
    dict(key="X10", title="Lifecycle as data with explicit recipe roles",
         summary="Describe phases with one table (`Phase` → check name, optionality, missing-hook rule, force implication, success/failure reasons, env scope). One phase-block function replaces the three copies, and one recipe-preparation function replaces the four preambles. `Readiness` is kept as an enum end to end, forced runs carry a reason, and the double `is_installed` verify is unified. An explicit `RecipeRole { Main, Dependency }` decides force, failure eligibility, work dirs (dependency dirs outside the main build dir), the output policy (dependencies may not write `/out`) and env guards. It replaces stack depth. Dependencies get a visited set, and a defined policy covers dependencies for remove/cleanup. Dependency tool env vars come from the dependency recipe's declared exports, not hard-coded bison/m4. `ExecutionContext` is built complete, with no call-order setup. The checkpoint payload holds only what the host reads. `has_fn` moves to break the invocation↔runner cycle.",
         done="One phase block and one preamble. No `active_recipes.len() > 1`. No BISON/M4 literals in the engine. The checkpoint sink takes `(recipe_path, state)`. No module cycle in `deps.txt`.",
         depends="X3, X9",
         ids="A38 C1 C12-C14 C17 C19-C30 C33 C40-C52 C54-C57 C60 C61 C64-C67 E77 I6-I13 I17 I18 I22-I26"),
    dict(key="X13", title="Typed events and narration; split logging",
         summary="Hook events become an enum (`Hook` × `Status`) emitted by the lifecycle and rendered by narration with an exhaustive `match`. Machine events get their own channel (a protocol frame kind or a dedicated fd) instead of being mixed into human lines. The machine-events flag lives only in options. `logging.rs` is reduced to the sink plumbing from X12; the log store moves to X7. In-process event-sink setters are removed from the public API, since events happen in the worker.",
         done="No hook/status string literals outside the event enum's serialization. Narration has no unreachable arms. `logging.rs` is gone or holds only the sink.",
         depends="X12, X10",
         ids="A77 C18 C35-C37 C76 C78 H9 I14"),
    dict(key="X11", title="Colocated helper registry with generated docs",
         summary="Each helper family module declares its own registrations through one macro that takes the Rhai name once and uses it for `register_fn`, tracing and error labels. `register_all` only calls family registrars, and the `pub use` relay blocks go away. The readiness constructors (`ready`/`needs_work`) live with `core::invocation`, which decodes them. Helper configuration (trace flag, HTTP timeout, GitHub token) travels in the invocation instead of process env that never reaches the worker. Helpers that cannot work in the worker (`llm_*`) are removed from it. Shared helper policies are written once: glob semantics, digest validation, the network, proxy and redirect policy for HTTP and git, privilege escalation, not-available vs failure, cwd/env options and package-manager file listing. `HELPERS_AUDIT.md` and `recipe-helpers.7` are generated from the registry (a `recipe helpers --list` mode or an xtask), so they cannot drift.",
         done="Each helper name appears once in code. The docs are generated, with a check that fails on drift. No `std::env::var` in helpers. One glob function. One network policy function used by HTTP and git.",
         depends="X12, X10",
         ids="C10 C32 D1-D7 D9-D15 D18-D23 D25 D30-D32 D36 D37 D40 D44 E5 E13-E17 E19 E20 E23 E30-E37 E40 E42 E43 E73 E75 B76"),
    dict(key="X14", title="One LLM interface; autofix built on shared parts",
         summary="`llm::complete(config, prompt) -> String` hides provider dispatch, fence stripping and codex's tempfile lifetime behind a `Provider` trait that also enforces the no-tools policy per provider. Config is one serde struct merged generically: either provider is optional, invalid values are rejected rather than clamped, and XDG order is correct. Autofix owns `AutoFixConfig` and states its patch policy once, in a validator from which the prompt rules are generated. It uses the X9 resolver for bases, the X12 runner for git, and a `Phase` table for protected function names. Kernel-specific context goes away in favour of a generic \"declared extra context\" rule.",
         done="Exactly one provider match, one fence stripper, one diff-header parser and one path validator. No `linux-deps` literal in autofix. `git apply` doesn't touch coordinator stdout.",
         depends="X9, X12",
         ids="E44-E49 E51 E53 E54 E57-E59 E61-E65 E70 E78-E88 E90-E93 H15"),
    dict(key="X16", title="Test harness on the contract, not the internals",
         summary="Build the e2e suite on a few shared pieces:\n"
                 "- One `ManagedCommand` builder in `tests/e2e.rs` that owns the env and flags: GINUX_* and RECIPE_* set or stripped once, TMPDIR isolated, cwd set.\n"
                 "- Shared fixtures for tar building, marker waiting, zombie scanning and state reading.\n"
                 "- Assertions that use the crate's X2/X3 constants and error `Display` instead of copied literals. Read state through the CLI or a small public inspection API, not private file layouts.\n"
                 "- A single preflight that reports clearly which prerequisites are missing. This keeps the hard-fail policy.\n"
                 "- Pins and patterns read from the distro-builder recipes rather than copied, with every `String::replace` patch asserting that it matched.\n"
                 "For the **Tests** addition, remove or fix the assertions listed under \"Test quality (a)\" and decide on the near-unit tests in (b).",
         done="One command builder. No private-layout reads in tests. No duplicated literals from src. The vacuous assertions are fixed or gone. The suite still passes at the privileged baseline.",
         depends="X2, X3, X7",
         ids="C34 F1-F31 F33-F35 F37-F50 F52 F53 F55 F56"),
    dict(key="X15", title="Docs with single sources",
         summary="Delete the obsolete design docs (`RECIPE_DEPENDENCY_MANAGEMENT.md`, `SYSTEM_VS_USER_RECIPES.md`, `OS_UPGRADES_BRAINDUMP.md`, `APOLOGY.md`, and the stale `CHANGELOG` content). Rewrite `CLAUDE.md` as a short pointer document. Make the man pages the reference, with README linking rather than restating. Generate the option and helper sections from code (clap help and the X11 registry) where practical. `WRITING_RECIPES` includes `examples/hello.rhai` by reference. Fix the Cargo metadata (repository URL, bin declarations, rpm paths) and the packaging scripts, or delete the scripts that cannot work. The bootstrap README points at the manifests instead of copying them.",
         done="No DRIFTED item is left. Each fact named in G has one home. `podman-test.sh`/`build-apk.sh` either work or are gone.",
         depends="X11, X4",
         ids="A92 B80 B81 B91 D8 D56 E97 F57 G1-G42 G46-G53 H11"),
]

LEAVE = [
    ("A49", "`_exit(125)` in the post-fork child is the correct async-signal-safe way to report a pre-exec failure; there is nothing to share it with."),
    ("A52", "Killing the launcher's process group after exit is a real safety requirement (stray pipe holders). Keep it, but name it in X6's teardown."),
    ("B40", "Temporarily relaxing the image `/etc` mode to install `/etc/hosts` follows from the read-only image requirement. It is the cheapest correct implementation."),
    ("B83", "Out of scope: distro-builder and xtask spell the public CLI flags. Those flags are a public contract, not a hidden internal link."),
    ("B90", "The RPM `usr/` and Rust component filters are the image builder's product rules, not accidental special cases."),
    ("D16", "Wiring `print`/`debug` through `on_print`/`on_debug` is how Rhai exposes them. No alternative exists."),
    ("D17", "Unexercised registered helpers are public recipe API. Removing them is a product decision, not an untangle; ask the user."),
    ("D26", "HTTP 416 with `bytes */N` is the standard way a server says a resumed download is already complete."),
    ("D27", "Rejecting a 200 on resume (instead of silently restarting) is a deliberate safety choice that keeps a partial file's provenance."),
    ("D39", "The allowed `.git/config` keys must track what git writes. That is an external contract that can only be checked, not shared."),
    ("D43", "The per-path git checks (`alternates`, `commondir`, `config`) are distinct security requirements."),
    ("D52", "Archives legitimately contain a `.` root entry. Skipping it is correct."),
    ("D53", "APK is a concatenation of gzip members, so per-member buffering is required by the format."),
    ("E50", "Disabling Codex MCP servers requires reading Codex's own config. This is an external contract."),
    ("E52", "This is a workaround for a missing upstream Codex flag. Revisit when Codex adds `--no-mcp`."),
    ("F51", "Out of scope: the required `KERNEL_TARGET` define belongs to `distro-builder/recipes/kernel/metadata.rhai`."),
    ("F58", "Out of scope: the hand-maintained test counts live in the superproject `docs/recipe-local-validation.md`."),
    ("G54", "The compiler workarounds are real current Ginux task-bound requirements, recorded as operator guidance."),
    ("G55", "The base digest is produced by Ginux's own OCI scripts. The cross-repo producer is owned upstream."),
    ("H10", "Redoing acquire and build after an autofix patch is correct: a patched recipe is a new identity."),
]

# Completeness pass 2 (section J) assignments.
J_ASSIGN = {
    "X1": "J26",
    "X3": "J50-J52 J59 J60",
    "X4": "J47",
    "X5": "J34 J45 J58",
    "X6": "J31 J32 J35 J38 J39 J49 J61",
    "X7": "J28-J30 J33 J36 J37 J40 J41 J43 J44 J46 J48 J53-J55 J57",
    "X8": "J1-J4 J7 J8 J27 J42",
    "X11": "J5 J6 J9-J11 J19 J21 J22",
    "X12": "J13 J15 J16 J20 J56",
    "X13": "J14",
    "X14": "J12 J17 J18 J23-J25",
}
for _f in FIXES:
    if _f["key"] in J_ASSIGN:
        _f["ids"] += " " + J_ASSIGN[_f["key"]]

# Reassignments made while applying fixes (the log must match what was done).
MOVES = {
    # X1 → later fixes that rewrite the same code anyway.
    "A43": "X3", "A79": "X7", "A83": "X4", "B68": "X4", "C69": "X10", "I19": "X10", "I20": "X10",
    "I21": "X9", "E38": "X11", "E66": "X12", "E60": "X14", "E74": "X11", "E94": "X14", "E95": "X14", "E96": "X14",
    # X12 → X13 (narration rewrite decides styling).
    "C38": "X13",
    # X8 → the fixes that own the export contract, publication and autofix paths.
    "A67": "X5", "A68": "X5", "J42": "X6", "E25": "X14",
    # X13 → X10 (dependency event naming lives in the lifecycle).
    "I14": "X10",
}
NEW_LEAVE = [
    ("A81", "`RecipeEngine::launcher()`'s `GINUX_BIN` fallback is the library default; `tests/library_backend.rs` exercises it (the audit only checked the CLI)."),
    ("E4", "Staged inputs carry only `& 0o777` modes by construction (discovery masks them), so masking input-derived modes and keeping full permissions for build-produced sources is the same rule applied to different origins."),
    ("E11", "`libc` 0.2.191 does not export `FICLONE` for this target; the literal is the Linux UAPI value."),
    ("E98", "The three directory-mode passes take modes from different sources (input manifest, export headers with mtimes, image headers with implicit-parent defaults); the shared part is only the deepest-first order."),
    ("E99", "Hardlink-preserving copy (inode map over a host walk) and export materialization (tar link records) start from different representations."),
    ("J4", "`extract()` targets existing, recipe-owned directories by contract; atomic staging would change its semantics. Recipes clean partial work through their cleanup hooks."),
    ("C36", "Machine JSON lines alongside human lines are the documented `--machine-events` contract that callers and tests parse; separating them is a CLI change, not an untangle."),
    ("C38", "Always-ANSI narration is the current CLI output contract (baseline snapshots capture it byte-for-byte); terminal detection would be a deliberate output change."),
    ("E41", "The string helpers are not pass-throughs: Rhai's built-in `trim`/`replace` mutate in place and return `()`, while these return new strings that recipes rely on."),
]
_moved = {}
for _f in FIXES:
    _keep = []
    for _i in ids(_f["ids"]):
        if _i in MOVES or _i in dict(NEW_LEAVE):
            _moved[_i] = MOVES.get(_i)
        else:
            _keep.append(_i)
    _f["ids"] = " ".join(_keep)
for _i, _k in MOVES.items():
    next(f for f in FIXES if f["key"] == _k)["ids"] += " " + _i
LEAVE += NEW_LEAVE
