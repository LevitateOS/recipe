# The recipe format can be different and better

Status: an idea, written down on 2026-11-16 so it is not lost. The owner
agreed with the direction and said not to build it now. Nothing here exists.
Today's format is the one in [`WRITING_RECIPES.md`](../WRITING_RECIPES.md).

## Why

Measured on 2026-11-15 over the 72 recipes of the catalog (`LevitateOS/recipes`),
41,000 lines in all:

| What the lines are | Lines | Share |
| --- | --- | --- |
| Per-file checksum lists in four recipes (`cmake`, `go`, `python`, `gxx`) | about 19,350 | 47% |
| One block of code copied into 46 group recipes | about 10,000 | 24% |
| Generated package pins and OS facts inside the recipes | about 3,250 | 8% |
| What a recipe actually says | about 8,400 | 21% |

`cmake.rhai` is 8,932 lines, of which 8,821 are one checksum per installed
file. `ripgrep.rhai` is 109 lines, of which about 55 are a routine every
prebuilt recipe repeats. A person is meant to read a recipe before running
it; three quarters of what they would read is not the recipe.

## What causes it

The `ctx` design, not the recipes' authors.

1. A recipe must say how to do each step and also how to tell that the step
   is done: `is_acquired`/`acquire`, `is_built`/`build`,
   `is_installed`/`install`, `remove`. Seven functions. The catalog has 216
   check functions.
2. A check written by hand needs data to check against, so recipes carry a
   checksum for every file. Recipe already records every file's checksum when
   it saves a step and when it writes a receipt. The recipe repeats what the
   engine knows.
3. `ctx` is one map for four different things: the author's facts, the
   engine's paths, the outputs, and saved state. Every function takes it and
   must hand it back. Setting `ctx.outputs` in `install` and forgetting it in
   `is_installed` is a mistake nothing reports.
4. The same routine is written out in every recipe: download and verify,
   unpack, copy the listed files into place.

## The idea: declare what, write only the unusual how

A recipe states facts. Recipe downloads, verifies, unpacks, and decides what
is already done from what it records itself.

### A prebuilt program: no functions

```rhai
const NEEDS = #{ /* as today: what the recipe may use */ };
const ABOUT = #{ /* as today: what a front end shows */ };

const PACKAGE = #{
    name: "ripgrep",
    version: "15.2.0",
    description: "Fast recursive text search with rg",
};

const SOURCES = #{
    release: #{
        url: "https://github.com/BurntSushi/ripgrep/releases/download/15.2.0/ripgrep-15.2.0-x86_64-unknown-linux-musl.tar.gz",
        sha256: "33e15bcf1624b25cdd2a55813a47a2f95dbe126268203e76aa6a585d1e7b149c",
    },
};

// [path in the unpacked source, path in the package, mode (0644 when left out)]
const FILES = [
    ["rg",          "bin/rg", 0o755],
    ["doc/rg.1",    "share/man/man1/rg.1"],
    ["COPYING",     "share/licenses/ripgrep/COPYING"],
    ["LICENSE-MIT", "share/licenses/ripgrep/LICENSE-MIT"],
    ["UNLICENSE",   "share/licenses/ripgrep/UNLICENSE"],
];
```

### A program that is compiled: one function

```rhai
const SOURCES = #{ source: #{ url: "...", sha256: "..." } };
let build_deps = ["gcc", "make"];

// src.source is the unpacked source; out is the package folder.
fn build(src, out) {
    run("./configure", ["--prefix=" + PREFIX], #{ cwd: src.source });
    checkpoint("configured");          // a long build can be resumed from here
    run("make", ["-j", JOBS], #{ cwd: src.source });
    run("make", ["install", "DESTDIR=" + out], #{ cwd: src.source });
}
```

Plain arguments, nothing handed back. What `RUNTIME` and `ctx.runtime` give
today become named constants (`PREFIX`, `JOBS`, `HOST_LIBC`, ...).

### A group of distribution packages (the desktop, the network stack)

```rhai
const PACKAGE = #{ name: "network", description: "NetworkManager, with WiFi" };

// What the group starts from; the pin generator adds what these need and
// writes the pins to network.pins beside this file.
const PACKAGES = #{
    glibc: ["NetworkManager", "NetworkManager-wifi", "wpa_supplicant"],
    musl:  ["networkmanager", "networkmanager-cli", "networkmanager-wifi", "wpa_supplicant"],
};
const ACCOUNTS = #{ musl: ["g plugdev -"] };
let deps = ["desktop-libs"];

// The group's own configuration, written into its tree.
fn configure(tree) {
    enable_service(tree, "NetworkManager");
}
```

Fetching the pinned packages, unpacking them and finishing the tree is the
engine's job. Today that is about 218 identical lines at the end of every
group recipe.

### When a recipe needs its own check

One optional function, for what only the recipe can know:

```rhai
fn verify(out) {
    let version = output_of(out + "/bin/cmake", ["--version"]);
    if !version.starts_with("cmake version 4.4.5") { throw "unexpected version: " + version; }
}
```

## How Recipe knows what is done

It already has what it needs.

- **Acquired:** a source is named by its SHA-256. It is there or it is not.
- **Built:** a build has an identity: the recipe's text, its sources'
  checksums, its dependencies' identities, the target. Recipe already computes
  this to match kept state. A kept result with the same identity, whose files
  still match the checksums Recipe recorded for them, is the built result.
- **Installed:** the receipt, which lists every file and its checksum.
- **Removed:** Recipe removes what the receipt lists. No recipe code runs.
  (This is already how `recipe remove NAME` works for packages.)

So the check functions, the per-file checksum lists and `remove` have nothing
left to do.

## What `ctx` was for: keeping state

`ctx` was designed to carry a recipe's state from one run to the next: whether
it is installed, where its downloads and build material are. That need is real
and this idea keeps it. What changes is who holds the state.

In today's engine most of it has already moved out of `ctx`:

| State | Where it is kept today | In this form |
| --- | --- | --- |
| Is it installed, which version, which files | Recipe's receipt, not `ctx` | the same |
| Where downloads and build material are | fixed folders Recipe gives the run (`RUNTIME.paths.build`, the download cache); `ctx` only repeats the paths, and they are worked out again on every run | Recipe's, and the recipe no longer repeats them |
| How far a build got | Recipe's checkpoint: a snapshot of the files, with `ctx.state` beside it | `checkpoint("name")` |
| A value the recipe learned while running (a checksum read from a release listing, say) | `ctx.state`, the only part of `ctx` that is saved | still needed: see below |

Only `ctx.state` is saved between runs today. The rest of `ctx` is built again
from the recipe's text each time, and `WRITING_RECIPES.md` says saved state
alone never proves that work is done: the files are checked. On 2026-11-16
none of the 72 catalog recipes used `ctx.state`; three of the 34 OS-building
recipes did, each to remember a checksum it had looked up.

So a recipe must still be able to save values of its own. Instead of a map
threaded through every function:

```rhai
fn build(src, out) {
    if saved("archive_sha") == () {
        save("archive_sha", lookup_digest(src.listing, "tool.tar.gz"));
    }
    verify_sha256(src.archive, saved("archive_sha"));
}
```

`save` and `saved` keep what `ctx.state` keeps, under the same rule: saved
with the files' snapshot, and never taken as proof by itself.

## What goes, what stays

| Today | In this form |
| --- | --- |
| `let ctx = #{...}` and `ctx` through every function | `PACKAGE`, `SOURCES`, constants; functions take what they use |
| `is_acquired`, `is_built`, `is_installed` | gone; optional `verify(out)` |
| `acquire` | gone; `SOURCES` (first designed in LevitateOS `docs/recipe-front-end-format.md`, 2026-11-10, never built) |
| `build`, `install` | one optional `build(src, out)`; `FILES` when nothing is compiled |
| `remove` | gone |
| `ctx.state` | `checkpoint("name")` for progress; `save`/`saved` for values the recipe learned |
| `ctx.outputs[...] = ...` in two places | the output is `out`; its kind is in `NEEDS.outputs` as today |
| Generated pin blocks inside recipes | a pins file beside the recipe, written only by the generator |
| The copied group code | in the engine |
| `NEEDS`, `ABOUT`, `deps`, `build_deps`, `extends` | unchanged |

Rough size after it, by estimate: a prebuilt program about 40 lines, a group
about 40, the whole catalog about 4,000 instead of 41,000.

## What it would take

- Engine work in Recipe: the declared sources, the built-in steps, the
  identity-based "done", the package-group source, the pins file.
- All catalog recipes rewritten (mostly mechanical).
- The OS-building recipes (`distro/builder/os-recipes` in LevitateOS, 34 files,
  about 4,400 lines) use `ctx` too. They are more procedural and shrink less;
  each keeps a `build` function. Moving them as well leaves one format.
- The desktop app reads a recipe's steps from its functions today
  (`hooks` in `recipe list --json`); it would read them from the declarations.
- `WRITING_RECIPES.md`, the man pages and the helper catalog.

## Open questions

1. One output folder (`out`) or several named outputs as today
   (`program`, `package`, `system`, `sdk`): how `FILES` and `build` name them.
2. Settled by the owner on 2026-11-16: everything moves at once. Recipe has
   never been released and has no users, so there is no backwards
   compatibility: no old form beside the new one and no `RECIPE_API` number
   to tell them apart.
3. How a recipe with several sources names which one `FILES` reads from.
4. What `verify` may do: read the output only, or also run its programs
   (which decides host or Ginux, see "Host or Ginux" in `WRITING_RECIPES.md`).
5. Whether text that is the same in many recipes' `configure` (the look
   functions in `plasma`, `konsole`, `lxqt`) becomes helpers or a shared file.
