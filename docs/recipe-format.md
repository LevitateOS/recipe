# The recipe format, API 1

Status: the specification for API 1, approved 2026-11-16 under the owner's
standing go (phase B of [`next-recipe-format-plan.md`](next-recipe-format-plan.md)).
The decisions were taken by the orchestrator with the recommended answers; they
are listed in section 11 and in the plan's section "Decisions taken while the
owner was away", and the owner may reverse any of them. It is the contract:
[`WRITING_RECIPES.md`](../WRITING_RECIPES.md) and the man pages follow it, and
section 12 lists the clarifications taken while building.

It starts from the plan's 18 proposed answers and the owner's decisions in
[`next-recipe-format.md`](next-recipe-format.md). Each answer was checked
against the engine (`src/`), the 72 catalog recipes (`recipes/`), the 34
OS-building recipes with their shared and lock modules
(`distro/builder/os-recipes/`) and the builder code that calls them
(`distro/builder/src/`). Where an answer does not hold for some recipe, or the
plan says nothing, the document makes a decision and marks it:

**Decided (2026-11-16, owner away):** a line like this one is a decision taken while the owner was
away. All of them are collected, numbered, at the end.

## 0. What is decided and not reopened

- `const RECIPE_API = 1;` is the first line of every recipe and every base a
  recipe extends. The engine accepts the number 1 and nothing else.
- No backwards compatibility. API 2 and 3, `ctx`, `RUNTIME` and the hooks are
  deleted, not kept beside the new form. All 106 recipes, the examples, the
  test recipes and every reader change in one step.
- One output by default, never named in the body of a normal recipe.
- `FILES` paths start with the source's name; with one source the name is
  left out. A path ending in `/` is a whole folder.
- `verify` only reads the result. It starts no program.
- The OS-building recipes are rewritten together with the catalog.
- No "go back", no kept earlier versions, no hold or unhold, no "Update all".
  An update is asked for one program at a time.
- Recipe never asks an AI. No declaration, function, helper, command or
  option calls a language model or starts an agent. A failure is reported;
  what the user does with it is theirs.
- Recipe only tells that upstream has a newer version. It never changes a
  recipe and never installs a version the recipe does not name.
- After a successful install Recipe deletes the downloads, the unpacked
  source, the build material and the saved state, unless the user chose to
  keep them. Files are moved into place, not copied, where both places are on
  the same disk.
- Nothing is hidden in storage reporting. Every step can be run alone. One
  action installs and one action removes.

Gone, and named here only so nobody looks for them: `go-back`, `hold`,
`unhold`, `storage --clear-earlier`, kept generations, `--autofix`,
`--autofix-attempts`, `--autofix-cwd`, `--autofix-prompt-file`,
`--autofix-allow-path`, `--llm-profile`, `recipe/llm.toml`, `src/llm`,
`src/core/autofix`.

## 1. A recipe at a glance

A recipe is a Rhai file. It states facts in literal declarations and writes
code only for what is unusual. Recipe downloads, checks, unpacks, places
files, unpacks distribution packages, finishes system trees, records what is
done, installs, removes and cleans up.

| Part | Form | Required |
|---|---|---|
| `RECIPE_API` | `const`, the number 1 | yes |
| `NEEDS` | `const`, literal map | yes, in every recipe and in every child |
| `ABOUT` | `const`, literal map | for catalog recipes |
| `PACKAGE` | `const`, literal map | for catalog recipes |
| `SOURCES` | `const`, literal map | no |
| `FILES` | `const`, literal list | no |
| `PACKAGES` | `const`, literal list or map | no |
| `ACCOUNTS` | `const`, literal map | no |
| `UPSTREAM` | `const`, literal map | no |
| `RERUN` | `const`, `true` | no |
| `deps`, `build_deps` | `let`, literal list of names | no |
| `//! extends: FILE` | first line comment | no |
| `import "FILE" as NAME;` | Rhai import, literal path | no |
| `build(src, out)` | function | no |
| `configure(tree)` or `configure(tree, src)` | function | only with `PACKAGES` |
| `verify(out)` | function | no |
| `latest()` | function | no |
| `cleanup(reason)` | function | no |

"Literal" means what `NEEDS` and `ABOUT` mean today: plain data, no helper
calls, no variables, no arithmetic. Recipe reads every declaration without
running the recipe, so `recipe list`, `info`, `index`, `validate`, the pin
generator and the app can show them.

A recipe may define other functions and constants of its own. The names
above are reserved. A recipe that defines a function named `acquire`,
`install`, `remove`, `is_acquired`, `is_built` or `is_installed`, or declares `ctx` or `RUNTIME`, is
refused (`cleanup` is not refused: it is a recipe function, section 3.7) with the line to change, so a half converted recipe fails at once.

### What a function can see

- Its arguments.
- The engine's constants (section 4).
- The recipe's own top-level constants whose value is literal data, and its
  declarations (`PACKAGE`, `SOURCES`, ...).
- Imported modules, through their alias (`lock::packages()`).

This holds for the functions the engine calls, for every function they call,
for the base of `extends` and for imported modules. A value computed at the
top level (`let x = ...`, or `const X = f(...)`) is not visible inside a
function: compute it inside the function, or in a function of its own.

Found while checking: today a hook sees the top-level scope, but a function a
hook calls does not. `steam.rhai` reads `VALVE_FILES`, `LAUNCHER` and
`NOFILE_CONF` inside `configure`, and the `xlibre` group reads `FILES` inside
`configure`; both are called from `build`. Under today's engine these look
like failures that have not shown because the two recipes never ran. The new
engine must make the rule above true everywhere (a variable resolver or a
global module for the engine's constants; literal constants as today).

### Top-level code

Statements outside functions, other than declarations, run once per run,
before the first function, with the engine's constants set. They run only for
the steps that run recipe code (build, configure, verify, latest), never for
`list`, `info` or `download`. A throw there fails the run. Example from
today: `live-overlay` reads and parses its `config` input at the top level;
converted, it does that inside `build`.

## 2. The declarations

### 2.1 `NEEDS`

As today: what the recipe may use. Ginux and the host confinement enforce it;
callers can only narrow it.

| Field | Meaning |
|---|---|
| `inputs` | name to `"file"`, `"directory"`, `"file?"` or `"directory?"` |
| `defines` | name to its default value (new, see below) |
| `outputs` | name to `"file"` or `"directory"` |
| `network` | `"host:port"` endpoints, redirect hosts included |
| `network_max_bytes` | bytes sent and received, default 8 GiB |
| `memory_mib` | default 2048 |
| `storage_max_objects` | default 16384 |
| `storage_max_bytes` | default bounded by memory |
| `timeout_secs` | default 3600, 1 to 86400 |

`outputs` holds exactly one name in a normal recipe. The name tells Recipe and
the caller what the output is (`package`: a package activated in a profile;
`system`: a tree written into a system root; `tools`: put on the PATH of a
consumer; any other name: a product the caller picks up). The body never uses
the name (section 3.1). Five OS-building recipes declare several outputs
(section 3.2).

Every host of a `SOURCES` URL, and every redirect host, must be in `network`.
`recipe validate` and `recipe info` say which one is missing.

#### Defines (answer 2)

Defines stay: genuine choices a caller makes, such as `KERNEL_TARGET` or
`INIT`. A recipe declares each define it reads, with its default, and reads it
as `DEFINES.NAME`.

```rhai
const NEEDS = #{
    defines: #{ KERNEL_TARGET: "levitate", INIT: () },
    // ...
};
```

**Decided (2026-11-16, owner away):** defines are declared in `NEEDS.defines`, name to default; a default of `()` means
required, and a run without it fails before anything runs. A define given on
the command line reaches every recipe of the graph that declares it (so
`tui-binary` and its dependency `bun-toolchain` can share one); a define no
recipe of the graph declares is refused. Names starting with `RECIPE_` are
the engine's own (`RECIPE_SYSTEM_CONTEXT`, `RECIPE_SYSTEM_PRESENT` today) and
cannot be declared. Why: `NEEDS` is where everything a caller may bind is
declared already, and today `release-medium` reads `CACHE_DIR` and
`DEFAULT_SET_DIR` without a default, so required defines exist.

Who relies on defines: `linux` and `linux-prebuilt` (`KERNEL_TARGET`),
`reclogin` (`OS_ID`, `INIT`), `install-tools` (`BIN`, `INSTALL_PATH`,
`INIT`), `tool-crate` (`BINS`), `prebuilt-fetch` (`BASE_URL`, `KEY`,
`ARCHIVE`), `release-medium` (`CACHE_DIR`, `DEFAULT_SET_DIR`). `xlibre` and
`desktop-apps` read a define `JOBS` that the builder never passes; they read
the engine's `JOBS` instead.

### 2.2 `ABOUT`

Unchanged: what a front end shows, the author's claim, checked only for form
(`kind`, `category`, `libc`, `libc_carried`, `systems`, `details`,
`homepage`, `license`, `adds`, `steps`, `keeps`). `ABOUT.systems`,
`target_extract_apk` and the options form of `target_finalize`, which API 3
added, are plain parts of API 1.

**Decided (2026-11-16, owner away):** the keys of `ABOUT.steps` are the step
names of section 6, `download`, `build`, `install`, instead of the hook names
`acquire`, `build`, `install`. Why: the hooks are gone, and the app shows
these four steps.

### 2.3 `PACKAGE`

```rhai
const PACKAGE = #{
    name: "ripgrep",
    version: "15.2.0",
    description: "Fast recursive text search with rg",
};
```

All three fields are literal strings. `version` is required for a recipe with
a `package` or `system` output, because receipts and updates compare it,
except for a recipe with `PACKAGES`, whose version comes from its pins file
(section 2.7). OS-building recipes may leave `version` out. A missing
`description` is shown as empty.

Seven OS recipes compute their description today (`"Locked Fedora " +
lock::release() + " RPM set"`); they write it out.

**Decided (2026-11-16, owner away):** `PACKAGE.name` equals the file name without `.rhai`;
`name` may be left out and is then the file name. Why: the
receipt, `update NAME` and the sources all find a recipe by its file name, and
the one recipe whose name is computed today, `install-tools` (name from the
`BIN` define), reads `DEFINES.BIN` instead.

### 2.4 `SOURCES`

What Recipe downloads before anything else. A map from a source name to one
source.

```rhai
const SOURCES = #{
    release: #{
        url: "https://github.com/BurntSushi/ripgrep/releases/download/15.2.0/ripgrep-15.2.0-x86_64-unknown-linux-musl.tar.gz",
        sha256: "33e15bcf1624b25cdd2a55813a47a2f95dbe126268203e76aa6a585d1e7b149c",
    },
    licence: #{
        url: "https://raw.githubusercontent.com/jqlang/jq/jq-1.8.2/COPYING",
        sha256: "ad2b4a266b2268939c1446979759706077421cf906a203aa188c6f396e8cfd74",
    },
};
```

| Field | Meaning |
|---|---|
| `url` | HTTPS address. Its host is in `NEEDS.network`. |
| `sha256` | 64 hex digits. Required for `url`. |
| `file` | the saved file's name; default the last part of the URL's path. Needed when the URL ends in a query (`muse`). |
| `unpack` | `false` keeps the download as one file even when it looks like an archive. |
| `format` | `tar.gz`, `tar.xz`, `tar.bz2`, `tar.zst`, `zip` or `rpm`, when the file name does not say it. |
| `strip` | `false` keeps a single top folder (see below). |
| `libc` | `"glibc"` or `"musl"`: only fetched on that C library (answer 6). |
| `git`, `commit` | a Git repository and a full commit id, instead of `url` and `sha256` (answer 7). |
| `tag` | for `git`: informational only. |

How a source arrives:

- The download goes into Recipe's download store, named by its SHA-256. A
  source is downloaded when its file is there and matches. The offline cache
  (`RECIPE_CACHE_DIR`, `/usr/share/recipe/cache`) is used first, as today.
- An archive (by `format`, else by the file name's ending) is unpacked into
  its own folder. An `rpm` source is unpacked natively, without scripts, like
  `target_extract_rpm`. Anything else stays one file.
- A `git` source is checked out at `commit`, with the checks `git_checkout`
  makes today. Its download is the checkout.
- In `build(src, out)`, `src.NAME` is the path of the unpacked folder, of the
  single file, or of the checkout.

Who relies on what: every prebuilt catalog recipe (archives and plain files:
`jq`, `claude-code` and `muse` download single programs; `rclone` and `ninja`
are `zip`), `gxx` (three `rpm` sources unpacked into one tree), `rec-apps`
and the `xlibre` group (`libc`), `steam` (Valve's tarball beside packages),
OS `xlibre` (two `git` sources, the only `git` user).

**Decided (2026-11-16, owner away):** a single top folder is left out when an archive is
unpacked. When every entry of an archive sits under one top
folder, `src.NAME` is that folder; `strip: false` keeps it. Why: the note's
own `ripgrep` example writes `"rg"`, not
`"ripgrep-15.2.0-x86_64-unknown-linux-musl/rg"`, and the same folder name
repeats the version in the paths of `bat`, `fd`, `gh`, `cmake`, `python`,
`steam` (`steam-launcher/`) and `antigravity` (`Antigravity-x64/`). An archive
that holds one file (`codex`) is not changed.

#### Choosing a source by C library or target (answer 6)

`libc` holds: `rec-apps` takes one prebuilt per C library, the `xlibre` group
a musl prebuilt, `steam` a glibc tarball. A source with `libc` is fetched only
when `HOST_LIBC` matches; a recipe whose every source and pin is left out on
this system has no build for it, which `ABOUT.libc` already says.

`target:` in the proposed answer has no meaning yet. Three recipes choose by
something other than the C library, and none fits a source field:

- `linux` and `linux-prebuilt` choose by the `KERNEL_TARGET` define, their
  pins live in `kernel/metadata.rhai`, which the builder also evaluates, and
  they take the optional `kernel_archive` input instead of downloading.
- `bun-toolchain` chooses by the contents of its `runtime_target` input.

**Decided (2026-11-16, owner away):** there is no `target:` field. Only `libc`
exists. The three recipes above declare no `SOURCES` and download inside
`build` (answer 8). Why: each of them needs something a declaration cannot
say (an input that replaces the download, pins a Rust caller reads, a choice
by an input's contents), and no other recipe needs `target`.

### 2.5 `FILES`

Which files of the sources go where in the output. A list of
`[from, to]` or `[from, to, mode]`.

```rhai
const FILES = [
    ["release/rg",          "bin/rg", 0o755],
    ["release/doc/rg.1",    "share/man/man1/rg.1"],
    ["release/COPYING",     "share/licenses/ripgrep/COPYING"],
    ["docs/",               "share/doc/ripgrep/"],
];
```

- `from` starts with the source's name. With exactly one source the name is
  left out: `["rg", "bin/rg", 0o755]`.
- `from` ending in `/` is a whole folder, everything in it; `"NAME/"` is the
  whole source, and with one source `""` is. `to` ending in `/` (or `""`, the
  output's top) is the folder it goes into. Single files can be listed beside
  a folder to rename one or to set a mode.
- `to` is relative to the output: the package folder (`bin/...`,
  `share/...`), the system tree (`usr/...`, `etc/...`), or any other
  directory output.
- A single file without a mode gets 0644. Files of a whole folder keep their
  mode from the source. Setuid and setgid bits are refused in a `package`
  output, as today.
- In a `system` output the mode goes into the tree's target metadata, as
  `target_mode` does today (so the `xlibre` group's setuid `Xorg.wrap` keeps
  working).
- Files are moved from the unpacked source into the output. A source file
  named by two entries is copied for the later one (`steam` puts the 48-pixel
  icon in two places).
- A `from` that does not exist fails the run, naming it.

**Decided (2026-11-16, owner away):** `FILES` names a source that is one plain file, and
a `file` output, like this: a plain-file source is named by its source name
even when it is the only source (there is no path inside it); in a recipe
whose output is a `file`, the entry whose `to` is `""` makes the output
itself. `FILES` is only for recipes with one output. Why: `busybox-static`
(one plain-file source, a `file` output) and the single-program downloads
(`claude-code`, `muse`) would otherwise have no way to write the entry.

**Decided (2026-11-16, owner away):** a recipe may have both `FILES` and `build`;
`FILES` are placed first, then `build(src, out)` runs on the result. Why:
`pi` places a whole folder and writes a launcher, and `antigravity` places a
whole folder, removes `chrome-sandbox`, adds the runtime's data, and then
`carry_libraries` rewrites the placed files, which needs them in place first.

The per-file checksum lists (`FILE_SHA256` in `cmake`, `go`, `python`, `gxx`,
`ninja`; the `sha256` per file in every prebuilt recipe and in `steam`'s
`VALVE_FILES`) are gone: the archive's checksum covers its contents, and
Recipe records every installed file's checksum in the receipt.

### 2.6 `PACKAGES`

The distribution packages a group is made of: the seeds the pin generator
starts from. The generator adds what they need and writes the pins file
(section 2.7). Recipe reads only the pins file; the seeds are for the
generator and for people.

```rhai
const PACKAGES = #{
    glibc: ["NetworkManager", "NetworkManager-wifi", "wpa_supplicant"],
    musl:  ["networkmanager", "networkmanager-cli", "networkmanager-wifi", "wpa_supplicant"],
};
```

- A map from C library to seeds: `glibc` (Fedora RPMs, LevitateOS) and `musl`
  (Alpine APKs, AcornOS). A C library left out, or with an empty list, has no
  build there.
- A plain list instead of a map: the same packages on every system. This is
  `desktop-runtime`, whose Fedora RPM set is pinned whatever the host's C
  library, because it is a build dependency that brings its own glibc.
- `glibc_i686`: 32-bit seeds, glibc only (`lib32`).

What Recipe does with them, in this order, for a recipe with `PACKAGES` and
no `build` function (45 of the 46 catalog groups; `lib32` is the other):

1. Download every pin for this system, as sources (offline cache first).
2. Unpack every package into the output tree, natively, without scripts, in
   pin order: first the packages whose `var/` is not kept, then delete
   `var/`, then the packages whose `var/` is kept as state seeds.
3. Place `FILES`.
4. Call `configure(tree)` or `configure(tree, src)`.
5. Finish the tree (section 2.8).

That is the block of about 218 lines that 46 group recipes copy today.

`configure` exists only with `PACKAGES`. Its first argument is the tree (the
output folder); the second, when the function takes two, is `src`, the
recipe's other `SOURCES` (answer 5; `steam` takes Valve's tarball, the
`xlibre` group its prebuilt server).

#### Packages in a recipe with its own `build`

Two recipes need the pinned packages but not the steps above:

- `desktop-runtime` makes an `sdk` folder from 80-odd Fedora RPMs: it keeps
  the shared libraries flat, compiles GSettings schemas and checks the
  library closure.
- `lib32` unpacks the i686 packages first and thins them to their 32-bit
  libraries before the 64-bit ones go in, because the two builds share
  identical non-library files.

**Decided (2026-11-16, owner away):** a recipe with `PACKAGES` does its own unpacking like this:
when it defines `build(src, out)`, Recipe still downloads the pins,
skips steps 2 to 4, and gives `src.packages`: a list, in pin order, of
`#{ name, path, sha256, bytes, keep_var, licence }` (`path` is the downloaded
package file, `keep_var` whether its `var/` is kept). The recipe unpacks with
`target_extract_rpm` or `target_extract_apk`. A `system` output is still
finished by Recipe afterwards. `packages` is then a reserved source name. Why:
it covers both recipes with one rule and no new helper, and it keeps the
pins, the download store and the disk rules the same for them.

### 2.7 The pins file (answer 15)

The pins a group installs are in a file beside the recipe, never in the
recipe text.

- Name: `NAME.pins`, beside `NAME.rhai` (`network.pins`).
- Written only by the generator: `cargo xtask base groups <distro>` for the
  groups, `recipes/generators/desktop-runtime-pins.py` for `desktop-runtime`.
  Recipe never writes it; nobody edits it by hand.
- It counts toward the recipe's identity: it is staged with the recipe, so a
  change to it is a different build, and `recipe index` records its SHA-256
  and size with the recipe (the index already lists the files beside a recipe
  that share its name). A remote source downloads it with the recipe and
  checks it against the index.

Form: plain text, one line per package, sections per C library.

```
# network.pins: written by `cargo xtask base groups`; do not edit.
# glibc: Fedora 45, locked 2026-11-16. musl: Alpine v3.24, locked 2026-11-16.
version 2026.11.16

[glibc rpm]
NetworkManager d671027360305dbedf54553eb37d49a9072c2ce291d29bc7c800d21f47cfeb47 2440040 var https://dl.fedoraproject.org/pub/fedora/linux/releases/45/Everything/x86_64/os/Packages/n/NetworkManager-1.58.1-3.fc45.x86_64.rpm GPL-2.0-or-later AND LGPL-2.1-or-later
NetworkManager-libnm 2a678dcf3676c22ae0f4bcbf6635d7216a3eff34266f3b9f739527037aed85b0 2293523 - https://dl.fedoraproject.org/pub/fedora/linux/releases/45/Everything/x86_64/os/Packages/n/NetworkManager-libnm-1.58.1-3.fc45.x86_64.rpm LGPL-2.1-or-later

[musl apk]
networkmanager e94d73bfaeceed7cbae6c10d7197ac7258a6b32b158117c84c2f91f32fcff92b 1491034 var https://dl-cdn.alpinelinux.org/alpine/v3.24/community/x86_64/networkmanager-1.52.2-r0.apk GPL-2.0-or-later
```

- `#` starts a comment line.
- `version V`: the group's version (see below).
- `[LIBC FORMAT]`: `LIBC` is `glibc`, `musl` or `any`; `FORMAT` is `rpm` or
  `apk`. `PACKAGE_FORMAT` is the format of the section in use.
- A package line: `NAME SHA256 BYTES VAR URL LICENCE`, separated by single
  spaces. `VAR` is `var` (its `var/` is kept as state seeds) or `-`.
  `LICENCE` is the rest of the line (it may contain spaces), `-` when not
  known. The file name is the last part of the URL. A `.i686.rpm` URL is a
  32-bit package.

Two things the generator writes into the recipe text today have to move,
because it no longer edits recipes:

**Decided (2026-11-16, owner away):** a group's version and network budget come as follows: the version is the pins file's `version` line, and a recipe
with `PACKAGES` declares none; Recipe adds the bytes of the pins in use to
`NEEDS.network_max_bytes`, so the recipe declares only the budget for its
other sources (`steam`: Valve's 20 MB tarball; 0 or nothing for plain
groups). The mirror hosts stay literal in `NEEDS.network`. Why: today
`version`, `network_max_bytes` and `OTHER_DOWNLOAD_BYTES` are rewritten in 47
recipes by the generator, and phase D1 stops it from touching recipe text.

### 2.8 `ACCOUNTS` and the finished system tree

```rhai
const ACCOUNTS = #{ musl: ["g plugdev -"] };
```

A map from C library to sysusers.d lines (`u`, `g`, `m`), for accounts a
system tree needs that its packages' own `usr/lib/sysusers.d` does not make
(Alpine packages make theirs in scripts, which never run). Unchanged in
meaning from `ACCOUNTS_GLIBC`/`ACCOUNTS_MUSL` and the `accounts` option of
`target_finalize`.

Every `system` output is finished by Recipe after `configure` or `build`:
the folders a system package does not own are removed (`run`, `tmp`, `boot`,
`home`, `root`, `proc`, `sys`, `dev`, `lost+found`, `var/run`, `var/tmp`,
`var/lock`), and the tree is finalized with owners resolved in the root it is
written into, `var/` as state seeds, and `ACCOUNTS`. A recipe does not call
`target_finalize` on a `system` output. This covers the groups, `rec-apps`
and `root-effects`. Trees for images (`rootfs`, `overlay`, `medium`, the
package-effects archive) are not `system` outputs; their recipes finalize
them as today.

### 2.9 `UPSTREAM` and `latest()` (answer 18)

Where the newest version can be found. Recipe asks only when the user asks
(`recipe updates --upstream`), never by itself, and only tells.

```rhai
const UPSTREAM = #{ github: "sharkdp/bat", prefix: "v" };
```

| Field | Meaning |
|---|---|
| `github` | `OWNER/REPO`. The newest version is the newest release that is not a pre-release. |
| `prefix` | text to take off the tag (`"v"` for `v0.26.1`, `"jq-"`, `"rust-v"`); default none. |

For a release page Recipe cannot read this way, the recipe defines
`latest()`, which returns the newest version as a string:

```rhai
// go: go.dev says the current version on its first line ("go1.27.4").
fn latest() {
    http_get("https://go.dev/VERSION?m=text").split("\n")[0].sub_string(2)
}
```

A recipe has `UPSTREAM`, `latest()`, or neither. Groups have neither: their
versions follow the locks. Most catalog recipes from GitHub releases get
`UPSTREAM` from their source URL when converted (phase E).

**Decided (2026-11-16, owner away):** the upstream check uses this network:
`UPSTREAM.github` is asked by Recipe itself, over HTTPS to GitHub's API, like
`recipe refresh`, without `NEEDS`; `latest()` runs alone on the host, may not
start programs, sees no inputs and no output, and reaches only
`NEEDS.network` (so `go` uses `go.dev:443`, which it declares already). Why:
a GitHub API host in `NEEDS` would also be granted to the build, while
`latest()` is recipe code and stays inside the recipe's grants.

### 2.10 `RERUN` (answer 9)

```rhai
const RERUN = true;
```

The recipe is run every time it is asked for; a result is never reused and
it is never "built". Only `root-effects` needs it: it rebuilds a root's
caches after every system change.

### 2.11 `deps`, `build_deps`

Unchanged:

```rhai
let deps = ["desktop-libs"];
let build_deps = ["desktop-runtime"];
```

`deps` are installed before the recipe and stay (runtime). `build_deps` are
there only while it builds. Each dependency runs separately under its own
`NEEDS`. A dependency's outputs reach the consumer through `DEPS`
(section 4); a `tools` output is also put on the consumer's `PATH`.

### 2.12 `extends` and `import` (answer 3)

Both stay.

`//! extends: base.rhai` on the first line makes the file a child of a base
recipe. The base holds the shared code; the child completes it. A function
the child defines replaces the base's of the same name and arity; a function
the base calls and only the child defines is the child's part of the
template. One level only: a base that extends fails, as today.

**Decided (2026-11-16, owner away):** declarations combine between a base and its child like this:
a declaration is taken whole from the child when the child has it,
else whole from the base; maps are never merged field by field. `NEEDS` must
be in the child, as today. Why: the templates in use (`locked-preseed`,
`package-effects`, `rootfs-producer`) need no merging, and a merge rule would
make it hard to see what a child may use.

Templates in use, converted:

- `locked-preseed` (six children): the child declares its `NEEDS` with its
  output's name, imports its lock and defines `rows()`; the base's `build`
  downloads every row into the output. The child no longer sets the output's
  name (`ctx.output`), because the body never names it.
- `package-effects` (two children): the base's `build` calls the child's
  `verify_inputs()`, `unpack(root)` and `package_effects(root)`.
- `rootfs-producer` (two children): the child sets the init system as a
  literal constant, `const INIT_SYSTEM = "systemd";`, which the base's
  `build` reads.

`import "path.rhai" as name;` stays, with a literal path relative to the
recipe's folder. A module has no `RECIPE_API` and no `NEEDS`; `recipe list`
skips it. A module's functions see the engine's constants (section 1). The
seven lock modules keep their form (`fn packages()` returning
`[name, id, URL, SHA256]` rows); the builder reads them too.

Shared modules after the rewrite (answer 14):

- `recipes/look.rhai`: the palette, colour scheme and terminal scheme code
  now copied into `konsole`, `qterminal`, `pcmanfm-qt`, `lxqt` and `plasma`.
- `recipes/os-facts.rhai`: generated by `cargo xtask base groups` from each
  variant's `distro.toml`, replacing the `RPM_OS`/`APK_OS` blocks copied into
  those five and into `firefox` and `login-screen`.
- `system-files.rhai`, `installed-system.rhai`, `kernel/metadata.rhai` in the
  OS set, rewritten without `ctx`.

A remote source must carry the modules its recipes import; `recipe index`
lists them beside the recipes, and fetching a recipe fetches its modules.

**Decided (2026-11-16, owner away):** there is no engine constant `OS`; recipes import
`os-facts.rhai` (answers 4 and 14), the module only. A recipe reads the facts with `os_facts::of(PACKAGE_FORMAT)`
(`rpm`: LevitateOS, `apk`: AcornOS), as the copied blocks choose today. Why:
the two answers describe the same facts twice; Recipe is also released for
other distributions and should not carry LevitateOS's colours; and a module
is one generated file the generator already knows how to write.

## 3. The functions

### 3.1 `build(src, out)`

The recipe's own work: compiling, adjusting, writing. Optional.

- `src`: a map from source name to path (section 2.4); `src.packages` with
  `PACKAGES` (section 2.6). An empty map when there are no sources.
- `out`: the output. For a `directory` output, the path of the folder, which
  exists and holds the placed `FILES` (or a copy of the `target` input,
  section 3.6). For a `file` output, the path where the file must be made;
  its folder exists.
- It returns nothing. A throw fails the run.
- It may replace the whole output folder (`rm(out); mv(dir, out)`).

It may start programs (then the recipe runs in Ginux, as today), download
when the recipe has no `SOURCES` (answer 8, below), and use `WORK` for
anything that is not the result.

```rhai
// A compiled program (an illustration; no catalog recipe compiles C today).
fn build(src, out) {
    if PREFIX == () { throw "htop is built for the folder it is activated in; install it with activation"; }
    let dir = src.source;
    exec_checked("./configure", ["--prefix=" + PREFIX], #{ cwd: dir });
    exec_checked("make", ["-j" + JOBS], #{ cwd: dir });
    let staged = join_path(WORK, "staged");
    exec_checked("make", ["install", "DESTDIR=" + staged], #{ cwd: dir });
    rm(out);
    mv(staged + PREFIX, out);
}
```

The program is configured for `PREFIX`, where the package will be once
activated, and installed through a staging folder, because `out` is the
package folder itself: `make install DESTDIR=out` with `--prefix=PREFIX`
would put the files under `out/PREFIX/...`. The note's example has that
mistake.

#### Downloading inside `build` (answer 8)

A recipe that declares no `SOURCES` may download in `build` with `download`,
`download_exact`, `fetch_bounded`, `git_checkout` and their relatives, within
`NEEDS.network`. Recipe then cannot tell "downloaded" apart for it: the
recipe has no download step, and its downloads are build material.

Who needs it:

| Recipe | Why its sources cannot be declared |
|---|---|
| the six preseeds (through `locked-preseed`) and `linux-deps` | rows come from a lock module; the preseeds take files from the builder's `store` input before the network |
| `release-medium` | URLs come from its `pins` input; also the `store` input |
| `prebuilt-fetch` | URL and digest come from defines and a downloaded manifest |
| `linux`, `linux-prebuilt` | chosen by a define, pins in `kernel/metadata.rhai`, replaced by the `kernel_archive` input when given |
| `bun-toolchain` | chosen by its `runtime_target` input |
| `tui-binary` | `bun install` fetches the frozen package graph itself |

### 3.2 Several outputs (answer 10)

A recipe that declares several outputs gets `out` as a map from output name
to path; its `build` uses each by name (the owner's decision). Recipe chooses
the paths. Which recipes, and what they use:

| Recipe | Outputs |
|---|---|
| `install-tools` | `program` (file), `rootfs` (directory) |
| `bun-toolchain` | `compiler` (file), `runtime` (file) |
| `prebuilt-fetch` | `files` (directory), `manifest` (file) |
| `linux` | `bundle` (directory), `kernel` (file), `modules` (directory) |
| `linux-prebuilt` | the same three |

The kernel recipes do not fit "Recipe chooses the paths": their `kernel` and
`modules` are inside `bundle`, at a path that depends on the kernel release
(`staging/usr/lib/modules/<release>`). The builder uses only `bundle`
(`recipe/linux.rs` checks that the other two exist).

**Decided (2026-11-16, owner away):** the kernel recipes do not keep three outputs: one output,
`bundle`. The builder checks the kernel image and the modules
inside the bundle itself, with `bundle_mismatch` from `kernel/metadata.rhai`,
which it already evaluates. Three recipes keep several outputs. Why: the
alternative is copying the modules (hundreds of MB) into a second output, or
letting a recipe point an output into another, which brings back
`ctx.outputs`.

### 3.3 `configure(tree)`, `configure(tree, src)` (answer 4, 5)

Only in a recipe with `PACKAGES` and no `build`. `tree` is the system tree
after the packages and `FILES` are in it; `src` is the other sources. It
writes the group's own configuration into the tree, with the tree helpers
(section 5) and the constants `PACKAGE_FORMAT` and `HOST_LIBC`.

```rhai
fn configure(tree) {
    enable_service(tree, "NetworkManager.service", "networkmanager");
}
```

### 3.4 `verify(out)`

An optional check of the result, after `build` or `configure` and before
installing. It only reads (files and their contents) and starts no program
(the owner's decision). A throw fails the run. It is not run again later;
"installed" is Recipe's record (section 6).

```rhai
// rootfs-producer: every command the capabilities input lists is in the tree.
fn verify(out) {
    for line in read_file(INPUTS.capabilities).split("\n") {
        let line = trim(line);
        if !line.starts_with("command ") { continue; }
        let command = trim(line.sub_string(8));
        let found = false;
        for dir in ["usr/bin", "usr/sbin", "bin", "sbin"] {
            if exists(join_path(out, dir + "/" + command)) { found = true; }
        }
        if !found { throw "the rootfs lacks the command " + command; }
    }
}
```

Checks that run a program (`cmake --version`, `python3 -c ...`, `go version`,
`desktop-runtime`'s closure check in Python, `install-tools`' `--version`)
move into `build` or are dropped (phase E3).

### 3.5 `latest()`

Section 2.9. No arguments; returns the newest upstream version as a string.

### 3.6 Writing into the caller's tree (answer 11)

Unchanged in meaning: an input named `target` is the caller's existing tree,
and it seeds the output before `build` runs, so a recipe can add to it
(`linux` keeps its `kernel-build` folder from the previous bundle and builds
again on top). With one output, `out` starts as a copy of `INPUTS.target`.

The six preseed children declare a `target` input the builder never passes;
they drop it when converted.

### 3.7 `cleanup(reason)` (answer 12)

`cleanup(reason)` stays as an optional function, without `ctx`. No recipe is
required to define it. Seven OS recipes define it today: `linux` and
`linux-prebuilt` delete the tarball after acquiring and the partial source
after a failure; `install-tools`, `tool-crate` and `reclogin` delete `target/`
and `cargo/` on a manual cleanup; `rootfs-producer` and `package-effects`
delete their stamp file. Recipe now deletes the build material itself (6.5),
so most of them need no function after the rewrite; a recipe that has
something of its own to delete still can.

It receives one argument, `reason`, a string with the same values as today:
`manual` and `auto.<phase>.success` or `auto.<phase>.failure`, where `<phase>`
is the step that just ended (`download`, `build` or `install`). `manual` is
what `recipe cleanup` passes; the explicit `--reason TEXT` option is gone
(6.3), so no other manual value exists. It returns nothing.

Recipe calls it once at the end of a run: after a successful run or a failed
one (the `auto` reasons), and on `recipe cleanup` (`manual`). It runs after
the engine's own clean-up decisions (6.5) and cannot stop the engine from
deleting material. It sees the constants of section 4 and may use the
helpers; it has no `src` and no `out`. The limits of `verify` do not apply
(it may delete files and start programs), but it must not delete installed
files: the program Recipe installed stays. A failure of `cleanup` after a
success fails the command; after a failure it is reported next to the first
error.

**Decided (2026-11-16, owner away):** `cleanup(reason)` stays as an optional recipe function, without
`ctx`, called with the reasons above. Why: the plan's answer 12 keeps it, no
recipe is required to define it, and the engine's own deletion of material
(6.5) does not depend on it.

## 4. The engine's constants

They replace `RUNTIME` and `ctx.runtime`. All are read-only and visible
everywhere (section 1).

| Constant | Value | Replaces |
|---|---|---|
| `PREFIX` | where the `package` output will live once activated (`PROFILE/.packages/NAME` through the profile's link); `()` with `--no-activate`, for a dependency, and for other outputs. It is part of the identity. | `RUNTIME.paths.prefix` |
| `JOBS` | how many jobs the user asked for, an integer from 1 to 16 (`"-j" + JOBS` makes `"-j4"`; `JOBS.to_string()` for an environment value) | `runtime.options.jobs`, the `JOBS` define |
| `HOST_LIBC` | `"glibc"`, `"musl"` or `"unknown"`: the C library of the system the result is installed on, from its dynamic loaders (for a system change, the root written into) | `RUNTIME.host.libc` |
| `PACKAGE_FORMAT` | `"rpm"` or `"apk"`: the format of the pins in use; `()` without `PACKAGES` | `ctx.format` in the groups |
| `INPUTS` | map from each declared input to its read-only path; `()` for an optional input not given | `RUNTIME.paths.inputs + "/NAME"` |
| `DEFINES` | map from each declared define to its value (given, else the default) | bare define constants, `is_def_var` |
| `DEPS` | map from each dependency name to the path of its output (read-only); for a dependency with several outputs, a map from output name to path | `RUNTIME.paths.dependencies + "/DEP/OUTPUT"` |
| `WORK` | a private folder for build material, deleted after a successful run unless kept | `RUNTIME.paths.build`, `.temp`, `.tools` |

Not carried over, because no recipe needs them once the hooks are gone:
`paths.recipe_dir`, `paths.base_recipe_dir`, `paths.output`, `paths.target`
(the output is `out`), `paths.tools` (tools are on `PATH`), `target.os`,
`target.arch`, `host.os`, `host.arch`, `operation`, `options.force_*`,
`options.persist_state`.

**Decided (2026-11-16, owner away):** `WORK` and the rest of the table are the whole list; one `WORK` folder instead of `build`, `temp` and `tools`.
Why: the only `temp` users write a manifest there that the rewrite deletes,
the only `tools` user (`linux-deps`) wrote its own output there, and a
`DEPS` path or `PATH` covers every consumer.

`DEPS["desktop-runtime"]` is `antigravity`'s `sdk`;
`DEPS["bun-toolchain"].compiler` is `tui-binary`'s compiler.

**Decided (2026-11-16, owner away):** `DEPS` of a single-output dependency is the path itself, and a map only for the three multi-output recipes, so a
consumer never names a normal recipe's output either. Why: it follows the
owner's rule that a normal recipe's output is never named.

## 5. Helpers

The helper catalog (`recipe-helpers(7)`) stays as it is, with these changes.

Gone: `ready`, `needs_work` (there are no checks).

New, for writing into a system tree; they replace the copies in 45 group
recipes (`put`, `put_link`, `tree_text`, `enable_service`), with the tree in
place of `ctx`:

| Helper | Does |
|---|---|
| `put(tree, path, text, mode)` | writes `text` at `path` in the tree, replacing what is there, with `mode` in the target metadata |
| `put_link(tree, path, target)` | a symbolic link at `path` to `target` |
| `tree_text(tree, path)` | the text of a packaged file; fails naming the file when the packages no longer ship it |
| `enable_service(tree, unit, service)` | on an `rpm` tree the links `systemctl enable` makes for the systemd `unit`, with its `Alias=` names; on an `apk` tree the OpenRC `service` in the default runlevel; an empty name skips that system |

New, for saved values and resuming (section 7): `save`, `saved`,
`checkpoint`, `reached`.

**Decided (2026-11-16, owner away):** the four tree helpers move into the engine under these
names. `system-files.rhai` and `installed-system.rhai` drop
their own `put` and `link` for them. Why: the plan moves the copied group code
into the engine, and these four are part of the copy.

**Decided (2026-11-16, owner away):** the program helpers keep their names:
`exec_checked`, `exec_output`, `exec` and the rest stay. The note's `run(...)`
and `output_of(...)` were shorthand. Why: renaming touches every recipe and
the man page for nothing, and `root-effects` defines its own function named
`run`.

## 6. Steps, states and commands

### 6.1 The four steps

| Step | What Recipe does |
|---|---|
| download | fetches `SOURCES` and the pins for this system into the download store |
| build | unpacks the sources, unpacks the packages, places `FILES`, runs `build` or `configure`, finishes a `system` tree, runs `verify`, and keeps the result |
| install | activates a `package` output, writes a `system` output into the root, or publishes a producer's outputs |
| cleanup | deletes the program's downloads, unpacked sources, build material and saved state |

`install` does every step still missing, in order, and then cleanup, unless
the user chose to keep the material. Each step can also be run alone.

A recipe without `SOURCES` and `PACKAGES` has no download step of its own. A
recipe without a `build` function shows no build step (the plan: "a recipe
that compiles nothing shows no build step"); its unpacking and placing still
happen, as part of install.

### 6.2 The states of a program

| State | Meaning |
|---|---|
| `none` | nothing of it here, or only part of its downloads |
| `downloaded` | all its sources and pins for this system are in the download store |
| `built` | a result with the current identity is kept, not installed |
| `installed` | the receipt lists it (any version) |

Recipe decides the state from its own records (answer 13): a download by its
SHA-256 in the store; a result by its identity and the checksums Recipe
recorded for its files; an installed program by its receipt. No recipe code
runs to decide it.

The identity of a build is, as today, the recipe's text, its base, its
modules, its pins file, the checksums of the selected sources and pins, its
inputs' contents, its defines, its dependencies' identities, `PREFIX`, the
root image and the engine's behaviour versions.

Beside the state, Recipe reports what the program holds on disk: its
downloads, its build material (unpacked sources, `WORK`, a result not
installed) and an unfinished run's saved state. An installed program with
the keep option on holds material too.

### 6.3 Commands

| Command | Does |
|---|---|
| `recipe download NAME` | the download step only |
| `recipe build NAME` | download what is missing, then build; does not install |
| `recipe install NAME` | every missing step, then cleanup unless kept |
| `recipe update NAME` | installs the version the recipe as fetched names, replacing the installed one; never edits a recipe |
| `recipe remove NAME` | removes the installed files by the receipt, then whatever is left of its downloads and build material |
| `recipe cleanup NAME` | deletes the program's downloads and build material; the installed program stays |
| `recipe cleanup NAME --downloads` | its downloads only |
| `recipe cleanup NAME --build` | its build material only (unfinished run included) |
| `recipe cleanup` | the downloads and build material of every program |
| `recipe cleanup --logs` | the diagnostic logs |
| `recipe state NAME` | prints the program's state (section 8.5) |
| `recipe runtime remove` | deletes the cached build image; it is fetched again when a recipe needs it |

Unchanged: `list`, `installed`, `updates` (with `--upstream`, new),
`storage`, `history`, `info`, `index`, `validate`, `inspect`, `explain`,
`diff`, `hash`, `source`, `refresh`, `runtime status|fetch|stage`, and the
system scope (`--system install|update|remove|installed|state`,
`--root`, `apply-release`, `record-base`, `--no-effects`).

Removed: `isinstalled`, `isbuilt`, `isacquired` (replaced by `state`), the
options `--force-install`, `--force-build`, `--force-acquire` (to redo a step,
clean it up and run it again), `cleanup --reason`, and the features of
section 0.

`recipe history` stays: it costs no space worth naming. Its entries keep
`at`, `what`, `package`, `from`, `to` and `source`; `what` is `install`,
`update`, `remove` or `release`.

**Decided (2026-11-16, owner away):** the three checks become one command, `recipe state
NAME`; it exits 0 and prints the state, and the builder's one
use (`isinstalled` on `linux-prebuilt`, the release build's kernel check)
becomes the builder's own check with `bundle_mismatch`. The builder's
`--force-*` uses (kernel pipeline, `recipe/mod.rs`, `stamp.rs`) become
`recipe cleanup NAME` before the run. Why: the state is Recipe's record now,
not three pieces of recipe code.

**Decided (2026-11-16, owner away):** `recipe remove` removes the installed
program and all its material, as the one-action Remove requires;
`recipe remove NAME --keep` removes only the installed program and keeps its
downloads and build material (the plan's "only uninstall"). Why: section 3 of
the plan asks for both, and with the default settings nothing else is left
after an install anyway.

### 6.4 The keep option

`--keep` on `install`, `update` or `build`: the downloads and build material
stay after a successful install. Off by default. It is remembered per
program: later installs and updates of that program keep too.

**Decided (2026-11-16, owner away):** the keep option is undone like this: `--no-keep` on
`install`, `update` or `build` turns it off and deletes the material at once
when the program is installed; `recipe remove NAME` forgets it; plain
`recipe cleanup` (and `recipe cleanup NAME`) deletes kept material too but
leaves the setting on, so the next install keeps again. Why: the plan says
only "remembered per program"; this gives a way out and keeps "clean up
everything" honest.

### 6.5 Disk rules

- Downloads, unpacked sources, `WORK` and saved state exist only between
  steps. After a successful install they are deleted, unless kept.
- A result moves into the package store, and a placed file moves from the
  unpacked source, wherever both are on the same disk.
  - Placing `FILES` from an unpacked source is a rename in both kinds of run.
    In Ginux, `/work` and `/out` are two assignments of one memory storage,
    and a rename between them moves the name, not the bytes. A plain-file
    source and a `git` checkout are the downloads themselves and are copied
    (section 2.5).
  - A host run moves its outputs from the run's directory into publication
    where that directory and the state root share a disk. With saved state
    (the default) it first archives the run's `work` and `out` as the kept
    result, so the result's bytes are on disk twice until cleanup deletes
    the kept result after the install; without saved state nothing is
    archived.
  - A Ginux run hands back one archive of its `/work` and `/out`, and its
    outputs are unpacked from that archive: Ginux keeps the run's files in
    its memory storage and has no other way out. Until cleanup deletes the
    kept result (or, without saved state, until the run ends) the result's
    bytes are on disk twice, in the archive and unpacked. The same holds
    when an install publishes a result kept by an earlier `build`.
- Saved state exists only while a run is unfinished, one copy at most, and
  is deleted when the install succeeds.
- A download two programs name is deleted only when neither still needs it.
- `recipe remove` frees the program's space at once: there are no earlier
  versions.

**Decided (2026-11-16, owner away):** these rules also apply to producer runs
(`--no-activate`, the OS builder), the same rules, with
"install" meaning "outputs published"; the builder passes `--keep` where it
wants material kept. Why: the builder keeps its own stamps keyed by identity
and reads only published outputs, so nothing it uses is lost, and one rule is
simpler than two.

### 6.6 Single steps and the app

The app's page for a program shows the state and two buttons, Install (or
Update) and Remove. Behind "One step at a time" it shows download, build,
install and cleanup, each with its button and what it holds on disk, taken
from `steps` in `recipe list --json` and from `recipe state`.

## 7. Saved values and resuming

`ctx.state` is gone. A recipe that must remember a value across an
interruption saves it:

| Helper | Does |
|---|---|
| `save(name, value)` | keeps a JSON-compatible value (map, array, string, boolean, `()`, finite number) under `name` |
| `saved(name)` | the value saved under `name`, or `()` |
| `checkpoint(name)` | records the work folders, the sources, the output and every saved value as one checkpoint |
| `reached(name)` | `true` when this run was restored from checkpoint `name` or a later one |

Rules, as for `ctx.state` today: a value is saved only together with the
files of its checkpoint; a saved value never proves that work is done; Recipe
keeps one checkpoint at most and deletes it when the install succeeds.
`--no-persist-state` turns checkpoints off.

An interrupted `build` resumes from the newest checkpoint: Recipe restores it
and calls `build(src, out)` again from the top.

```rhai
fn build(src, out) {
    let dir = src.source;
    if !reached("configured") {
        exec_checked("./configure", ["--prefix=" + PREFIX], #{ cwd: dir });
        checkpoint("configured");
    }
    exec_checked("make", ["-j" + JOBS], #{ cwd: dir });
    // ...
}
```

**Decided (2026-11-16, owner away):** a resumed `build` skips what it did with the
function `reached(name)` above. Why: the note names `save`, `saved` and
`checkpoint` but not how a restarted `build` knows where it stopped, and
`saved` alone cannot order several checkpoints.

Who uses saved values today: `bun-toolchain`, `tui-binary` and
`prebuilt-fetch`, each to carry a checksum from one hook to the next. In one
`build` function those become local variables; after the rewrite no recipe
needs `save` or `checkpoint`. They stay for long builds.

## 8. JSON

Every command below prints JSON with `--json`, and when its output is piped.
`schema_version` is 1 in each.

### 8.1 `recipe list --json`

Every recipe Recipe can find, read without running anything. Top level, as
today: `schema_version`, `host_libc`, `collections`, `sources`, `recipes`,
`problems`.

Each recipe:

| Field | Meaning |
|---|---|
| `name`, `version`, `description` | from `PACKAGE` (a group's version from its pins file) |
| `path` | the recipe file |
| `source` | the source it comes from |
| `about` | `ABOUT` as written, or `null` |
| `needs` | `NEEDS`, with defaults filled in |
| `deps`, `build_deps` | names |
| `steps` | the steps it has, from `download`, `build`, `install`, `cleanup` |
| `functions` | the functions it defines, from `build`, `configure`, `verify`, `latest` |
| `upstream` | `UPSTREAM` as written; `{"latest": true}` when it defines `latest()`; else `null` |
| `runs_here`, `not_runnable_reason`, `not_runnable_override` | as today |
| `state` | `none`, `downloaded`, `built` or `installed`, on this system |
| `installed_version` | the installed version, or `null` |
| `keep` | whether the keep option is on |

Gone: `hooks`.

```json
{"schema_version": 1, "host_libc": "glibc", "collections": ["/home/u/.local/share/recipe/recipes"],
 "sources": [{"name": "levitate", "kind": "remote", "title": "LevitateOS recipes", "path": null, "checked": 1795300000, "problem": null}],
 "recipes": [
  {"name": "ripgrep", "version": "15.2.0", "description": "Fast recursive text search with rg",
   "path": "/home/u/.local/share/recipe/sources/levitate/ripgrep.rhai", "source": "levitate",
   "about": {"kind": "tool", "category": "utilities", "adds": {"programs": ["rg"], "menu": []}},
   "needs": {"inputs": {}, "defines": {}, "outputs": {"package": "directory"},
             "network": ["github.com:443", "release-assets.githubusercontent.com:443"],
             "network_max_bytes": 8388608, "memory_mib": 2048, "storage_max_objects": 16384,
             "storage_max_bytes": null, "timeout_secs": 300},
   "deps": [], "build_deps": [], "steps": ["download", "install", "cleanup"], "functions": [],
   "upstream": {"github": "BurntSushi/ripgrep"},
   "runs_here": true, "not_runnable_reason": null, "not_runnable_override": null,
   "state": "installed", "installed_version": "15.2.0", "keep": false}],
 "problems": []}
```

### 8.2 `recipe installed --json`

What is installed for this user (or, with `--system`, in the roots; that
form keeps today's `booted`, `other`, `not_done` shape, and each package gets
the fields below that apply).

| Field | Meaning |
|---|---|
| `schema_version`, `profile` | as today |
| `packages` | one entry per installed program |

Each package:

| Field | Meaning |
|---|---|
| `name`, `version` | installed |
| `recipe`, `source`, `commit`, `sha256` | which recipe file installed it |
| `needs` | the `NEEDS` it had when installed |
| `installed_on` | seconds since 1970 |
| `size_bytes` | its installed files |
| `keep` | the keep option |
| `on_disk` | `{"downloads": BYTES, "build": BYTES, "unfinished": BYTES}`: material it holds beside the installed files |
| `update` | `null`, or `{"to": VERSION, "reason": "version" or "recipe", "source": NAME}`: the recipe as last fetched names another version or is another file. Nothing is fetched to know it. |

Gone: `held`, `kept`, earlier versions. "Newer upstream" is not here: it is
known only when the user asks (section 8.3).

```json
{"schema_version": 1, "profile": "/home/u/.local/share/recipe/profile",
 "packages": [
  {"name": "bat", "version": "0.26.0", "recipe": "/home/u/.local/share/recipe/sources/levitate/bat.rhai",
   "source": "levitate", "commit": "4c1e9a0", "sha256": "9f2b...", "needs": {"outputs": {"package": "directory"}},
   "installed_on": 1795300000, "size_bytes": 6291456, "keep": false,
   "on_disk": {"downloads": 0, "build": 0, "unfinished": 0},
   "update": {"to": "0.26.1", "reason": "version", "source": "levitate"}}]}
```

**Decided (2026-11-16, owner away):** `installed --json` carries the "Update" mark: the offline mark as above, so the Installed page needs one
call; "Newer upstream" comes only from `updates --upstream`. Why: the plan
puts both marks on the Installed page and no Updates page, but asking
upstream must stay a separate request.

### 8.3 `recipe updates --json`

Without `--upstream`: what `recipe update` would change, from the recipes as
last fetched; nothing is fetched or run (as today). With `--upstream`
(optionally with names): Recipe also asks upstream for each installed
program, or each named recipe, and tells what it found. The two marks are
in separate lists and never mixed.

| Field | Meaning |
|---|---|
| `schema_version` | 1 |
| `upstream_checked` | whether upstream was asked in this run |
| `updates` | installable updates: `name`, `source`, `from`, `to`, `reason` (`version` or `recipe`), `changes` (what the newer recipe may use that the installed one may not, as today) |
| `upstream` | only with `--upstream`: `name`, `installed` (version or `null`), `recipe_version`, `latest` (or `null`), `newer` (whether `latest` is newer than `recipe_version`), `how` (`github` or `latest()`), `error` (or `null`) |
| `problems` | `name`, `problem` |

Gone: `held`.

```json
{"schema_version": 1, "upstream_checked": true,
 "updates": [{"name": "bat", "source": "levitate", "from": "0.26.0", "to": "0.26.1", "reason": "version", "changes": []}],
 "upstream": [{"name": "bat", "installed": "0.26.0", "recipe_version": "0.26.1", "latest": "0.27.0", "newer": true, "how": "github", "error": null},
              {"name": "muse", "installed": "1.4.18-R8587.1", "recipe_version": "1.4.18-R8587.1", "latest": null, "newer": false, "how": null, "error": "the recipe says nowhere where its newest version is"}],
 "problems": []}
```

### 8.4 `recipe storage --json`

Everything Recipe holds on disk, by kind. Each byte is counted once.

| Field | Meaning |
|---|---|
| `schema_version`, `profile`, `state_dir` | |
| `total_bytes` | the sum of all kinds |
| `kinds` | one entry per kind, always all seven, in this order |

Each kind: `kind`, `bytes`, `removable`, `remove` (the command that removes
it, or `null`), `programs` (per program: `name`, `bytes`; empty for the last
two kinds), `path` (for `build_image` and `logs`, else `null`).

| Kind | What | Removed by |
|---|---|---|
| `installed` | installed programs' files | `recipe remove NAME` |
| `downloaded` | downloads of programs not installed | `recipe cleanup NAME --downloads` |
| `built` | build material and results of programs not installed | `recipe cleanup NAME --build` |
| `kept` | material of installed programs with the keep option | `recipe cleanup NAME` |
| `unfinished` | saved state of interrupted runs | `recipe cleanup NAME --build` |
| `build_image` | the cached compiler image (about 1.3 GB) | `recipe runtime remove`; `removable` is false for an image installed with the system |
| `logs` | diagnostic logs | `recipe cleanup --logs` |

Gone: `active_bytes`, earlier versions.

```json
{"schema_version": 1, "profile": "/home/u/.local/share/recipe/profile", "state_dir": "/home/u/.local/state/recipe",
 "total_bytes": 1396703232,
 "kinds": [
  {"kind": "installed", "bytes": 12582912, "removable": true, "remove": "recipe remove NAME", "programs": [{"name": "ripgrep", "bytes": 6291456}, {"name": "bat", "bytes": 6291456}], "path": null},
  {"kind": "downloaded", "bytes": 8312345, "removable": true, "remove": "recipe cleanup NAME --downloads", "programs": [{"name": "gh", "bytes": 8312345}], "path": null},
  {"kind": "built", "bytes": 0, "removable": true, "remove": "recipe cleanup NAME --build", "programs": [], "path": null},
  {"kind": "kept", "bytes": 0, "removable": true, "remove": "recipe cleanup NAME", "programs": [], "path": null},
  {"kind": "unfinished", "bytes": 0, "removable": true, "remove": "recipe cleanup NAME --build", "programs": [], "path": null},
  {"kind": "build_image", "bytes": 1363148800, "removable": true, "remove": "recipe runtime remove", "programs": [], "path": "/home/u/.cache/recipe/images"},
  {"kind": "logs", "bytes": 12659175, "removable": true, "remove": "recipe cleanup --logs", "programs": [], "path": "/home/u/.local/state/recipe/logs"}]}
```

**Decided (2026-11-16, owner away):** `recipe cleanup --logs` and `recipe runtime remove`
are the ways to remove the last two kinds. Why: the plan requires a
way to remove each kind, and neither exists today.

### 8.5 `recipe state NAME --json`

```json
{"schema_version": 1, "name": "gh", "version": "2.103.3", "state": "downloaded", "installed_version": null,
 "keep": false, "steps": ["download", "install", "cleanup"],
 "on_disk": {"downloads": 8312345, "build": 0, "unfinished": 0}}
```

`version` is the recipe's; `installed_version` the installed one or `null`.

### 8.6 A finished run

`install`, `update`, `download`, `build`, `remove` and `cleanup` print one
result at the end (and write it to `--json-output FILE`). It replaces the
returned `ctx`.

| Field | Meaning |
|---|---|
| `schema_version` | 1 |
| `operation` | `download`, `build`, `install`, `update`, `remove` or `cleanup` |
| `name`, `version` | the recipe's |
| `state` | the program's state after the run |
| `steps` | per step of this run: `step`, `status` (`done`, `already` when it was done before, `none` when the recipe has no such step) |
| `outputs` | output name to absolute published path, for a run that built or published; `{}` otherwise. The builder reads this. |
| `activation` | `{"profile", "bin"}` when a package was activated, else `null` |
| `kept` | whether the material was kept |
| `freed_bytes` | bytes deleted by cleanup or remove in this run |
| `dependencies` | per dependency that ran: `name`, `state`, `steps` |
| `contract`, `network_usage` | the Ginux contract the run was checked against, as Recipe prints it: `catalog`, `digest` and the `contract` itself (`catalog`, `allow`, `deny`: a few short rule names); absent for a run on the host. And the bounded network summary |

A failed run exits nonzero and prints `{"failure": {...}}` as today
(`libc_mismatch`, `system_mismatch`, a denied connection's `refused`). A
system change (`--system`) keeps today's result fields (`action`, `package`,
`other_root`, `effects`, ...) and adds `state` and `freed_bytes`.

```json
{"schema_version": 1, "operation": "install", "name": "ripgrep", "version": "15.2.0", "state": "installed",
 "steps": [{"step": "download", "status": "done"}, {"step": "build", "status": "none"},
           {"step": "install", "status": "done"}, {"step": "cleanup", "status": "done"}],
 "outputs": {"package": "/home/u/.local/share/recipe/store/ripgrep"},
 "activation": {"profile": "/home/u/.local/share/recipe/profile", "bin": "/home/u/.local/share/recipe/profile/bin"},
 "kept": false, "freed_bytes": 8925511, "dependencies": [],
 "contract": {"catalog": 1, "digest": "sha256:5d1c...", "contract": {"catalog": 1, "allow": ["syscall.*", "filesystem.path.read", "..."], "deny": ["..."]}},
 "network_usage": {"complete": true, "bytes": 2179134}}
```

The builder reads `ctx.kernel_version` from the kernel recipe's result
today. There is no recipe-reported value any more; the builder already knows
the release from its pin (`pin(variant)?.release` in `pipeline/kernel.rs`)
and uses that.

### 8.7 Events

With `--machine-events`, one JSON object per line with an `event` field:

- `plan`: every recipe of the run in order, with its `steps`.
- `step`: `recipe`, `step`, `status`, `msg`. `status` is exactly one of
  `running`, `done`, `already`, `none`, `skipped`, `failed`.
- `fetch`: `recipe`, `source`, `bytes`, `total` (null when unknown).
- `progress`: `recipe`, `phase`, `what`, `done`, `total`.
- `done`: `recipe`; the last event of a run that succeeded.
- `failed`: `recipe`, `step` (null when no step failed), `reason`, and
  `refused` when Ginux denied a connection; the last event of a run that failed.

The engine emits no `output` or `say` event, and there is no `agent` event.

## 9. Examples

The API line is in every example; comments are short.

### 9.1 A prebuilt program (`ripgrep`, shape a)

```rhai
const RECIPE_API = 1;
const NEEDS = #{
    outputs: #{ "package": "directory" },
    network: ["github.com:443", "release-assets.githubusercontent.com:443"],
    network_max_bytes: 8388608,
    timeout_secs: 300,
};
const ABOUT = #{
    kind: "tool", category: "utilities",
    details: "Searches the files under a folder for a text pattern, fast.",
    homepage: "https://github.com/BurntSushi/ripgrep", license: "MIT OR Unlicense",
    adds: #{ programs: ["rg"], menu: [] }, keeps: [],
};
const PACKAGE = #{ name: "ripgrep", version: "15.2.0", description: "Fast recursive text search with rg" };
const UPSTREAM = #{ github: "BurntSushi/ripgrep" };
const SOURCES = #{
    release: #{
        url: "https://github.com/BurntSushi/ripgrep/releases/download/15.2.0/ripgrep-15.2.0-x86_64-unknown-linux-musl.tar.gz",
        sha256: "33e15bcf1624b25cdd2a55813a47a2f95dbe126268203e76aa6a585d1e7b149c",
    },
};
// One source: paths are inside it; its single top folder is left out.
const FILES = [
    ["rg",          "bin/rg", 0o755],
    ["doc/rg.1",    "share/man/man1/rg.1"],
    ["COPYING",     "share/licenses/ripgrep/COPYING"],
    ["LICENSE-MIT", "share/licenses/ripgrep/LICENSE-MIT"],
    ["UNLICENSE",   "share/licenses/ripgrep/UNLICENSE"],
];
```

### 9.2 A whole folder and a launcher (`pi`)

```rhai
const RECIPE_API = 1;
const NEEDS = #{ outputs: #{ "package": "directory" }, network: ["github.com:443", "release-assets.githubusercontent.com:443"], network_max_bytes: 62914560, timeout_secs: 3600 };
const PACKAGE = #{ name: "pi", version: "1.5.2", description: "pi terminal coding agent" };
const UPSTREAM = #{ github: "earendil-works/pi", prefix: "v" };
const SOURCES = #{
    release: #{ url: "https://github.com/earendil-works/pi/releases/download/v1.5.2/pi-linux-x64.tar.gz",
                sha256: "811c6f2c9e7aaee252b5404252e556fb713aa209c1f7f924226aa77d4ef70cb5" },
};
const FILES = [["", "lib/pi/"]];   // everything in the release, under lib/pi
const LAUNCHER = `#!/bin/sh
set -eu
tool_root=$(CDPATH= cd -- "$(dirname -- "$(readlink -f -- "$0")")/.." && pwd)
export PI_PACKAGE_DIR="$tool_root/lib/pi"
exec "$PI_PACKAGE_DIR/pi" "$@"
`;

fn build(src, out) {
    let launcher = join_path(out, "bin/pi");
    mkdir(dirname(launcher));
    write_file(launcher, LAUNCHER);
    chmod(launcher, 0o755);
}
```

### 9.3 A compiled program

Section 3.1 shows `build`; the declarations are those of 9.1 with
`SOURCES = #{ source: #{ url: "...", sha256: "..." } }`, no `FILES`, and the
compilers it needs in `build_deps` or the build image.

### 9.4 A package group (`network`, shape c)

```rhai
const RECIPE_API = 1;
const NEEDS = #{
    outputs: #{ system: "directory" },
    network: ["dl.fedoraproject.org:443", "dl-cdn.alpinelinux.org:443"],
    memory_mib: 4096, storage_max_objects: 262144, timeout_secs: 7200,
};
const ABOUT = #{
    kind: "system", category: "utilities", libc: ["glibc", "musl"], systems: ["levitateos", "acornos"],
    details: "NetworkManager and its WiFi support over wpa_supplicant; nmcli comes with it.",
    homepage: "https://networkmanager.dev", license: "GPL-2.0-or-later AND LGPL-2.1-or-later AND BSD-3-Clause",
    keeps: [],
};
const PACKAGE = #{ name: "network", description: "NetworkManager, with WiFi" };   // version: network.pins
const PACKAGES = #{
    glibc: ["NetworkManager", "NetworkManager-wifi", "wpa_supplicant"],
    musl:  ["networkmanager", "networkmanager-cli", "networkmanager-wifi", "wpa_supplicant"],
};
const ACCOUNTS = #{ musl: ["g plugdev -"] };
let deps = ["desktop-libs"];

fn configure(tree) {
    enable_service(tree, "NetworkManager.service", "networkmanager");
}
```

### 9.5 A group with a second, non-package source (`steam`)

```rhai
const RECIPE_API = 1;
const NEEDS = #{
    outputs: #{ system: "directory" },
    network: ["dl.fedoraproject.org:443", "repo.steampowered.com:443"],
    network_max_bytes: 20971520,           // Valve's tarball; the pins' bytes are added by Recipe
    memory_mib: 4096, storage_max_objects: 262144, timeout_secs: 7200,
};
const PACKAGE = #{ name: "steam", description: "Valve's Steam launcher and what it needs" };
const PACKAGES = #{ glibc: ["zenity", "xz"] };
const SOURCES = #{
    valve: #{ url: "https://repo.steampowered.com/steam/archive/stable/steam_1.0.0.87.tar.gz",
              sha256: "649375d2f9377f8009aaf3e2ff09978041eb9114897d9c5a3886f1d82e27ba1f", libc: "glibc" },
};
let deps = ["desktop-libs", "lib32", "graphics-drivers"];
// From Valve's tarball into the tree (its top folder steam-launcher/ is left out).
const FILES = [
    ["valve/bootstraplinux_ubuntu12_32.tar.xz", "usr/lib/steam/bootstraplinux_ubuntu12_32.tar.xz"],
    ["valve/steam.desktop",                     "usr/share/applications/steam.desktop"],
    ["valve/icons/48/steam.png",                "usr/share/icons/hicolor/48x48/apps/steam.png"],
    ["valve/icons/48/steam.png",                "usr/share/pixmaps/steam.png"],
    ["valve/subprojects/steam-devices/60-steam-input.rules", "usr/lib/udev/rules.d/60-steam-input.rules"],
    // ... the rest of today's VALVE_FILES, without the per-file digests
];
const NOFILE_CONF = "[Manager]\nDefaultLimitNOFILE=1024:1048576\n";

fn configure(tree, src) {
    // Valve's launcher, without the block that copies steam.desktop onto the desktop.
    let text = read_file(join_path(src.valve, "bin_steam.sh"));
    let from = text.index_of("\t# put the Steam icon on the user's desktop\n");
    let to = text.index_of("\t# Restore the umask\n");
    if from < 0 || to < from { throw "Valve's bin_steam.sh changed; compare it with RPM Fusion's steam-no-icon-on-desktop.patch"; }
    put(tree, "usr/lib/steam/bin_steam.sh", text.sub_string(0, from) + text.sub_string(to), 0o755);
    put_link(tree, "usr/bin/steam", "/usr/lib/steam/bin_steam.sh");
    put(tree, "usr/lib/systemd/system.conf.d/01-steam.conf", NOFILE_CONF, 0o644);
    put(tree, "usr/lib/systemd/user.conf.d/01-steam.conf", NOFILE_CONF, 0o644);
}
```

### 9.6 Packages with an own build (`desktop-runtime`, sketch)

```rhai
const PACKAGES = ["glibc", "glibc-common", "gtk3", "nss" /* ... the seeds */];   // any C library
const NEEDS = #{ outputs: #{ sdk: "directory" }, network: ["dl.fedoraproject.org:443"], memory_mib: 4096 };

fn build(src, out) {
    let unpacked = join_path(WORK, "unpacked");
    for pin in src.packages { target_extract_rpm(pin.path, unpacked); }   // `package` is a reserved word in Rhai
    // ... keep the shared libraries flat in out/lib, compile the schemas, check the closure
}
```

### 9.7 A file output from one plain file (`busybox-static`)

```rhai
const RECIPE_API = 1;
const NEEDS = #{ outputs: #{ busybox: "file" }, network: ["busybox.net:443"] };
const PACKAGE = #{ name: "busybox-static", version: "1.35.0", description: "Static busybox for the initramfs" };
const SOURCES = #{
    busybox: #{ url: "https://busybox.net/downloads/binaries/1.35.0-x86_64-linux-musl/busybox",
                sha256: "6e123e7f3202a8c1e9b1f94d8941580a25135382b99e8d3e34fb858bba311348" },
};
const FILES = [["busybox", "", 0o755]];   // the source file becomes the output file
```

### 9.8 An OS-building recipe with inputs, defines and two outputs (`install-tools`)

```rhai
const RECIPE_API = 1;
const NEEDS = #{
    inputs: #{ source: "directory", program: "file?", catalog: "directory?", launcher: "file?",
               runtime_image: "file?", runtime_reference: "file?", dist: "directory?", keys: "directory?" },
    defines: #{ BIN: "recstrap", INSTALL_PATH: "", INIT: "" },
    outputs: #{ program: "file", rootfs: "directory" },
    memory_mib: 8192,
    storage_max_bytes: 6442450944,
};
const PACKAGE = #{ name: "install-tools", description: "One install tool as a static musl program, and the tree it installs" };
const RUST_TARGET = "x86_64-unknown-linux-musl";

fn build(src, out) {
    let bin = DEFINES.BIN;
    if INPUTS.program != () {
        copy_file(INPUTS.program, out.program);        // an input is read-only: copied
    } else {
        mv(compile(bin), out.program);
    }
    chmod(out.program, 0o755);
    let path = if DEFINES.INSTALL_PATH == "" { "usr/bin/" + bin } else { DEFINES.INSTALL_PATH };
    let installed = join_path(out.rootfs, path);
    mkdir(dirname(installed));
    copy_file(out.program, installed);
    chmod(installed, 0o755);
    if bin == "recipe" { recipe_runtime(out); }
    if bin == "recupdate" { recupdate_files(out.rootfs, DEFINES.INIT); }
}

fn compile(bin) {
    exec_checked("cargo", ["build", "--release", "--locked", "--offline", "--bin", bin, "--target", RUST_TARGET,
        "--manifest-path", join_path(INPUTS.source, "Cargo.toml"), "--target-dir", join_path(WORK, "target")],
        #{ cwd: INPUTS.source, env: #{ CARGO_HOME: join_path(WORK, "cargo"), CARGO_BUILD_JOBS: JOBS.to_string() } });
    let built = join_path(WORK, "target/" + RUST_TARGET + "/release/" + bin);
    let version = trim(exec_output(built, ["--version"]));
    if !version.starts_with(bin + " ") { throw "unexpected " + bin + " program: " + version; }
    built
}

fn verify(out) {
    if !is_file(out.program) { throw "the program is missing"; }
}
// recipe_runtime(out) and recupdate_files(root, init) as today, without ctx.
```

### 9.9 A template child (`fedora-rpm-preseed` over `locked-preseed`)

The base, `locked-preseed.rhai`:

```rhai
const RECIPE_API = 1;
const NEEDS = #{};
// A child imports its lock and defines rows(): the lock's [name, id, URL, SHA256] rows.
// Each file goes into the output, named by its URL's last part; the builder's
// content-addressed store (the optional `store` input) is used before the network.
fn build(src, out) {
    let rows = rows();
    let done = 0;
    progress(0, rows.len(), "files");
    for row in rows {
        let file = join_path(out, basename(row[2]));
        let kept = if INPUTS.store == () { "" } else { join_path(INPUTS.store, row[3].to_lower()) };
        if kept != "" && is_file(kept) {
            copy_file(kept, file);
            verify_sha256(file, row[3]);
        } else {
            try { download(row[2], file, row[3]); }
            catch (err) { throw "downloading " + row[1] + " failed (" + err + "); the lock may need refreshing"; }
        }
        done += 1;
        progress(done, rows.len(), "files");
    }
}
```

The child, `fedora-rpm-preseed.rhai`:

```rhai
//! extends: locked-preseed.rhai
const RECIPE_API = 1;
const NEEDS = #{ inputs: #{ store: "directory?" }, outputs: #{ rpms: "directory" },
                 network: ["dl.fedoraproject.org:443", "kojipkgs.fedoraproject.org:443"], memory_mib: 8192 };
const PACKAGE = #{ name: "fedora-rpm-preseed", description: "The locked Fedora RPM set" };
import "sources/levitate-rpm-lock.rhai" as lock;
fn rows() { lock::packages() }
```

### 9.10 Run every time (`root-effects`)

```rhai
const RECIPE_API = 1;
const NEEDS = #{ inputs: #{ root: "directory" }, outputs: #{ system: "directory" }, memory_mib: 4096, storage_max_objects: 262144 };
const PACKAGE = #{ name: "root-effects", version: "1", description: "The whole-root caches of a LevitateOS or AcornOS root" };
const RERUN = true;
fn build(src, out) {
    // runs the root's own cache tools against INPUTS.root and keeps only what they generate in out;
    // Recipe finishes the system tree afterwards
}
```

## 10. The 18 proposed answers, checked

| # | Answer | Holds? | Recipes it touches |
|---|---|---|---|
| 1 | `INPUTS.name` | yes; an absent optional input is `()` | 25 OS recipes |
| 2 | `DEFINES.NAME` with defaults | yes, with where and how required (2.1) | `linux`, `linux-prebuilt`, `reclogin`, `install-tools`, `tool-crate`, `prebuilt-fetch`, `release-medium`; `xlibre`, `desktop-apps` move to `JOBS` |
| 3 | `extends` and `import` stay | yes, with a rule for declarations (2.12) | 10 children of 3 bases; 18 recipes import |
| 4 | `configure(tree)` + `PACKAGE_FORMAT`, `HOST_LIBC`, `OS` | `OS` duplicates answer 14; proposed module only | 46 groups; 7 read the OS facts |
| 5 | `SOURCES` with `PACKAGES`, `configure(tree, src)` | yes for `steam`, `xlibre` group; not for `desktop-runtime` and `lib32`, which need an own build (2.6) | `steam`, `xlibre`, `desktop-runtime`, `lib32` |
| 6 | `libc:` or `target:` | `libc` yes; `target` has no user that fits; proposed dropped (2.4) | `rec-apps`, `xlibre` group, `steam`; `linux`, `linux-prebuilt`, `bun-toolchain` download in `build` |
| 7 | `git:` with `commit:` | yes | OS `xlibre` |
| 8 | download inside `build` without `SOURCES` | yes, and it must also cover lock-module rows, the `store` input, a replacing input and a program that fetches (3.1) | 13 OS recipes |
| 9 | `RERUN = true` | yes | `root-effects` |
| 10 | single output named only in `NEEDS.outputs` | yes; several outputs need `out` as a map, and the kernel's overlapping outputs need one output (3.2) | 5 multi-output OS recipes |
| 11 | caller's tree as the `target` input | yes | `linux`, `linux-prebuilt`; six preseeds drop an unused `target` |
| 12 | `cleanup(reason)` without `ctx` | holds; stays as an optional function (3.7) | 7 OS recipes |
| 13 | checks report state, force options replaced | yes: `recipe state`, Recipe's own records, single steps (6.3) | builder `pipeline/kernel.rs`, `recipe/mod.rs`, `stamp.rs` |
| 14 | `look.rhai`, generated `os-facts.rhai` | yes | `konsole`, `qterminal`, `pcmanfm-qt`, `lxqt`, `plasma`, `firefox`, `login-screen` |
| 15 | `NAME.pins` | yes, with the form, and with version and network budget moved out of the recipe text (2.7) | 46 groups and `desktop-runtime` |
| 16 | keep option, storage kinds | yes (6.4, 8.4) | engine, app |
| 17 | single-step commands, states | yes (6) | engine, app |
| 18 | `UPSTREAM`, `latest()`, `updates --json` | yes, with `prefix` and the network rule (2.9) | catalog recipes from releases |

Also found:

- Functions called by functions do not see top-level constants under
  today's engine (section 1); `steam` and the `xlibre` group look broken by it.
- The builder reads `ctx.kernel_version` from the kernel result; it can use
  its own pin (8.6).
- No recipe uses `save`, `saved` or `checkpoint` after the rewrite (7).
- Seven OS recipes compute `PACKAGE` fields; one computes its name (2.3).

## 11. Decisions taken while the owner was away

Taken 2026-11-16 under the owner's standing go, with the recommended answers.
The owner may reverse any of them. Decision 14 differs from the first draft:
it follows the plan's answer 12.

1. **Where are defines declared and how is one required?** In
   `NEEDS.defines` with defaults, `()` for required; a define reaches every
   recipe of the graph that declares it, an undeclared one is refused,
   `RECIPE_*` is reserved. Why: `NEEDS` already holds everything a caller may
   bind, and required defines exist (`release-medium`).
2. **What are the keys of `ABOUT.steps`?** `download`, `build`, `install`.
   Why: the hooks they were named after are gone.
3. **`PACKAGE.name` equals the file name,** and may be left out. Why:
   receipts, `update` and sources find recipes by file name.
4. **A single top folder is left out when an archive is unpacked,** unless
   `strip: false`. Why: it keeps the version out of every `FILES` path, as the
   note's own example does.
5. **There is no `target:` source field,** only `libc`; `linux`,
   `linux-prebuilt` and `bun-toolchain` download inside `build`. Why: none of
   them fits a declared source.
6. **`FILES` names a lone plain-file source and a `file` output** by its
   source name; `to: ""` makes the output file; `FILES` only in single-output
   recipes. Why: `busybox-static`, `claude-code` and `muse` need it.
7. **A recipe may have both `FILES` and `build`,** `FILES` first. Why: `pi`
   and `antigravity` place a folder and then work on it.
8. **A recipe with `PACKAGES` does its own unpacking** with `build(src, out)`
   and `src.packages`; a plain list means any C library; `glibc_i686` holds
   32-bit seeds. Why: `desktop-runtime` and `lib32` need the pins but not the
   group steps.
9. **A group's version and network budget come from the pins file's
   `version` line;** Recipe adds the pins' bytes to `network_max_bytes`. Why:
   the generator no longer edits recipe text.
10. **The pins file is `NAME.pins` in the form of 2.7.** Why: one line per
    package, sections per C library, nothing the engine has to guess.
11. **Declarations combine whole:** from the child, else whole from the base;
    `NEEDS` in the child. Why: no template needs merging, and merging hides
    what a child may use.
12. **There is only the `os-facts.rhai` module,** read with
    `os_facts::of(PACKAGE_FORMAT)`, and no `OS` constant. Why: answers 4 and
    14 describe the same facts twice, and Recipe should not carry one
    distribution's colours.
13. **The kernel recipes have one output, `bundle`;** the builder checks the
    kernel and modules inside. Why: the other two sit inside the bundle at a
    release-dependent path.
14. **`cleanup(reason)` stays** as an optional recipe function without `ctx`,
    called with the reasons `manual` and `auto.<phase>.<success|failure>`.
    Why: the plan's answer 12 keeps it, no recipe is required to define it,
    and the engine's own deletion of material does not depend on it.
15. **`PREFIX`, `JOBS`, `HOST_LIBC`, `PACKAGE_FORMAT`, `INPUTS`, `DEFINES`,
    `DEPS` and `WORK` are the whole list of constants.** Why: every other
    `RUNTIME` field loses its last user in the rewrite.
16. **`DEPS` of a single-output dependency is the path itself;** a map only
    for multi-output dependencies. Why: a normal recipe's output is never
    named.
17. **`put`, `put_link`, `tree_text` and `enable_service` move into the
    engine,** with the tree as first argument. Why: they are part of the group
    code copied into 45 recipes.
18. **The program helpers keep their names (`exec_checked`, ...).** Why:
    renaming touches every recipe for nothing.
19. **`UPSTREAM.github` is asked by Recipe itself** without `NEEDS`;
    `latest()` runs alone on the host within `NEEDS.network`. Why: a GitHub
    API grant would also reach the build.
20. **The three checks become `recipe state NAME`, and the force options
    become `recipe cleanup` before a run;** the builder checks the kernel
    bundle itself. Why: the state is Recipe's record now.
21. **`recipe remove` removes the program and all its material;** `--keep`
    removes only the program. Why: one action removes, and "only uninstall"
    stays possible.
22. **The keep option is undone with `--no-keep`;** `remove` forgets it;
    `cleanup` deletes kept material but leaves the setting. Why: the plan
    gives no way out.
23. **The disk rules apply to producer runs (`--no-activate`, the builder),**
    "installed" meaning "outputs published"; the builder may pass `--keep`.
    Why: the builder relies on its own stamps and published outputs only.
24. **A resumed `build` skips finished work with `reached(name)`.** Why:
    `save`, `saved` and `checkpoint` alone cannot say where a restarted build
    stopped.
25. **`installed --json` carries the "Update" mark,** the offline one;
    "Newer upstream" only from `updates --upstream`. Why: one call for the
    Installed page, and asking upstream stays a separate request.
26. **`recipe cleanup --logs` and `recipe runtime remove` remove logs and the
    build image.** Why: every kind needs a way to be removed, and these two
    have none today.

## 12. Clarifications from the build (2026-11-16)

Choices taken while building where the sections above were silent. They are part
of the format.

- `package` is a reserved word in Rhai: loop variables over `src.packages` are
  named `pin` (the example in 9.6 is corrected).
- `PACKAGES_SOURCE = "packages"` is the reserved source name for
  `src.packages`. With `PACKAGES` present, the packages count as a source for
  the "one source, name left out" rule in `FILES`.
- Network budget: with `PACKAGES` present the budget is 0 plus the pins' bytes;
  `network_max_bytes` is left out of plain groups.
- `UPSTREAM`: groups have none; only programs from upstream releases do.
- Archive endings are the listed formats and their short forms (`.tgz`, `.txz`,
  `.tbz2`, `.tzst`); `.tar` and `.apk` stay single files.
- A source host missing from `NEEDS.network` is an error.
- Declaration rules: `ACCOUNTS` only with a `system` output; `configure` only
  with `PACKAGES` and no `build`; `PACKAGES` without `build` needs exactly one
  directory output; a whole-folder `from` goes to a folder `to` without a mode;
  a `file` output takes exactly one `[from, ""]` entry; a pins file beside a
  recipe without `PACKAGES` is refused.
- The pins file is staged at the recipe's domain name with `.pins` (`main.pins`
  for the main recipe). The main recipe itself is staged as `main.rhai`; the
  worker reads it under the name the coordinator sends (the request's
  `recipe_name`, the real file name), so `PACKAGE.name` is checked against the
  real file name and events name the recipe, not `main`. A group whose `PACKAGES` lists are empty but whose pins
  file has a section for that C library is a shared group filled by the
  generator (`desktop-libs`). A C library with no seeds has its key left out of
  `PACKAGES`.
- A run-a-program check does not belong in `verify`; it moves into `build`.
  `verify` cannot check link targets (no read-link helper).
- A download's saved name allows letters, digits, `.`, `_`, `+` and `-`; a URL
  ending in anything else needs an explicit `file:`.
- Several whole folders named into one output merge; later entries do not
  replace the tree. `FILES` keeps symbolic links inside a whole folder.
  Whole-folder entries keep the archive's own file modes.
- A `FILES` list cannot name a different source per C library: a recipe that
  needs that reads `src.glibc` and `src.musl` in `build`. `ACCOUNTS` is a map
  from C library.
- `PACKAGE` must be literal, so a description that was computed from a lock now
  carries the release in text. Functions see the literal `PACKAGE` constant
  (`PACKAGE.version`).
- Top-level code also runs before `cleanup(reason)`. The `<phase>` in a cleanup
  reason is the worker's last step: `build` when the recipe defines `build()`,
  else `install`; `auto.install.*` appears only for recipes without `build`.
- With a single output, `out` is placed at `/out/NAME` and the `target` input
  seeds that folder; `target` with other output shapes is refused.
- `checkpoint` works only inside `build` and `configure`; `save` and `saved`
  anywhere. `latest` runs as a worker operation on the host. `verify`'s
  read-only rule is not enforced by the engine.
- Visibility of the engine's constants is a variable resolver, not a module
  (Rhai's optimizer would inline module variables over same-named parameters).
- `recipe installed --json` carries no `files`; programs and menu entries come
  from `ABOUT.adds`. `recipe list --json` carries each recipe's `sources`.
- The `step` event has a fixed set of statuses: `running`, `done`, `already`,
  `none`, `skipped`, `failed`; the `failed` event's field is `step`. The engine
  emits no `output` or `say` event (the app's fixtures do). `recipe updates
  --upstream` takes recipe names as plain arguments.
- State and disk figures for programs not installed come from the computer's
  store; a person's own store may differ. A kept program's material counts under
  `kept`; an unfinished run's only under `unfinished`.
- The `--system` scope has `state` only; it has no `download`, `build` or
  `cleanup`.
- A cleanup result (8.6) carries `outputs: {}`. The builder passes the same
  `--define` and `--input` flags to `cleanup` as to install, so recipes with
  required ones load.
- A group whose `PACKAGES` lists are empty but whose pins file has a section for
  that C library (`desktop-libs`) is a shared group: the generator fills it, and
  a pins section is refused only for a C library whose key is not in `PACKAGES`.
- A `FILES` entry that names a source of another C library (not fetched on this
  system) is skipped together with its source.
- `src.packages[i].var` is `keep_var` (`var` is a reserved word in Rhai); the
  pins file column keeps its name.
- Each program keeps its own downloads; there is no download store shared
  between programs. `recipe download` keeps them in the program's download slot,
  named by its state key (not by the full identity of a build, which also names
  the dependencies' outputs), so a build or install of the same key starts from
  them. They are kept beside a build's unfinished run or result: `recipe cleanup
  NAME --downloads` removes only the downloads, `--build` only the unfinished run
  and the result; the cleanup after an install and `recipe remove` remove both.
- The state key and the identity of a build name `HOST_LIBC`. A receipt records
  the state key it was built with; `recipe install` of an installed program finds
  nothing missing only when the recipe file, the version and the state key are
  the same.
- A `FILES` file or folder entry beside a folder entry of the same source
  renames it: it is not also at its old place inside the folder. Only a path two
  entries name exactly is copied for the later one.
- A `RERUN` recipe's successful run leaves no checkpoint to be resumed.
- `latest()` runs without inputs; an input or a define without a default that it
  is not given is `()` there.
- A producer's (`--no-activate`) run result says `installed`: its outputs are
  published (decision 23). `recipe state` reflects only receipts and kept
  material, so it says `none` for that producer afterwards.
- Published outputs no command took (an interrupted run's, or a producer's that
  its caller left in the state root) count under `unfinished` in `recipe
  storage`; plain `recipe cleanup` deletes them unless a running invocation holds
  them.
- A recipe whose `RECIPE_API` is any number but 1 is refused: "Recipe accepts
  only RECIPE_API = 1".
