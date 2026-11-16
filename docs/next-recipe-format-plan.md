# Implementation plan: the next recipe format

Status: a plan, 2026-11-16. Nothing in it has been started. It follows
[`next-recipe-format.md`](next-recipe-format.md) (the idea and the owner's
decisions). It was written from three read-only surveys of the code made the
same day: the engine, all recipes, and everything else that reads recipes.

Rules that hold for every step:

- The main session plans, briefs, reviews and commits. It changes no code.
  Every change is made by an agent on the model named here.
- No backwards compatibility. One format, `RECIPE_API = 1`. The old form is
  deleted in the same step.
- Nothing is run to prove the work: no installs, no builds, no test suites.
  Allowed checks are compiling, linting and `recipe info FILE` (seconds).
  Two exceptions need the owner's yes and are marked **[ask]**.
- The work happens in a second copy of the repositories, not in the shared
  checkout, and reaches the real repositories only at the end (step 9).

## 0. What the surveys found

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
it is why step 1 exists.

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

## 0b. Disk space: requirements (the owner, 2026-11-16)

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
   has never run on an installed system. "Keep at this version" (`hold`) is a
   separate feature, keeps no copies, and is not decided here.

The cost, accepted by the owner's model: updating or reinstalling a program
downloads and builds it again.

Where this lands in the steps below:

- Step 1 (the specification) states these rules, the option's name, and what
  `recipe storage --json` reports.
- Step 2d (the engine decides what is done from its own records) is what
  makes requirement 1 possible: a finished install no longer needs the build
  material as proof. Step 2d also does requirements 1, 3, 4, 6 and 7.
- Step 3c (the Recipe app) gains the Storage page of requirement 5 and the
  "keep build material" option of requirement 2.
- Step 8a's review checks, by reading, that no step copies where it can move.

## 0c. Every step by itself (the owner, 2026-11-16)

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
- This fits the disk rule of section 0b. Downloads and build material exist
  only between steps: after "only download" the download stays until the user
  builds, installs or removes it. After a successful install they are
  deleted, unless the user chose to keep them.
- The Storage page lists what is downloaded and what is built but not
  installed, each with its size and a way to remove it.
- One click still installs: "Install" does every step that is missing.

Where this lands: step 1 names the commands and the states; step 2d builds
them into the engine (it owns what is done and what is kept); step 3c builds
the buttons, the state and the Storage rows into the app.

## 1. Finish the format: the specification

**Who:** one opus agent drafts; the main session reviews; the owner approves.
**Output:** `docs/recipe-format.md`: the complete format, every declaration,
every function and its arguments, every constant, the pins file, the JSON
that `recipe list` and a finished run return, and the commands that remain.
**Why opus:** each answer must be checked against what the engine and the 34
OS recipes actually do; a wrong answer here is copied into 106 recipes.

The draft starts from these proposed answers (numbers as in section 0):

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
| 13 | To be settled with section 0c: the checks `isinstalled`, `isbuilt`, `isacquired` report a program's state; the `--force-*` options are replaced by the single-step commands. |
| 14 | One shared module `look.rhai`; one generated `os-facts.rhai` module instead of seven copied blocks. |
| 15 | `NAME.pins`: one line per package, written only by the generator, staged with the recipe so it counts toward its identity. |
| 16 | Disk space (section 0b): the option that keeps build material, and the kinds `recipe storage --json` reports. No earlier versions: `go-back` is removed. |
| 17 | Every step by itself (section 0c): the commands for download only, build only, remove the download, remove the build; the states a program can be in and how `recipe list`/`installed` report them. |

The owner sees the spec with the decisions that are theirs marked, before
step 2 starts.

## 2. The engine (`tools/recipe`)

Four opus agents, one after the other in the same working copy, because the
parts depend on each other. Each ends with the crate compiling and lint-clean,
and with the reference recipes of its part passing `recipe info`.

| Step | What | Where (from the survey) | Risk |
|---|---|---|---|
| 2a | Read the declarations (`PACKAGE`, `SOURCES`, `FILES`, `PACKAGES`, `ACCOUNTS`, `RERUN`); one API number; `list`, `info`, `index`, `validate` report the new shape | `needs.rs`, `catalog.rs`, `core/executor.rs` (`declared_metadata`), `core/invocation.rs` (`validate_api`), `sources.rs`, `execution.rs` | medium |
| 2b | The core: no `ctx`. Constants in place of `RUNTIME`; call `build(src, out)`, `configure`, `verify`, `cleanup(reason)`; outputs mapped by the engine; `save`/`saved`/`checkpoint` | `core/invocation.rs`, `core/executor/private/{flow,actions,loaded,context}.rs`, `worker/run.rs`, `worker/protocol.rs`, `launch.rs`, `host.rs`, `publication.rs`, `backend.rs`, `lifecycle.rs` | **highest** |
| 2c | Built-in steps: fetch and unpack `SOURCES`; copy `FILES`; package groups (pins file, native RPM/APK unpack, accounts, finish the tree) | new step in `flow.rs`; reuses `helpers/acquire`, `helpers/build/extract.rs`, `helpers/target.rs`, `accounts.rs`, `offline_cache.rs`, `inputs/discover.rs` | medium |
| 2d | "Done" from Recipe's own records; delete build material and saved state after a successful install, move instead of copy, `go-back` and kept generations removed, `recipe storage` reporting everything (section 0b); each step as its own command and the states of a program (section 0c); `remove` hook and `Operation::Remove` gone; autofix and narration renamed | `retained.rs`, `host/snapshot.rs`, `packages.rs`, `system.rs`, `core/autofix/*`, `core/output/narration.rs`, `bin/recipe/commands.rs` | high |

With 2a–2c the agents also convert the **reference recipes**, one per shape:
`ripgrep` (a), `cmake` (b), `network` (c), `pi` (d), `desktop-runtime` (e),
`steam` (group with a second source), and from the OS set `busybox-static`,
`locked-preseed` with `fedora-rpm-preseed` (template), `linux-deps`,
`install-tools` (two outputs, eight inputs, three defines).

| Step | What | Model |
|---|---|---|
| 2e | Independent review of the whole engine change against the spec, by an agent that has not seen the work. It looks for what compiles but is wrong: outputs lost, a result taken as done when it is not, the worker and the coordinator disagreeing | opus |
| 2f | Fixes from the review | the agent that wrote the part (resumed) |

## 3. The pin generator and the other text readers

Can start when the spec is approved; needs 2a only for the JSON shape.

| Step | What | Model |
|---|---|---|
| 3a | `xtask/src/tasks/groups.rs` (about 270 lines in 8 functions), `base.rs` `group_seeds`, `kernels/publish.rs`: read `PACKAGES`, write `NAME.pins` and `os-facts.rhai`, no longer rewrite recipe text. Also `recipes/generators/desktop-runtime-pins.py` | sonnet |
| 3b | Builder Rust: `recipe/outputs.rs` and `stamp.rs` (read the new result), `default_set.rs` (`deps_of`, `pins_of`, copy pins files to the medium), `update_release.rs` (pins files into the base and `recipes.tar.zst`), `pipeline/kernel.rs` (the `isinstalled` call) | sonnet |
| 3c | Recipe app: steps from the declarations instead of `hooks`; the Storage page shows everything Recipe holds, by kind, with a way to remove each; the option to keep build material; "Go back" removed; a program's state and a button for each single step (section 0c); the stand-in `fixtures/recipe` and `catalog.json`; README | sonnet |

## 4. The catalog recipes (72)

All against the reference recipes and the spec. Check per recipe:
`recipe info`.

| Step | Recipes | How | Model |
|---|---|---|---|
| 4a | 12 plain downloads: bat, delta, codex, claude-code, opencode, muse, fzf, micro, fd, jq, rclone, uv | by hand, one batch | haiku |
| 4b | zoxide, eza, ninja, btop, gh (and ripgrep is the reference) | by hand; gh and btop fold file lists into folders | haiku |
| 4c | Toolchains: python, go, gxx (cmake and pi are references) | by hand; the run-the-program checks go, since `verify` only reads | sonnet |
| 4d | 28 groups with no configuration and 7 with a small one | one conversion script moves the pins into `NAME.pins` and drops the copied block; the script is thrown away after | sonnet |
| 4e | printing, firefox, login-screen | by hand | sonnet |
| 4f | The five look groups: konsole, qterminal, pcmanfm-qt, lxqt, plasma, onto the shared `look.rhai` and `os-facts.rhai` | by hand | sonnet |
| 4g | The hard five: lib32, xlibre, rec-apps, antigravity (steam and desktop-runtime are references) | by hand | opus |

## 5. The OS-building recipes (34) and their modules

| Step | Recipes | Model |
|---|---|---|
| 5a | The six preseeds, bun-toolchain, prebuilt-fetch (locked-preseed and busybox-static are references) | sonnet |
| 5b | Shared modules: system-files, installed-system, kernel/metadata. The 7 lock modules keep their form | sonnet |
| 5c | Package-tree steps: package-effects and both effects recipes, rootfs-producer and both source-rootfs, alpine-apk-base-tree, fedora-rpm-unpack, both apps-build-roots | opus |
| 5d | Builds: desktop-apps, reclogin, xlibre, tool-crate, live-overlay, release-medium, root-effects (install-tools is a reference) | opus |
| 5e | Long builds: tui-binary, linux-prebuilt, linux (linux-deps is a reference) | opus |

## 6. Recipe's own tests and examples

About 150 embedded recipes in 26 files, plus 7 example and consumer recipes.
They are rewritten and compiled (`cargo check --tests`); they cannot be
proven without a sandbox run. Tests of things that no longer exist (the API
split, the removed commands) are deleted.

| Step | Files | Model |
|---|---|---|
| 6a | `managed_inputs.rs` (33 recipes), `managed_state.rs`, `managed_storage.rs`, `managed_retention.rs`, `managed_target.rs`, `managed_cancellation.rs`, `managed_autofix.rs` | sonnet |
| 6b | `needs.rs` (24), `query.rs` (13), `invocation.rs`, `api2.rs`, `cli.rs`, `cleanup.rs`, `failures.rs` | sonnet |
| 6c | `downloads.rs`, `git_acquisition.rs`, `archives.rs`, `dependencies.rs`, `command_options.rs`, `process.rs`, `worker_entry.rs`, `target_metadata.rs`, kernel and preseed tests, `library_backend.rs`, `examples/`, `tests/consumers/` | sonnet |

## 7. Documentation and the strays

| Step | What | Model |
|---|---|---|
| 7a | `WRITING_RECIPES.md` (nearly all of 560 lines), the three man pages, `HELPERS_AUDIT.md`, the README sections, `CLAUDE.md`, the in-source doc comments | sonnet |
| 7b | Docs pages (`02-recipe-format`, the helper pages, the FAQ, the CLI reference: about 450 lines), the website's Recipe page, `docs/package-recipe-format.md`, parts of `recipe-declared-needs.md` and `ginux-recipe-interfaces.md`, the builder's two READMEs, `recipes/README.md` | sonnet |
| 7c | Strays: the probe recipe in `testing/install-tests/test-scripts/runtime.sh`, `tools/reclogin/dist/recipe/login-screen.rhai` with its `SWITCH.md`; a dated "superseded" line on the historical design notes | haiku |
| 7d | Generated files: `recipes/index.json` (`recipe index`), the docs' generated index, the two terminal-app snapshots of the docs page | haiku |

## 8. Review before landing

| Step | What | Model |
|---|---|---|
| 8a | Sample 20 converted recipes of all shapes against their old text: every file, mode, link, account and service of the old recipe must still be there | sonnet |
| 8b | The whole workspace compiles; every recipe in both sets passes `recipe info`; `recipe index --check`; `cargo xtask gen --check`; no `ctx`, `RECIPE_API = 2` or `= 3` left anywhere | haiku |
| 8c | **[ask]** `cargo xtask base groups <distro> --check`: about a minute per OS, reads the package repositories. It proves the moved pins still equal the locks | haiku |
| 8d | **[ask]** Recipe's acceptance suite on CI (the run that passed 146 of 146 on 2026-11-15). It is the only proof of the engine short of an image build | no model; a CI run |

## 9. Landing

The main session commits and pushes, submodules first, then the main
repository with all pointers in one commit: `tools/recipe`, `recipes`,
`distro/builder`, `tools/gui`, `docs/content`, `docs/website`, `tools/tui`,
`testing/install-tests`, `tools/reclogin`, then LevitateOS itself. CI is
checked on each.

Two things to settle before step 2 starts:

- **Where the work happens.** A second clone of the main repository with its
  submodules. No branch is created; it is pushed from there at the end.
- **A freeze.** While the rewrite runs, no other session changes a recipe or
  an OS-building recipe. Any that does is converted again at landing.

## 10. Order and what can run side by side

```
1  spec ──► 2a ──► 2b ──► 2c ──► 2d ──► 2e ──► 2f
              │              │
              │              ├──► 4a 4b 4c 4d 4e 4f 4g   (catalog, side by side)
              │              ├──► 5a 5b ──► 5c 5d 5e     (OS recipes)
              │              └──► 6a 6b 6c               (tests)
              └──► 3a 3b 3c                              (readers)
                                   7a 7b 7c  after 2c
                                   7d, 8, 9  last
```

## 11. Size

| Model | Agent runs | For |
|---|---|---|
| opus | 10 | the spec, the engine (4), its review, the hard catalog five, the OS recipes (3) |
| sonnet | 14 | readers (3), catalog (4), OS recipes (2), tests (3), docs (2) |
| haiku | 6 | plain catalog recipes (2), strays, generated files, final checks (2) |

For scale only: on 2026-11-15 an opus agent on engine work used between
250,000 and 630,000 tokens. This is larger than everything written that day
for the Recipe-installed desktop.

## 12. What stays unproven

Even when every step is done, nothing has run. The engine's core is replaced
without a single execution, 150 test recipes are rewritten without being
run, and the OS-building recipes are rewritten on top of a builder that has
not produced an image since 2026-11-15 either. The first real proof is step
8d if the owner allows it, and after that the owner's build.
