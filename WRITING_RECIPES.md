# Writing Recipe recipes

A practical guide to the recipe format, API 1. The full contract, with every
rule and the reasons for it, is [docs/recipe-format.md](docs/recipe-format.md);
this guide shows how to write recipes and points there for the details. The
reference pages are `recipe-recipe(5)` (the format) and `recipe-helpers(7)` (the
helpers); [HELPERS_AUDIT.md](HELPERS_AUDIT.md) lists every registered helper.

A recipe is a Rhai file. It states facts in literal declarations and writes
code only for what is unusual. Recipe downloads, checks, unpacks, places files,
unpacks distribution packages, finishes system trees, records what is done,
installs, removes and cleans up. A recipe never says how to do those things; it
says what the program is, where it comes from and what goes where.

Every recipe starts with the API line and declares its authority:

```rhai
const RECIPE_API = 1;
const NEEDS = #{ outputs: #{ "package": "directory" }, network: ["github.com:443"] };
```

The engine accepts the number 1 and nothing else. There is no older form: the
hooks (`acquire`, `install`, `remove`, `is_acquired`, `is_built`,
`is_installed`), the `ctx` map and the `RUNTIME` constant do not exist, and a
recipe that defines one of those functions or declares `ctx` or `RUNTIME` is
refused with the line to change.

OS artifact producers and user-managed software use the same format and engine.
In the LevitateOS workspace they live in `distro/builder/os-recipes/` and
`recipes/`. Their consumer owns deployment: the builder composes OS images, the
CLI activates `package` outputs.

## The parts of a recipe

| Part | Form | Required |
| --- | --- | --- |
| `RECIPE_API` | `const`, the number 1 | yes |
| `NEEDS` | `const`, literal map | yes, also in every child of `extends` |
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

"Literal" means plain data: no helper calls, no variables, no arithmetic.
Recipe reads every declaration without running the recipe, so `recipe list`,
`info`, `index`, `validate`, the pin generator and the app can show them. A
recipe may define other functions and constants of its own; the names above are
reserved.

## A prebuilt program

Most catalog recipes are declarations only. This is all `ripgrep` needs:

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
];
```

`package` is a Rhai keyword, so the output key is quoted. Recipe downloads the
source into its download store (checked against `sha256`), unpacks it, moves the
listed files into the package folder, records every file's checksum in the
receipt and, on `install`, activates the package. `recipe install ripgrep` then
cleans up the downloads and the unpacked source unless you pass `--keep`.

## NEEDS

What the recipe may use. Ginux and the host confinement enforce it; callers can
only narrow it.

| Field | Meaning |
| --- | --- |
| `inputs` | name to `"file"`, `"directory"`, `"file?"` or `"directory?"` |
| `defines` | name to its default value; `()` means required |
| `outputs` | name to `"file"` or `"directory"` |
| `network` | `"host:port"` endpoints, redirect hosts included |
| `network_max_bytes` | bytes sent and received, default 8 GiB |
| `memory_mib` | default 2048 |
| `storage_max_objects` | default 16384 |
| `storage_max_bytes` | default bounded by memory |
| `timeout_secs` | default 3600, 1 to 86400 |

`outputs` holds exactly one name in a normal recipe. The name tells Recipe and
the caller what the output is: `package` (a package activated in a profile),
`system` (a tree written into a system root), `tools` (put on the `PATH` of a
consumer) or any other name (a product the caller picks up). The body never
uses the name. A few OS-building recipes declare several outputs (see
[Several outputs](#several-outputs)).

Every host of a `SOURCES` URL, and every redirect host, must be in `network`;
`recipe validate` and `recipe info` say which one is missing. An optional
input is `()` when it was not given; a supplied path must still have its kind.
`--network-host` selects a subset and resource flags select lower limits;
widening fails with the declaration field to edit. Read
[runtime limits](docs/runtime-limits.md) when choosing grants: memory is
per-process address space, CPU selection is real affinity, network authority is
numeric IP:port and archive timestamps have whole-second precision.

### Defines

Defines are genuine choices a caller makes (`KERNEL_TARGET`, `INIT`). A recipe
declares each define it reads, with its default, and reads it as
`DEFINES.NAME`:

```rhai
const NEEDS = #{ defines: #{ KERNEL_TARGET: "levitate", INIT: () } };
```

A default of `()` means required: a run without it fails before anything runs.
A define given with `--define NAME=VALUE` reaches every recipe of the graph that
declares it; a define no recipe of the graph declares is refused. Names starting
with `RECIPE_` are the engine's own and cannot be declared. A define grants no
host path.

## ABOUT and PACKAGE

`ABOUT` is what a front end shows, the author's claim, checked only for form:
`kind`, `category`, `libc`, `libc_carried`, `systems`, `details`, `homepage`,
`license`, `adds`, `steps` and `keeps`. The keys of `ABOUT.steps` are the step
names `download`, `build` and `install`.

`PACKAGE` holds `name`, `version` and `description`, all literal strings.
`name` equals the file name without `.rhai` and may be left out. `version` is
required for a recipe with a `package` or `system` output, because receipts and
updates compare it, except for a recipe with `PACKAGES`, whose version is the
`version` line of its pins file.

`ABOUT.libc` is a list from `"glibc"` and `"musl"`: the C libraries the build
works with (absent means any). Before `install` or `update` acquires anything,
Recipe compares it with `HOST_LIBC` for the recipe and its runtime `deps` (not
`build_deps`). On a difference it states the facts and the flag that installs
anyway, `--any-libc`, and exits nonzero; with `--json` it adds a record of kind
`libc_mismatch`. `ABOUT.libc_carried: true` says the package brings its own C
library and loader (see `carry_libraries`); the facts then say where it was
made and tried. `ABOUT.systems` is a list of os-release `ID`s (`["levitateos",
"acornos"]`); the recipe runs only where the root's `etc/os-release` has one of
them, and `--any-system` goes ahead anyway (JSON kind `system_mismatch`). For a
system change, "this system" is the root written into. Write the recipe's own
`details` and comments the same way: state the facts and the way forward.

## SOURCES

What Recipe downloads before anything else: a map from a source name to one
source. Source names are free; the convention is `release` for the main archive
or file and `licence*` for licence files.

| Field | Meaning |
| --- | --- |
| `url` | HTTPS address; its host is in `NEEDS.network` |
| `sha256` | 64 hex digits, required with `url` |
| `file` | the saved file's name; default the last part of the URL's path (needed when the URL ends in a query; allowed characters are letters, digits, `.`, `_`, `+`, `-`) |
| `unpack` | `false` keeps the download as one file even when it looks like an archive |
| `format` | `tar.gz`, `tar.xz`, `tar.bz2`, `tar.zst`, `zip` or `rpm`, when the file name does not say it |
| `strip` | `false` keeps a single top folder |
| `libc` | `"glibc"` or `"musl"`: only fetched on that C library |
| `git`, `commit` | a Git repository and a full commit id, instead of `url` and `sha256` |
| `tag` | for `git`: informational only |

The download goes into Recipe's download store, named by its SHA-256, and is
done when the file is there and matches. The offline cache (`RECIPE_CACHE_DIR`,
`/usr/share/recipe/cache`) is used first. An archive is unpacked into its own
folder (the endings `.tgz`, `.txz`, `.tbz2` and `.tzst` count; `.tar` and `.apk`
stay single files), an `rpm` source natively without scripts, a `git` source is
checked out at `commit`. In `build(src, out)`, `src.NAME` is the path of the
unpacked folder, of the single file or of the checkout.

When every entry of an archive sits under one top folder, `src.NAME` is that
folder and `FILES` paths leave it out; `strip: false` keeps it. An archive that
holds one file is unchanged. A `libc` source is fetched only when `HOST_LIBC`
matches; a recipe whose every source and pin is left out on this system has no
build there. There is no `target:` field: a recipe that chooses by a define or
an input's contents declares no `SOURCES` and downloads inside `build`.

## FILES

Which files of the sources go where in the output: a list of `[from, to]` or
`[from, to, mode]`.

```rhai
const FILES = [
    ["release/rg",          "bin/rg", 0o755],
    ["release/doc/rg.1",    "share/man/man1/rg.1"],
    ["docs/",               "share/doc/ripgrep/"],
];
```

- `from` starts with the source's name. With exactly one source the name is
  left out (`["rg", "bin/rg", 0o755]`); a source that is one plain file is
  always named by its source name.
- `from` ending in `/` is a whole folder; `"NAME/"` is the whole source, and with
  one source `""` is. `to` ending in `/` (or `""`, the output's top) is the
  folder it goes into. Several whole folders into one output merge.
- A single file without a mode gets 0644. Files of a whole folder keep the
  archive's own modes. Symbolic links inside a whole folder are kept.
  Setuid and setgid bits are refused in a `package` output. In a `system` output
  the mode goes into the tree's target metadata.
- Files are moved from the unpacked source into the output; a source file
  named by two entries is copied for the later one.
- In a recipe whose output is a `file`, the entry whose `to` is `""` makes the
  output itself (`["busybox", "", 0o755]`). `FILES` is only for recipes with one
  output.
- A `from` that does not exist fails the run, naming it.
- A recipe may have both `FILES` and `build`: `FILES` are placed first, then
  `build(src, out)` runs on the result.

There are no per-file checksum lists: the archive's checksum covers its
contents, and Recipe records every installed file's checksum in the receipt.

## build(src, out)

The recipe's own work: compiling, adjusting, writing. Optional; a recipe that
compiles nothing has no build step.

- `src`: a map from source name to path; `src.packages` with `PACKAGES`
  (see [Package groups](#package-groups)). An empty map without sources.
- `out`: the output. For a `directory` output the folder, which exists and holds
  the placed `FILES` (or a copy of the `target` input). For a `file` output the
  path where the file must be made; its folder exists. With several outputs,
  a map from output name to path.
- It returns nothing; a throw fails the run. It may replace the whole output
  folder (`rm(out); mv(dir, out)`).

```rhai
// pi: a whole folder is placed by FILES, build adds a launcher.
fn build(src, out) {
    let launcher = join_path(out, "bin/pi");
    mkdir(dirname(launcher));
    write_file(launcher, LAUNCHER);
    chmod(launcher, 0o755);
}
```

A compiled program is configured for `PREFIX`, where the package will be once
activated, and installed through a staging folder, because `out` is the package
folder itself (`make install DESTDIR=out` with `--prefix=PREFIX` would put the
files under `out/PREFIX/...`):

```rhai
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

A recipe that declares no `SOURCES` may download in `build` with `download`,
`download_exact`, `fetch_bounded`, `git_checkout` and their relatives, within
`NEEDS.network`. Recipe then has no download step for it; its downloads are
build material. Use `WORK` for anything that is not the result.

### What a function can see

- Its arguments.
- The engine's constants (below).
- The recipe's own top-level constants whose value is literal data, and its
  declarations (`PACKAGE`, `SOURCES`, ...).
- Imported modules through their alias (`lock::packages()`).

This holds for the functions the engine calls, for every function they call, for
the base of `extends` and for imported modules. A value computed at the top level
(`let x = f(...)`, `const X = f(...)`) is not visible inside a function: compute
it inside the function. Statements outside functions, other than declarations,
run once per run before the first function, only for steps that run recipe code
(build, configure, verify, latest), never for `list`, `info` or `download`. A
throw there fails the run.

### The engine's constants

All are read-only and visible everywhere.

| Constant | Value |
| --- | --- |
| `PREFIX` | where the `package` output will live once activated (`PROFILE/.packages/NAME`); `()` with `--no-activate`, for a dependency and for other outputs; part of the identity |
| `JOBS` | how many jobs the user asked for, 1 to 16 (`"-j" + JOBS`; `JOBS.to_string()` for an environment value) |
| `HOST_LIBC` | `"glibc"`, `"musl"` or `"unknown"`: the C library of the system the result is installed on (for a system change, the root written into) |
| `PACKAGE_FORMAT` | `"rpm"` or `"apk"`, the format of the pins in use; `()` without `PACKAGES` |
| `INPUTS` | map from each declared input to its read-only path; `()` for an optional input not given |
| `DEFINES` | map from each declared define to its value (given, else the default) |
| `DEPS` | map from each dependency name to the path of its output; for a multi-output dependency a map from output name to path |
| `WORK` | a private folder for build material, deleted after a successful run unless kept |

`DEPS["desktop-runtime"]` is a single path; `DEPS["bun-toolchain"].compiler` is
one of a multi-output dependency's. The engine has no `OS` constant: a recipe
that needs the distribution's facts imports `os-facts.rhai` and reads
`os_facts::of(PACKAGE_FORMAT)` (`rpm`: LevitateOS, `apk`: AcornOS).

## Package groups

`PACKAGES` lists the distribution packages a group is made of: the seeds the pin
generator starts from. A map from C library to seeds (`glibc`: Fedora RPMs,
`musl`: Alpine APKs; a library left out or empty has no build there), or a plain
list for the same packages on every system. `glibc_i686` holds 32-bit seeds
(glibc only).

```rhai
const PACKAGES = #{
    glibc: ["NetworkManager", "NetworkManager-wifi", "wpa_supplicant"],
    musl:  ["networkmanager", "networkmanager-cli", "networkmanager-wifi", "wpa_supplicant"],
};
const ACCOUNTS = #{ musl: ["g plugdev -"] };

fn configure(tree) {
    enable_service(tree, "NetworkManager.service", "networkmanager");
}
```

Recipe reads only the pins file `NAME.pins` beside the recipe (written by the
generator, never by Recipe, never by hand); it counts toward the recipe's
identity. Its form is in [docs/recipe-format.md](docs/recipe-format.md), section
2.7. A group's version is the pins file's `version` line; Recipe adds the pins'
bytes to `NEEDS.network_max_bytes`, so the recipe declares only the budget for
its other sources. For a recipe with `PACKAGES` and no `build`, Recipe:

1. downloads every pin for this system (offline cache first);
2. unpacks every package into the output tree, natively and without scripts,
   packages whose `var/` is not kept first, then deleting `var/`, then the
   packages whose `var/` is kept as state seeds;
3. places `FILES`;
4. calls `configure(tree)` or `configure(tree, src)` (`src` is the other
   `SOURCES`; `steam` takes Valve's tarball, the `xlibre` group its prebuilt
   server);
5. finishes the tree.

`configure` writes the group's own configuration into the tree with the tree
helpers, and exists only with `PACKAGES` and no `build`. It may read `src` but
the pins stay Recipe's.

A recipe with `PACKAGES` and its own `build(src, out)` (`desktop-runtime`,
`lib32`) does its own unpacking: Recipe still downloads the pins, skips steps 2
to 4 and gives `src.packages`, a list in pin order of `#{ name, path, sha256,
bytes, var, licence }`; the recipe unpacks with `target_extract_rpm` or
`target_extract_apk`, and a `system` output is still finished by Recipe.
`packages` is then a reserved source name (written `src.packages`; in a `for`
loop name the variable `pin`, because `package` is a reserved word in Rhai).

### System trees

A `system` directory output is a root-relative tree (`etc/...`, `usr/...`) that
`recipe --system` writes into a root with a receipt, so it can be removed again.
Recipe finishes every `system` output after `configure` or `build`: it removes
the folders a system package does not own (`run`, `tmp`, `boot`, `home`, `root`,
`proc`, `sys`, `dev`, `lost+found`, `var/run`, `var/tmp`, `var/lock`) and
finalizes the tree with owners resolved in the root it is written into, `var/`
as state seeds (created in the system's `/var` where missing, never overwritten,
owned or removed) and `ACCOUNTS`. A recipe does not call `target_finalize` on a
`system` output. Trees for images (`rootfs`, `overlay`, `medium`) are not
`system` outputs; their recipes finalize them.

`ACCOUNTS` is a map from C library to sysusers.d lines (`u`, `g`, `m`) for
accounts a tree needs that its packages' own `usr/lib/sysusers.d` does not make
(Alpine packages make theirs in scripts, which never run). It is only valid with
a `system` output. Accounts are made as the OS builder makes them: `g` lines,
then `u` lines, an existing account kept, `-` the highest free id below 1000,
`u NAME -` takes the group NAME when it exists, `m USER GROUP` adds USER to
GROUP once. An owner no account resolves fails the change naming the line to
declare.

### Tree helpers

| Helper | Does |
| --- | --- |
| `put(tree, path, text, mode)` | writes `text` at `path` in the tree, replacing what is there, with `mode` in the target metadata |
| `put_link(tree, path, target)` | a symbolic link at `path` to `target` |
| `tree_text(tree, path)` | the text of a packaged file; fails naming the file when the packages no longer ship it |
| `enable_service(tree, unit, service)` | on an `rpm` tree the links `systemctl enable` makes for the systemd `unit`, with its `Alias=` names; on an `apk` tree the OpenRC `service` in the default runlevel; an empty name skips that system |

## verify(out)

An optional check of the result, after `build` or `configure` and before
installing. It only reads (files and their contents) and starts no program. A
throw fails the run. It is not run again later: "installed" is Recipe's record.

```rhai
fn verify(out) {
    if !is_file(join_path(out, "bin/tool")) { throw "the package has no bin/tool"; }
}
```

A check that must run a program (`tool --version`) belongs in `build`. There is
no helper that reads a link's target, so `verify` cannot check one.

## Steps, states and saved values

Recipe has four steps. `download` fetches `SOURCES` and the pins into the
download store. `build` unpacks the sources and packages, places `FILES`, runs
`build` or `configure`, finishes a `system` tree, runs `verify` and keeps the
result. `install` activates a `package` output, writes a `system` output into
the root or publishes a producer's outputs. `cleanup` deletes the program's
downloads, unpacked sources, build material and saved state. `install` does
every step still missing, in order, then cleanup unless you chose `--keep`; each
step can be run alone. A recipe without `SOURCES` and `PACKAGES` has no
download step of its own; a recipe without `build` shows no build step.

The state of a program is `none`, `downloaded`, `built` or `installed`. Recipe
decides it from its own records (a download by its SHA-256, a result by its
identity and recorded checksums, an installed program by its receipt); no recipe
code runs to decide it, and `recipe state NAME` prints it. After a successful
install the downloads, unpacked source, `WORK` and saved state are deleted;
files are moved into place, not copied, where both places are on one disk.

An interrupted `build` resumes from the newest checkpoint: Recipe restores it
and calls `build(src, out)` again from the top. Four helpers:

| Helper | Does |
| --- | --- |
| `save(name, value)` | keeps a JSON-compatible value (map, array, string, boolean, `()`, finite number) under `name` |
| `saved(name)` | the value saved under `name`, or `()` |
| `checkpoint(name)` | records the work folders, the sources, the output and every saved value as one checkpoint; only inside `build` and `configure` |
| `reached(name)` | `true` when this run was restored from checkpoint `name` or a later one |

```rhai
fn build(src, out) {
    let dir = src.source;
    if !reached("configured") {
        exec_checked("./configure", ["--prefix=" + PREFIX], #{ cwd: dir });
        checkpoint("configured");
    }
    exec_checked("make", ["-j" + JOBS], #{ cwd: dir });
}
```

A value is saved only together with the files of its checkpoint, a saved value
never proves work is done (inspect the real files), and Recipe keeps one
checkpoint at most, deleting it when the install succeeds. `--no-persist-state`
turns checkpoints off. Most recipes need none of this: in one `build` function
what used to cross hooks is a local variable. They are for long builds.

A long step can report how far it is with `progress(done, total, what)` (or
`progress(done, total)`; `total` 0 means unknown). It is cheap in a loop and
shows with `--progress-file PATH`.

## Several outputs

A recipe that declares several outputs gets `out` as a map from output name to
path and uses each by name; `install-tools` (`program`, `rootfs`),
`bun-toolchain` (`compiler`, `runtime`) and `prebuilt-fetch` (`files`,
`manifest`) are the ones. The kernel recipes have one output, `bundle`. A
consumer reads `DEPS["bun-toolchain"].compiler`.

```rhai
fn build(src, out) {
    copy_file(INPUTS.program, out.program);    // an input is read-only: copied
    chmod(out.program, 0o755);
}
```

## Inputs and target trees

```sh
recipe install ./tool.rhai --input source=./offline-source
recipe build ./kernel.rhai --input target=./existing-bundle \
  --input kconfig=./kconfig --define KERNEL_TARGET=acorn
```

Inputs are read-only; `INPUTS.name` is the path. Use named inputs for source
trees, archives, configuration and executable inputs. Changes to imported
bytes, modes or links change the identity. The input named `target` is the
caller's existing tree and seeds the output before `build` runs, so a recipe can
add to it (with one output, `out` starts as a copy of `INPUTS.target`; with other
output shapes `target` is refused). A compiler-image binary under `/usr` is a
build tool, not evidence that something is installed. Rootfs absolute symlink
bytes are preserved without host traversal; ordinary source trees require
confined relative links.

## Host or Ginux

A recipe whose code cannot start a program runs on the host, without the root
image; everything else runs in Ginux. These helpers put a recipe in Ginux:
`exec`, `exec_checked`, `exec_output`, `shell*`, `git_checkout`,
`target_extract_tar`, `command_exists`, `rpm_installed`, `rpm_version`,
`dnf_*`, `apk_*`, `carry_link_flags` and `carry_libraries`. So do `eval`,
`Fn(name)` with a computed name and a dynamic `import`. The rule covers the
recipe, its `extends` base and every module it imports, called or not. `recipe
info NAME` shows where a recipe runs and, for Ginux, the first call that keeps
it there.

On the host, helpers reach only the run's inputs, work and output paths and
refuse any other path, `..` and links that lead out; build paths from `INPUTS`,
`WORK`, `out` and `tree`. Downloads reach only `NEEDS.network`, and a host name
there that resolves to a loopback, link-local, private, CGNAT or unique-local
address is refused unless the entry is that address itself. A recipe that
downloads, checks digests, unpacks and copies needs nothing else; the
declarations above cover it without any code.

## Verified acquisition inside build

Source facts belong in `SOURCES`; `build` downloads only when it cannot declare
them (rows from a lock module, a store input, a define-selected pin):

```rhai
download(url, join_path(WORK, "source.tar.xz"), sha256);
```

`download(url, exact_dest, sha256[, resume_bool])` requires a full 64-hex
SHA256, stages bytes, validates status, length and hash and replaces the
destination only after success. `verify_sha256`, `verify_sha512` and
`verify_blake3` validate existing files. `git_checkout(url, exact_dest,
full_commit)` needs a full 40/64-hex commit and a direct repository URL; Git
redirects are disabled. Declare grants as literal data; every endpoint,
including redirect destinations, belongs in `NEEDS.network`.

## Commands and extraction

`exec_checked(program, args[, options])` throws on nonzero exit,
`exec_output(...)` returns checked stdout, `exec(...)` returns a status you
handle. Arguments are an array; the optional map has child-only `cwd`, `env` and
integer `uid`/`gid`:

```rhai
exec_checked("cargo", ["build", "--release", "--locked", "--offline"], #{
    cwd: INPUTS.source,
    env: #{ CARGO_HOME: join_path(WORK, "cargo") },
});
```

Commands exist only in a Ginux run and need binaries in the selected image or
dependency tools. Use `shell` only when shell syntax is needed and quote data;
for pipelines use a checked `bash -o pipefail -c ...` or separately checked
steps. `extract(archive, dest)` and `extract_with_format` are checked archive
helpers; `target_extract_rpm`, `target_extract_apk` and `target_extract_tar`
extract into a target tree with its metadata (see
[docs/target-metadata.md](docs/target-metadata.md)).

## Dependencies and inheritance

```rhai
let deps = ["desktop-libs"];
let build_deps = ["desktop-runtime"];
```

`deps` are installed before the recipe and stay (runtime); `build_deps` are
there only while it builds. Each dependency runs separately under its own
`NEEDS`; no parent grant is lent. A dependency's outputs reach the consumer
through `DEPS`; a `tools` output is also put on the consumer's `PATH`. Bind a
dependency's input with `--input DEP::NAME=PATH`.

`//! extends: base.rhai` on the first line makes the file a child of a base
recipe. The base holds the shared code and the child completes it. A function
the child defines replaces the base's of the same name and arity, and a function
the base calls and only the child defines is the child's part of the template.
A declaration is taken whole from the child when it has it, else whole from the
base; maps are never merged field by field, and `NEEDS` must be in the child.
One level only. Templates in use: `locked-preseed` (the child imports its lock,
defines `rows()`), `package-effects` (the child defines `verify_inputs()`,
`unpack(root)` and `package_effects(root)`) and `rootfs-producer` (the child
sets `const INIT_SYSTEM = "systemd";`).

`import "path.rhai" as name;` has a literal path relative to the recipe's folder.
A module has no `RECIPE_API` and no `NEEDS`, and `recipe list` skips it. Its
functions see the engine's constants. Shared modules in the LevitateOS
workspace: `look.rhai` (palette), `os-facts.rhai` (generated), the lock modules
(`fn packages()` returning `[name, id, URL, SHA256]` rows). A remote source
carries the modules its recipes import.

## A root's effects recipe

An OS ships `/usr/lib/recipe/effects.rhai` in its base to rebuild whole-root
caches after Recipe changes system software in that root (package scripts never
run). After every successful system change Recipe runs it once against the root
it changed:

```rhai
const RECIPE_API = 1;
const NEEDS = #{ inputs: #{ root: "directory" }, outputs: #{ system: "directory" } };
const PACKAGE = #{ name: "root-effects", version: "1", description: "Caches of a root" };
const RERUN = true;
// INPUTS.root is the changed root, read-only. Write only the cache files
// under `out`; Recipe finishes the system tree afterwards.
fn build(src, out) { /* ... */ }
```

Its `system` output is written into the root as the reserved package
`system-effects`. `RERUN = true` means the recipe is run every time it is asked
for and never "built". A recipe that runs the root's own programs runs in Ginux.
A failure is reported and does not undo the change; `--no-effects` skips the run
(README, "Root effects").

## UPSTREAM and latest()

Where the newest version can be found. Recipe asks only when the user asks
(`recipe updates --upstream`), never by itself, and only tells; it never edits a
recipe or installs a version the recipe does not name.

```rhai
const UPSTREAM = #{ github: "sharkdp/bat", prefix: "v" };
```

`github` is `OWNER/REPO` (the newest release that is not a pre-release);
`prefix` is text to take off the tag (`"v"`, `"jq-"`; default none). For a
release page Recipe cannot read this way, define `latest()`, which returns the
newest version as a string:

```rhai
fn latest() {
    http_get("https://go.dev/VERSION?m=text").split("\n")[0].sub_string(2)
}
```

`UPSTREAM.github` is asked by Recipe itself over HTTPS, without `NEEDS`.
`latest()` runs alone on the host, may not start programs, sees no inputs and no
output, and reaches only `NEEDS.network`. A recipe has `UPSTREAM`, `latest()` or
neither; groups have neither (their versions follow the locks).

## cleanup(reason)

An optional function for something of the recipe's own to delete. Recipe
already deletes downloads, unpacked sources, `WORK` and saved state after a
successful install, so most recipes need none. It receives `reason`: `manual`
(what `recipe cleanup` passes) or `auto.<phase>.success` and
`auto.<phase>.failure`, where `<phase>` is the step that just ended: `download`,
`build` or `install` (the worker's last step is `build` when the recipe defines
`build()`, else `install`; `auto.install.*` appears only for recipes without
`build`). Top-level code runs before it. It runs once at the end of a run, after
the engine's own clean-up, cannot stop the engine deleting material, sees the
constants and helpers, has no `src` and no `out`, may delete files and start
programs, and must not delete installed files. A failure after a success fails
the command; after a failure it is reported next to the first error.

## Failure

A throw fails the run and is reported with the step and the line; what to do
about it is the user's. Recipe never asks an AI: no declaration, function,
helper, command or option calls a language model or starts an agent. Write a
failure message that gives the facts and the way forward. Cleanup errors after a
success fail the command. Source inputs and earlier publications stay immutable;
failed work is not published.

See the [README recovery section](README.md#state-diagnostics-and-recovery) for
log paths, retention, cancellation and timeouts.

## Checking a recipe

`recipe info FILE.rhai` reads a recipe in seconds without running it: its
declarations, where it runs, what it is missing (for example a source host not
in `NEEDS.network`). `recipe validate` also checks dependencies, input bindings
and compiled contracts. Extra rules the declarations check: `ACCOUNTS` only with
a `system` output; `configure` only with `PACKAGES` and no `build`; `PACKAGES`
without `build` needs exactly one directory output; a whole-folder `from` goes
to a folder `to` without a mode; a `file` output takes exactly one `[from, ""]`
entry; a pins file beside a recipe without `PACKAGES` is refused; a source host
missing from `NEEDS.network` is an error. A successful compile is source
evidence; a managed run proves only its observed image, recipe and workload.

Kernel producers default to the LevitateOS pin; builders select Acorn with
`KERNEL_TARGET=acorn`. Kernel rebuilds are excluded from release acceptance.
