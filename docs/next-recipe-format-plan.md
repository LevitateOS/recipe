# Implementation plan: the next Recipe

Status: carried out on 2026-11-16 (phases A to H) while the owner was away, under their standing go; see the last section for every decision taken in their place. Nothing was run except the checks the plan allows and H3/H4. It covers
everything the owner decided that day: the recipe format
([`next-recipe-format.md`](next-recipe-format.md)), disk space, single steps,
the features that go, update checks, and the app. It was written from four
read-only surveys of the code: the engine, all recipes, everything else that
reads recipes, and what Recipe leaves on disk.

## How the work is done

- The main session plans, briefs, reviews and commits. It changes no code.
  Every change is made by an agent on the model named here.
- No backwards compatibility. One format, `RECIPE_API = 1`. Old forms and
  removed features are deleted, not kept beside the new.
- Recipe never asks an AI. No step adds anything that calls a model.
- Nothing is run to prove the work: no installs, no builds, no test suites.
  Allowed checks are compiling, linting and `recipe info FILE` (seconds).
  Two exceptions need the owner's yes and are marked **[ask]**.
- Phase A is done in the shared checkout: small removals, committed as they
  finish. Phases C to G are done in a second clone of the repositories and
  reach the real ones in one landing (phase H). No branch is created.
- While phases C to G run, no other session changes a recipe or an
  OS-building recipe. One that does is converted again at landing.

## The phases at a glance

| Phase | What | Models | Lands |
|---|---|---|---|
| A | Removals: everything that asks an AI, "go back", "Update all", "Keep at this version" | sonnet ×2, opus ×1 | at once, by itself |
| B | The complete specification, for the owner's approval | opus ×1 | as a document |
| C | The engine | opus ×6, sonnet ×1 | with H |
| D | The pin generator and the builder | sonnet ×2 | with H |
| E | All 106 recipes | haiku ×2, sonnet ×6, opus ×4 | with H |
| F | The Recipe app | sonnet ×3 | with H |
| G | Recipe's own tests, the documentation, the strays | sonnet ×5, haiku ×2 | with H |
| H | Review, final checks, landing | sonnet ×1, haiku ×2, the main session | |

About 12 opus runs, 20 sonnet and 6 haiku. For scale only: on 2026-11-15 an
opus agent on engine work used between 250,000 and 630,000 tokens.

The owner decides four things along the way: to start phase A; to approve the
specification (end of B); the freeze and the second clone (start of C); and
the two **[ask]** checks in H.

# Part 1: what is decided

## 1. What the surveys found

| Fact | Number |
|---|---|
| Engine source (`tools/recipe/src`) | 38,272 lines |
| Catalog recipes | 72 files, 41,049 lines |
| OS-building recipes (`distro/builder/os-recipes`) | 34 files plus 3 shared modules and 7 lock modules |
| Recipes embedded in Recipe's own tests | about 150, in 26 test files (9,884 lines) |
| Repositories that need a commit | 10 |

How the recipes convert:

| Shape | Catalog | OS-building |
|---|---|---|
| a. `FILES` only, no function | 16 | 1 |
| b. `FILES` with a whole folder | 4 | 0 |
| c. package group (`PACKAGES` + `configure`) | 43 | 0 |
| d. small `build` function | 3 | 1 |
| e. large `build` function | 1 | 0 |
| f. needs something the note does not offer yet | 5 | 32 |

The catalog fits the idea. The OS-building recipes mostly do not, as the note
stands: 32 of 34 need things it never mentions. That is the main finding, and
it is why phase B exists.

What the note does not cover yet:

1. Inputs the caller passes in (`NEEDS.inputs`): 25 of 34 OS recipes.
2. Choices the caller passes in (defines such as `KERNEL_TARGET`, `INIT`).
3. Templates (`extends`, used by 10 OS recipes) and shared modules (`import`,
   used by 18).
4. What `configure(tree)` receives besides the tree: the package format, the
   C library, the OS facts (name, colours).
5. A second, non-package source beside `PACKAGES` (`steam`, `xlibre`, `lib32`).
6. A source chosen by C library or target (`rec-apps`, `bun-toolchain`).
7. Git sources (`xlibre` in the OS set).
8. Sources only known at run time (`prebuilt-fetch`, `release-medium`).
9. A recipe that is never "done" (`root-effects`).
10. An output name other recipes read (`tools`, `sdk`, `rootfs`).
11. Writing into a tree the caller owns (`linux`, `linux-prebuilt`).
12. `cleanup(reason)`, used by 7 OS recipes.
13. Which commands remain: `isinstalled`, `isbuilt`, `isacquired`, the
    `--force-*` options.
14. The look code shared by five groups, and the OS facts block copied into
    seven.
15. The name and form of the pins file.

## 2. Disk space: requirements (the owner, 2026-11-16)

The owner's model of a recipe: download, build, install, and then nothing is
left but the installed program. Recipe must respect the user's disk. A survey
of the code on 2026-11-16 found that it does not:

| Found | Detail |
|---|---|
| Nothing is removed after a successful install | Each step saves the whole work folder (downloads, unpacked source, build tree) and the result as one archive. It stays. |
| The same bytes are stored 4 to 8 times | Two saved-state archives (current and previous, identical when measured), the published result, and a real copy in the package store |
| Trimming is weak | Two archives for each of up to four variants per recipe, trimmed only when that recipe runs again; no total limit, no age rule; up to 2 GB per archive by default |
| Earlier versions for "go back" are never removed | Every install, update and remove adds one; no limit |
| `recipe remove` frees nothing | until the earlier versions are cleared too |
| It is hidden | `recipe storage` and the app's Storage page count only the package store. On the survey machine about 2.2 GB (923 MB saved state, 1.3 GB cached compiler image) was held and not reported, with no program installed for the user. |
| The cleanup step does not help | `cleanup(reason)` is optional per recipe; no catalog recipe defines one, and it cannot delete the engine's saved state |

Requirements:

1. **After a successful install, Recipe itself deletes the downloads, the
   source and the build material**, for every recipe. Only the installed
   files and the receipt remain. This is the engine's job, not a function a
   recipe may forget to write.
2. **Keeping them is a choice the user makes**, in the app and on the command
   line, for people who rebuild often. It is off by default.
3. **Files are moved into place, never copied**, wherever source and
   destination are on the same disk. No stage keeps a second copy of what the
   next stage already holds.
4. **Saved state exists only while a run is unfinished.** It lets an
   interrupted build continue. It is deleted when the install succeeds, and
   one copy is kept at most, never two identical ones.
5. **Nothing is hidden.** `recipe storage` and the Storage page report
   everything Recipe holds, by kind: installed programs, earlier versions,
   kept build material, unfinished runs, the compiler image cache, logs. Each
   kind has a way to remove it.
6. **Removing a program frees its space.**
7. **"Go back" is removed entirely** (the owner, 2026-11-16: "I never asked
   for that feature"). No earlier versions of a program are kept. Gone with
   it: the command `recipe go-back`, the kept generations of the package
   store, `recipe storage --clear-earlier`, the "Go back" button and the
   "earlier versions" numbers in the app. A program is installed or it is not;
   updating replaces it. The feature was added on 2026-11-10 for the app and
   has never run on an installed system.

8. **"Update all" and "Keep at this version" are removed** (the owner,
   2026-11-16: "that should never have existed"). An update is something the
   user asks for, one program at a time. Gone: the "Update all" button, the
   commands `recipe hold` and `recipe unhold`, the "kept" mark on installed
   programs and in `recipe updates`. `hold` existed only to exempt a program
   from "Update all". Updating one program (`recipe update NAME`, the
   "Update" button) stays: it installs the version the fetched recipe names.
   Recipe never writes or changes a recipe to do this.

9. **Recipe never asks an AI for anything** (the owner, 2026-11-16: "all
   the features that would ask an AI to do something have to be removed").
   Everything that can call a language model goes, in the engine and in what
   calls it. Surveyed the same day:

   | Where | What goes |
   |---|---|
   | Engine | `src/llm/` and `src/core/autofix/` (about 1,300 lines), the options `--autofix`, `--autofix-attempts`, `--autofix-cwd`, `--autofix-prompt-file`, `--autofix-allow-path`, `--llm-profile`, the settings file `recipe/llm.toml`, the test `tests/e2e/managed_autofix.rs`, and the mentions in `lifecycle.rs`, `recipes.rs`, `backend.rs`, `lib.rs`, `core/mod.rs`, `bin/recipe*.rs` |
   | Builder | the kernel compile's autofix choice (`pipeline/kernel.rs`, `recipe/mod.rs`, `recipe/stamp.rs`) |
   | xtask | the option that passes it (`cli.rs`, `tasks/kernels/mod.rs`) |
   | Docs | the autofix section of the README, `docs/runtime-limits.md`, `CHANGELOG.md` |

   No helper lets a recipe itself call an AI, and the desktop app has no such
   feature (its "Copy for agent" button only copies text to the clipboard,
   which `docs/product-handoff.md` §2.8 allows). When something fails, Recipe
   reports the failure. What the user does with it, including asking an agent
   of their own in their own window, is theirs.

The cost, accepted by the owner's model: updating or reinstalling a program
downloads and builds it again.

Requirements 7, 8 and 9 are removals and are done first (phase A). The rest
is built in the engine in phase C4 and shown in the app in phase F.

## 3. Every step by itself (the owner, 2026-11-16)

The user decides how far a recipe goes. Each step can be done alone, and the
app shows it.

| The user wants to | Today | Required |
|---|---|---|
| only download | no command; only `install` does it, as its first step | a command and a button |
| only build (from what is downloaded) | no command | a command and a button |
| only install (what is built) | `install` does all missing steps | stays; it does only what is still missing |
| only uninstall | `recipe remove` | stays, and frees the space |
| only remove what was downloaded | not possible (`recipe cleanup` does nothing for catalog recipes) | a command and a button |
| only remove what was built | not possible | a command and a button |
| only clean up (remove what was downloaded and what was built, in one action) | `recipe cleanup` exists but only calls a recipe's own function, which no catalog recipe has | a step of its own: a command and a button |

Surveyed on 2026-11-16: Recipe has `install`, `update`, `remove`, `cleanup`
and the three checks `isacquired`, `isbuilt`, `isinstalled`. It has no command
that only downloads or only builds, and none that removes downloads or build
material. The app offers Install, Update, Remove, Open, Keep at this version
and Go back; "Fetching" and "Building" appear only as labels of a running
install.

What follows from it:

- A program has a visible state: nothing here, downloaded, built, installed.
  The app's page for a program shows that state and the next possible
  actions.
- This fits the disk rule of section 2. Downloads and build material exist
  only between steps: after "only download" the download stays until the user
  builds, installs or removes it. After a successful install they are
  deleted, unless the user chose to keep them.
- The Storage page lists what is downloaded and what is built but not
  installed, each with its size and a way to remove it.
- One click still installs: "Install" does all four steps by itself
  (download, build, install, clean up), skipping what is already done. The
  single steps are optional, for the user who wants to go one at a time.
- One click removes: "Remove" on a program removes the installed program and
  whatever is left of its download and build. The user does not have to
  remove each kind separately; the finer removals are optional too.
- Clean-up is a step like the others: download, build, install, clean up.
  It runs by itself after a successful install (section 2, requirement 1)
  unless the user chose to keep the material, and the user can run it alone
  at any time, for one program or for everything.

The app's page for a program (the owner, 2026-11-16):

- It shows the program's state and two main buttons: "Install" (or "Update")
  and "Remove".
- The single steps are hidden behind a link, "One step at a time". Opened, it
  shows download, build, install and clean up, each with its own button, what
  it holds on disk, and a way to remove just that part.
- A recipe that compiles nothing shows no build step.
- Two different marks, never mixed up: "Update" (the fetched recipe names a
  newer version; one press installs it) and "Newer upstream" (the recipe does
  not name it yet; nothing to press, see section 4).
- No Updates page: the installed list shows both marks per program.

Built in the engine in phase C4 and in the app in phase F.

## 4. Knowing that a newer version exists (the owner, 2026-11-16)

Surveyed: Recipe has no way to learn that a program has a newer version
upstream, and no hook for it. `recipe updates` runs nothing and fetches
nothing; it compares the installed version with the version written in the
recipe file as last fetched. So "update available" today means only that
somebody edited the recipe. The helpers `github_latest_release` and
`github_latest_tag` exist; no recipe calls them.

Decided:

- **Each recipe says where its newest version can be found.** A declaration
  for the common case, such as `const UPSTREAM = #{ github: "OWNER/REPO" };`,
  and an optional function `latest()` that returns the newest version number
  for programs with an unusual release page.
- **Recipe only tells.** When the user asks ("Check for updates"), Recipe
  asks upstream and shows "upstream has 15.3.0, this recipe installs 15.2.0".
  It never checks by itself, never changes a recipe, and never installs a
  version the recipe does not name. A recipe keeps pinning one version and
  its checksum.
- **Changing the recipe is the user's.** A newer version upstream means the
  recipe has to be updated, by hand or by the user's own agent in the user's
  own window. Recipe does not ask an AI (section 2, requirement 9); the app
  may offer the facts to copy.
- An update that can be installed still means what it means today: the
  recipe, as fetched, names a newer version than the one installed.

Built in phase C5; each recipe gets its `UPSTREAM` line when it is converted
(phase E); shown in the app in phase F.

# Part 2: the work

## Phase A: removals

Small, low risk, and they shrink what the rewrite has to carry. Each is its
own commit in the shared checkout. Check: the workspace compiles and lints.

| Step | What goes | Where | Model |
|---|---|---|---|
| A1 | Everything that asks an AI (section 2, requirement 9): `src/llm/`, `src/core/autofix/`, the `--autofix*` and `--llm-profile` options, `recipe/llm.toml`, `tests/e2e/managed_autofix.rs`, the mentions in the engine; the builder's kernel autofix choice; xtask's option for it; the README section | `tools/recipe`, `distro/builder`, `xtask` | sonnet |
| A2 | "Go back", "Keep at this version" and the kept generations (requirements 7, 8): the commands `go-back`, `hold`, `unhold`, `storage --clear-earlier`; the package store keeps exactly one generation, and the one it replaces is deleted once the new one is active; `recipe remove` frees the space; `recipe updates` and `installed` lose `held` and the kept versions | `tools/recipe` (`packages.rs`, `packages/history.rs`, `bin/recipe/{cli,commands}.rs`, man page, README) | opus |
| A3 | The same in the app: the "Go back", "Update all", "Keep at this version" and "Clear earlier versions" actions, the receipt page's go-back, the earlier-versions numbers; the stand-in and the story follow | `tools/gui/recipe` | sonnet |

Why opus for A2: the package store switches generations atomically. Deleting
the old one at the wrong moment loses an installed program without any error.

## Phase B: the specification

**Who:** one opus agent drafts; the main session reviews; the owner approves.
**Output:** `docs/recipe-format.md`: every declaration, every function and its
arguments, every constant, the pins file, the commands and the states of a
program, what `recipe list`, `installed`, `updates`, `storage` and a finished
run return as JSON.
**Why opus:** each answer must hold for what the engine and the 34
OS-building recipes actually do; a wrong one is copied into 106 recipes.

The draft starts from these proposed answers to the 15 gaps of section 1,
plus the three that sections 2 to 4 add:

| # | Proposed answer |
|---|---|
| 1 | `NEEDS.inputs` stays. Functions read an input as `INPUTS.name`. |
| 2 | Defines stay, read as `DEFINES.NAME`, with defaults declared in the recipe. |
| 3 | `extends` and `import` both stay. Templates become a base recipe whose functions the child completes. |
| 4 | `configure(tree)` plus constants: `PACKAGE_FORMAT` (`rpm`/`apk`), `HOST_LIBC`, and `OS` (the facts). |
| 5 | `SOURCES` and `PACKAGES` may both be present; `configure(tree, src)`. |
| 6 | A source may carry `libc:` or `target:`; only the matching one is fetched. |
| 7 | A source may be `git:` with a pinned `commit:`. |
| 8 | `download` and its relatives stay usable inside `build` for recipes that declare no `SOURCES`. |
| 9 | A declaration `RERUN = true`: the recipe runs every time it is asked for. |
| 10 | The single output keeps its name in `NEEDS.outputs`; the recipe body never uses the name. |
| 11 | Unchanged: the caller's tree comes in as the `target` input, as today. |
| 12 | `cleanup(reason)` stays, without `ctx`. |
| 13 | To be settled with section 3: the checks `isinstalled`, `isbuilt`, `isacquired` report a program's state; the `--force-*` options are replaced by the single-step commands. |
| 14 | One shared module `look.rhai`; one generated `os-facts.rhai` module instead of seven copied blocks. |
| 15 | `NAME.pins`: one line per package, written only by the generator, staged with the recipe so it counts toward its identity. |
| 16 | Disk space (section 2): the option that keeps build material, and the kinds `recipe storage --json` reports. No earlier versions: `go-back` is removed. |
| 17 | Every step by itself (section 3): the commands for download only, build only, remove the download, remove the build; the states a program can be in and how `recipe list`/`installed` report them. |
| 18 | Newer versions upstream (section 4): `UPSTREAM`, `latest()`, and what `recipe updates --json` reports. Recipe only tells. |

Also to be stated in the specification:

- The commands for single steps (section 3): proposed `recipe download NAME`,
  `recipe build NAME`, `recipe install NAME`, `recipe remove NAME`,
  `recipe cleanup NAME` (with `--downloads` or `--build` for one kind, and
  without a name for everything not installed).
- The option that keeps the download and build after installing: proposed
  `--keep`, off by default, remembered per program.
- The kinds `recipe storage --json` reports: installed, downloaded, built,
  kept by choice, unfinished runs, build image, logs; each with bytes and
  whether it can be removed.
- What stays of `recipe history` (the log of changes; it costs no space
  worth naming).

The owner sees the specification with the decisions that are theirs marked.
Nothing after phase B starts before it is approved.

## Phase C: the engine (`tools/recipe`)

Opus agents, one after the other in the same working copy, because the parts
depend on each other. Each ends with the crate compiling and lint-clean, and
with the reference recipes of its part passing `recipe info`.

| Step | What | Where (from the survey) | Risk | Model |
|---|---|---|---|---|
| C1 | Read the declarations (`PACKAGE`, `SOURCES`, `FILES`, `PACKAGES`, `ACCOUNTS`, `UPSTREAM`, `RERUN`); one API number; `list`, `info`, `index`, `validate` report the new shape | `needs.rs`, `catalog.rs`, `core/executor.rs`, `core/invocation.rs` (`validate_api`), `sources.rs`, `execution.rs` | medium | opus |
| C2 | The core without `ctx`: constants in place of `RUNTIME`; calling `build(src, out)`, `configure`, `verify`, `cleanup(reason)`; outputs mapped by the engine; `save`/`saved`/`checkpoint` | `core/invocation.rs`, `core/executor/private/*`, `worker/run.rs`, `worker/protocol.rs`, `launch.rs`, `host.rs`, `publication.rs`, `backend.rs`, `lifecycle.rs` | **highest** | opus |
| C3 | Built-in steps: fetch and unpack `SOURCES`; copy `FILES`; package groups (pins file, native RPM/APK unpack, accounts, finish the tree) | new step in `flow.rs`; reuses `helpers/acquire`, `helpers/build/extract.rs`, `helpers/target.rs`, `accounts.rs`, `offline_cache.rs`, `inputs/discover.rs` | medium | opus |
| C4a | Recipe decides what is done from its own records; the states of a program (nothing, downloaded, built, installed); the single-step commands `download`, `build`, `install`, `remove`, `cleanup`; the `remove` hook and `Operation::Remove` gone | `retained.rs`, `host/snapshot.rs`, `core/executor/private/flow.rs`, `worker/protocol.rs`, `bin/recipe/{cli,commands}.rs`, `core/output/narration.rs` | high | opus |
| C4b | Disk (section 2): after a successful install the engine deletes downloads, source, build material and saved state; the keep option; saved state only while a run is unfinished and never twice; moves instead of copies from download to installed file; `recipe storage` reports every kind with a way to remove it; the same for system installs (`system-producer`) | `retained.rs`, `publication.rs`, `packages.rs`, `temporary.rs`, `invocation_log.rs`, `inputs/image_cache.rs`, `bin/recipe/system_route.rs`, `bin/recipe/commands.rs` | high | opus |
| C5 | Newer versions upstream (section 4): the check behind `UPSTREAM` and `latest()`, run only on request; `recipe updates --json` tells "installable update" from "newer upstream" | `helpers/acquire/http.rs`, `bin/recipe/commands.rs`, `updates.rs` | low | sonnet |
| C6 | Independent review of the whole engine change against the specification, by an agent that has not seen the work. It looks for what compiles but is wrong: outputs lost, a result taken as done when it is not, material deleted that an unfinished run needs, the worker and the coordinator disagreeing. Fixes go back to the agent that wrote the part | all of the above | | opus |

With C1 to C3 the agents also convert the **reference recipes**, one per
shape: `ripgrep` (a), `cmake` (b), `network` (c), `pi` (d), `desktop-runtime`
(e), `steam` (a group with a second source), and from the OS set
`busybox-static`, `locked-preseed` with `fedora-rpm-preseed` (a template),
`linux-deps`, `install-tools` (two outputs, eight inputs, three defines).

## Phase D: the pin generator and the builder

Can start when the specification is approved; needs C1 only for the JSON.

| Step | What | Model |
|---|---|---|
| D1 | `xtask/src/tasks/groups.rs` (about 270 lines in 8 functions), `base.rs` `group_seeds`, `kernels/publish.rs`: read `PACKAGES`, write `NAME.pins` and `os-facts.rhai`, no longer rewrite recipe text. Also `recipes/generators/desktop-runtime-pins.py` | sonnet |
| D2 | Builder Rust: `recipe/outputs.rs` and `stamp.rs` (read the new result), `default_set.rs` (`deps_of`, `pins_of`, pins files onto the medium), `update_release.rs` (pins files into the base and `recipes.tar.zst`), `pipeline/kernel.rs` (the `isinstalled` call), the `--force-*` options it passes | sonnet |

## Phase E: the recipes (106)

All against the reference recipes and the specification. Check per recipe:
`recipe info`. Each catalog recipe gets its `UPSTREAM` line as it is converted
(for the many that come from GitHub releases it follows from the source URL).

| Step | Recipes | How | Model |
|---|---|---|---|
| E1 | 12 plain downloads: bat, delta, codex, claude-code, opencode, muse, fzf, micro, fd, jq, rclone, uv | by hand, one batch | haiku |
| E2 | zoxide, eza, ninja, btop, gh | by hand; gh and btop fold file lists into folders | haiku |
| E3 | Toolchains: python, go, gxx | by hand; the run-the-program checks go, since `verify` only reads | sonnet |
| E4 | 28 groups with no configuration and 7 with a small one | one conversion script moves the pins into `NAME.pins` and drops the copied block; the script is thrown away after | sonnet |
| E5 | printing, firefox, login-screen | by hand | sonnet |
| E6 | The five look groups (konsole, qterminal, pcmanfm-qt, lxqt, plasma) onto the shared `look.rhai` and `os-facts.rhai` | by hand | sonnet |
| E7 | The hard ones: lib32, xlibre, rec-apps, antigravity | by hand | opus |
| E8 | OS set: the six preseeds, bun-toolchain, prebuilt-fetch | by hand | sonnet |
| E9 | OS set: shared modules system-files, installed-system, kernel/metadata. The 7 lock modules keep their form | by hand | sonnet |
| E10 | OS set: package-tree steps (package-effects and both effects recipes, rootfs-producer and both source-rootfs, alpine-apk-base-tree, fedora-rpm-unpack, both apps-build-roots) | by hand | opus |
| E11 | OS set: builds (desktop-apps, reclogin, xlibre, tool-crate, live-overlay, release-medium, root-effects) | by hand | opus |
| E12 | OS set: long builds (tui-binary, linux-prebuilt, linux) | by hand | opus |

## Phase F: the Recipe app (`tools/gui/recipe`)

The app as decided in sections 2 to 4. About 6,800 lines today; three pages
change shape. Check: it compiles. Nothing is started, no pictures are taken.

| Step | What | Model |
|---|---|---|
| F1 | What the window reads and runs: the new `list`, `installed`, `updates` and `storage` JSON; a program's state; the single-step commands; the keep option; the two marks "Update" and "Newer upstream"; steps taken from the declarations instead of `hooks` (`model.*`, `client.*`, `beforewords.*`) | sonnet |
| F2 | The pages: a program's page with one main button and the steps behind "One step at a time"; Installed with sizes and both marks per program, no Updates page; Storage with one total and one line per kind, each removable, and "Clean up everything not installed" (`pages.*`, `storage.*`, `window.*`, kit pieces if needed) | sonnet |
| F3 | The stand-in `fixtures/recipe` and `catalog.json` in the new shapes, the story code, the README | sonnet |

## Phase G: tests, documentation, strays

About 150 recipes embedded in 26 test files, plus 7 example and consumer
recipes. They are rewritten and compiled (`cargo check --tests`); they cannot
be proven without a sandbox run. Tests of things that no longer exist are
deleted. No test is added that is a unit test.

| Step | What | Model |
|---|---|---|
| G1 | `managed_inputs.rs` (33 recipes), `managed_state.rs`, `managed_storage.rs`, `managed_retention.rs`, `managed_target.rs`, `managed_cancellation.rs` | sonnet |
| G2 | `needs.rs` (24), `query.rs` (13), `invocation.rs`, `api2.rs`, `cli.rs`, `cleanup.rs`, `failures.rs` | sonnet |
| G3 | `downloads.rs`, `git_acquisition.rs`, `archives.rs`, `dependencies.rs`, `command_options.rs`, `process.rs`, `worker_entry.rs`, `target_metadata.rs`, the kernel and preseed tests, `library_backend.rs`, `examples/`, `tests/consumers/` | sonnet |
| G4 | `WRITING_RECIPES.md` (nearly all of 560 lines), the three man pages, `HELPERS_AUDIT.md`, the README, `CLAUDE.md`, the in-source doc comments | sonnet |
| G5 | Docs pages (the recipe format page, the helper pages, the FAQ, the CLI reference: about 450 lines), the website's Recipe page, `docs/package-recipe-format.md`, parts of `recipe-declared-needs.md` and `ginux-recipe-interfaces.md`, the builder's READMEs, `recipes/README.md`, the app's pictures list | sonnet |
| G6 | Strays: the probe recipe in `testing/install-tests/test-scripts/runtime.sh`, `tools/reclogin/dist/recipe/login-screen.rhai` with its `SWITCH.md`; a dated "superseded" line on the historical design notes | haiku |
| G7 | Generated files: `recipes/index.json`, the docs' generated index, the two terminal-app snapshots of the docs page | haiku |

## Phase H: review and landing

| Step | What | Model |
|---|---|---|
| H1 | Sample 20 converted recipes of all shapes against their old text: every file, mode, link, account and service of the old recipe must still be there. Read the engine once more for any step that copies where it can move | sonnet |
| H2 | The whole workspace compiles; every recipe in both sets passes `recipe info`; `recipe index --check`; `cargo xtask gen --check`; nothing left of `ctx`, `RECIPE_API = 2` or `= 3`, `go-back`, `hold`, `autofix` | haiku |
| H3 | **[ask]** `cargo xtask base groups <distro> --check`: about a minute per OS, reads the package repositories. It proves the moved pins still equal the locks | haiku |
| H4 | **[ask]** Recipe's acceptance suite on CI (the run that passed 146 of 146 on 2026-11-15). It is the only proof of the engine short of an image build | a CI run, no model |
| H5 | Landing: commit and push, submodules first (`tools/recipe`, `recipes`, `distro/builder`, `tools/gui`, `docs/content`, `docs/website`, `tools/tui`, `testing/install-tests`, `tools/reclogin`), then the main repository with all pointers in one commit; CI checked on each | the main session |

## Order

```
A1 A2 A3            removals, land by themselves
B                   specification, owner approves
C1 > C2 > C3 > C4a > C4b > C6
 |         |      \> C5
 |         +--> E1..E7 (catalog)   E8 E9 > E10 E11 E12 (OS recipes)   G1 G2 G3 (tests)
 +--> D1 D2 (readers)              F1 > F2 > F3 (app, after C4b's JSON is fixed by the spec)
G4 G5 G6 after C3;  G7, H last
```

## What stays unproven

Even when every step is done, nothing has run. The engine's core is replaced
without a single execution, 150 test recipes are rewritten without being run,
the OS-building recipes are rewritten on top of a builder that has not
produced an image since 2026-11-15, and the app is rebuilt without being
started. Deleting build material is new behaviour in code that guards against
losing work, and a mistake there shows only when a run is interrupted. The
first real proof is H4 if the owner allows it, and after that the owner's
build and a look at the app on a desktop.

# Decisions taken while the owner was away (2026-11-16)

The owner went to sleep with a standing go ("the entire implementation should be finished when I wake up... if it asks anything use the recommended answer"). The orchestrator took these choices in the owner's place. Each can be reversed.

| Asked | Chosen | Why |
|---|---|---|
| Gate 1, the specification (`recipe-format.md`) | approved with the recommended answers: decisions 1-13 and 15-26 of its section 11 as proposed | the owner's standing go; each answer was checked by the drafting agent against the engine and all 106 recipes |
| Decision 14, `cleanup(reason)` | stays, optional, without `ctx` (the plan's answer 12), not removed as the draft proposed | the owner said the plan's proposed answers are the recommended ones |
| Answers 4 and 14 both define the OS facts | only the generated `os-facts.rhai` module, no `OS` constant | the plan described the same facts twice; one home |
| Answer 6, `target:` source field | dropped; only `libc:` | no recipe fits it; nothing is added that nothing uses |
| Answer 10, kernel outputs | `linux` and `linux-prebuilt` get one output, `bundle`; the builder checks kernel and modules inside it | the other two outputs sit inside the bundle at a release-dependent path |
| Gate 2, the second clone and the freeze | yes: phases C to G are done in `/home/vince/Projects/recipe-next/LevitateOS`; the freeze on recipe changes is in force; baselines in `/home/vince/Projects/recipe-next/NOTES.md` | the owner's standing go; other sessions were idle |
| Step A3, the Disk app (Recdirstat) | its "Kept by Recipe" row, its `recipe storage` read, the "Open Recipe…" button and its stand-in were removed with the earlier versions | they existed only to show earlier versions; the storage kinds come back in phase F |
| Step A2, existing package stores | no migration: a store whose receipts still carry `previous` or `held` no longer loads and must be removed and reinstalled | no backwards compatibility; Recipe has no users |
| Order of phase F | The app (F1-F3) started right after the specification was approved, before the engine's JSON existed | The specification fixes the JSON; the app is a separate repository |
| Order of phase G tests | G1-G3 ran after C4b and C5, not right after C3 | Tests compile against the engine; running them while C4 changed it would not compile |
| `desktop-libs` names no packages on purpose | A `PACKAGES` key with an empty list plus a pins section is a shared group the generator fills | Simplest rule; keeps the old generator behaviour |
| `FILES` entry whose source is for the other C library (`rec-apps`) | Skipped together with that source | The engine refused it on every system; typos are still caught |
| `src.packages[i].var` does not compile in Rhai | Renamed `keep_var` | `var` is a Rhai reserved word |
| Review C6: downloads kept apart from build material | A Downloaded generation is kept beside a checkpoint or built result; `--build` and `--downloads` remove only their kind; each program keeps its own downloads (no store shared between programs) | The specification promises the two kinds can be removed separately; a shared store is new work |
| Review C6: a `FILES` folder entry plus a renaming file entry left both names | The renamed file is not also placed at its old name | Section 2.5 "rename one" |
| Review C6: a producer's run says "installed" while `recipe state` says "built" or "none" | Behaviour kept and documented | Decision 23 (installed = outputs published) against section 6.2 (installed = in a receipt) |
| Review C6: shape of `contract` in a run's result | The specification describes what the engine prints | No reader depends on the shorter form |
| A musl build of `claude-code` and `opencode` | Not added; they ship glibc only, as before | Not adding features; now possible with `libc:` sources |

Smaller choices taken by the agents while building (names, edge cases, where the specification was silent) are listed in `recipe-format.md` section 12, "Clarifications from the build".

## Not decided, left for the owner

- During step A1 an agent ran `cargo fmt` over the whole superproject by mistake. About 55 files in `xtask/` and `distro/builder/` in the shared checkout `/home/vince/Projects/LevitateOS` are reformatted, uncommitted, with no change in meaning. Restoring them was refused by the permission check and was not forced. To drop them: `git -C /home/vince/Projects/LevitateOS checkout -- xtask` and `git -C /home/vince/Projects/LevitateOS/distro/builder checkout -- .` (check first that no other session has edits there).
- The terminal apps' docs snapshots (`cargo xtask check tui`) were not regenerated. Blessing them runs every TUI story, about 20 minutes. The docs pages changed, so that check will report differences until someone runs `cargo xtask check tui --bless`. It already failed before this work (a system-setup story and the same snapshots).
- Not checked, without running anything: whether the archives placed as whole folders (`antigravity`, `pi`, `gh`, `btop`, `zoxide`) contain symbolic links. Package activation refuses links. Also whether, inside Ginux, the move of `FILES` into the output becomes a copy, because `/work` and `/out` are separate storage.
- Possible follow-ups, not done: a musl build for `claude-code` and `opencode` (now possible with `libc:` sources); `installed-system.rhai` reading `INPUTS` itself instead of a folder of all inputs; a download store shared between programs.
