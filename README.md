# Recipe

Recipe executes Rhai package recipes (the API 1 format, [`docs/recipe-format.md`](docs/recipe-format.md)).
A recipe mostly declares what a program is and where it comes from; Recipe
downloads, checks, unpacks, places files, installs, removes and cleans up. Code
a recipe does contain, if it can start a program, runs inside a configured Ginux
domain; if it cannot, as a confined host process (see "Execution modes"). Either
way the coordinator imports explicit immutable inputs, keeps the downloads and
finished results it records and publishes the recipe's named outputs, and no
recipe code runs unconfined. After a successful install Recipe deletes the
downloads, the unpacked source, the build material and the kept results, unless
you chose `--keep`.

## One engine, two consumers

In the LevitateOS workspace, `distro/builder/os-recipes/` holds OS artifact
producers, and the top-level `recipes/` directory is the user-managed software
catalog, starting with ripgrep. The builder selects its own collection
with `--recipes-path DIR` and `--no-activate`; normal CLI lookup uses an explicitly selected
catalog, or the user's XDG recipe directory and then the sources, of which the
image ships the official one in `/usr/share/recipe/sources`.
Both use this engine, the same recipe format (API 1, see
`WRITING_RECIPES.md`) and the same Ginux execution rules. Directory placement expresses ownership, not a new recipe type
or permission boundary. This repository's `examples/` and `tests/` are engine
examples and fixtures, not the application catalog.

The OS builder assembles published products into images; `recab` owns the two
roots of an A/B system (see "System software on A/B installations"). Apps bundled in a desktop image belong to that
OS lifecycle. For user software, the CLI activates a directory output named
`package` after managed execution and publication succeed. `--no-activate` keeps
product-only behavior (the builder's route, with the same disk rules; the builder
passes `--keep` where it wants material kept). `list` lists recipe declarations; `installed` lists active packages.

## System software on A/B installations

On an A/B system the booted root is locked and the other root is unlocked,
mounted read-write where `recab status --json` reports `other_mount`. System
software goes into the other root, never the booted one; Recipe does not write
into, remount or unlock the booted root. Root's install/update/remove commands
(and `--system`, for any user who may write the other root) do this:

```sh
sudo recipe --system install ripgrep   # written into the other root
sudo recipe --system installed         # the booted root's software, and the other root's
sudo recipe --system state ripgrep
sudo recab boot-other                  # start the other root at the next boot
sudo reboot
rg --version
```

The change is active when the other root is booted. If the other root is not
mounted, the command says so and stops; `recab copy` recreates it. Normal package
outputs install under the other root's `/usr/lib/recipe/profile`, with the
selection recorded in its `/usr/lib/recipe/system.json`. Explicit `system`
directory outputs carry root-relative intended metadata from `target_finalize`.
Dependencies are produced through the existing Recipe graph. Updates and removals
refuse conflicting ownership and administrator edits.

`recipe installed` and `state` say which software is in the running (booted)
root and which is in the other root and becomes active when that root is booted.
`update` resolves the current catalog first, then falls back to the recipe the
package was installed from. When the running root has software the other root's
receipt lacks, a change prints one line naming it; `recab copy` makes the other
root a copy of the running system. It is information only.

`recab copy` overwrites the other root, and with it the system changes Recipe
wrote there. Recipe keeps a record of those changes in `other-root.json` in its
package root (on the shared `/var`: `/var/lib/recipe`). Before any system-scope
command, it compares the record with both roots' receipts. A change that the
booted root now holds was booted into and is forgotten; a change neither root
holds was wiped, and Recipe writes exactly that system part into the other root
again (not the whole install, and never the "for me" software in a user's own
store), then ends as every system change does: the root's effects and library
cache, once, after all such parts (a failure is a warning; the parts stay
written). Until that has succeeded, the part is reported as not done.

A system without `recab.immutable` on its kernel command line has one root, the
running one, and no recab. `--system` there writes the change into that root with
its receipt (`/usr/lib/recipe/system.json`), with no A/B record and no call to
recab; `system` outputs (a desktop, say) install there like anywhere else. Without
`--system`, root's install there still uses its own package root, `/var/lib/recipe`.

## System software in a root that is not running

`--system --root DIR` writes a system change into DIR and reads `installed` and
`state` from it: an installer's target before the first boot, or an image
tree. It does what a change to the other root does, without recab and without the
`other-root.json` record; `--root` given the booted root of an A/B system is refused.

```sh
recipe --system --root /mnt install plasma printing firefox   # one run; each group written once
recipe --system --root /mnt installed
```

**Several recipes, each once.** A system-scope `install` takes any number of
recipes and installs them in order in one run. Before producing anything, Recipe
tells the producer what the root already holds; a recipe, or a dependency, that the
root's receipt holds at the same recipe digest and version is not produced or written
again, in the same run and across runs (with or without `--no-persist-state`). The
recipe digest names the recipe's content (the recipe, its pins, the base it extends,
the modules it imports, by their names beside the recipe), not the folder it lies
in: the install medium's copy and a release's copy of the same recipe match. The
JSON lists every package written (`packages`) and every one already there
(`present`). A runtime dependency's `system` output is written into the root, not
staged into its consumer's inputs. (`update` and `remove` take one recipe; an update
to what the root already holds writes nothing.)

**Root effects.** After a successful system change to a root, if the root ships
`/usr/lib/recipe/effects.rhai`, Recipe runs it once against that root and writes its
`system` output into the root as the reserved package `system-effects`, replacing that
package's previous files (they are caches other programs regenerate in place, so the
administrator-edit check does not apply to them). That recipe rebuilds whole-root caches (GSettings schemas,
GIO modules, pixbuf loaders, MIME and icon caches, fonts, desktop database, ...),
which package scripts would have rebuilt; the OS provides it. Its contract:

- it receives the root, read-only, as its input `root`
  (`NEEDS.inputs: #{ root: "directory" }`, as `INPUTS.root`; the system scope
  passes it with `--effects-root ROOT`), without the root's top-level folders
  `var`, `boot`, `home`, `root`, `tmp`, `run`, `proc`, `sys`, `dev`, `mnt`,
  `media`, `srv` and `lost+found`, and runs where its helpers put it (in Ginux
  when it runs the root's own programs). Every other top-level folder (`usr`,
  `etc`, and `lib`, `bin`, `sbin` where they are folders) is copied into the
  run as for any input (the public Ginux lifecycle has no read-only bind, so a
  whole root costs its size in disk and time on every run, with every file
  hashed, without files the secret-bearing guard refuses, such as key material
  and `etc/pki/tls/cert.pem`, and with the top-level files); top-level links
  (`lib -> usr/lib`) are kept as links. A file system mounted below one of
  those folders is not in it. The recipe cannot change the root: it only ever
  sees the copy, and runs as Ginux's unprivileged host user (uid 65534 on the
  host), so files the root keeps from other users (`etc/shadow`, host keys)
  cannot be read. The run is an ordinary run: it keeps its state unless
  `--no-persist-state` is given;
- it returns one `system` output (Recipe finishes the tree), holding only the
  cache files; it has no runtime `deps`; it declares `const RERUN = true;`;
- it runs the root's own tools, so it declares `NEEDS.runtime: "minimal"`: its
  Ginux root is unpacked from the minimal runtime image (a shell, coreutils,
  findutils, sed and grep, about 8 MB) instead of the 1.3 GB compiler image;
- `system-effects` is in the receipt but not listed by `installed`, cannot be
  removed by itself, and no recipe may take the name;
- a failed effects run is reported (JSON `effects: {"status": "failed", "error"}`,
  exit status nonzero, "the system change is written, but rebuilding the root's
  caches ... failed") and does not undo the change; the next change runs it again;
- `--no-effects` skips it, for a caller that runs it itself once at the end (an image
  builder installing many groups: install them with `--no-effects`, then run one
  last change without it). JSON `effects.status` is `done`, `none` (no effects
  recipe), `skipped`, `unchanged` (nothing was written) or `failed`.

**Library cache.** After the effects, Recipe rebuilds the root's glibc library cache
`/etc/ld.so.cache` itself, with or without `--no-effects`, because the cache must be
written inside the root: it runs the root's own `ldconfig` (`usr/sbin` or `usr/bin`,
found inside the root) as `ldconfig -X -i -r ROOT`, which writes the cache and
changes no library links. It runs only for a glibc root (one with
`/usr/lib64/ld-linux-x86-64.so.2`) that has `/etc/ld.so.conf`; a musl root (AcornOS)
is left alone. The cache is the machine's file: no package or base owns it. A failure
is reported like a failed effects run (JSON `effects.library_cache.status` `failed`,
exit status nonzero) and does not undo the change; the next change rebuilds it. The
other values are `done`, `none` (not such a root, or no `ldconfig`) and `unchanged`.

**System releases.** `recipe --system [--root DIR] apply-release FOLDER` applies a
fetched release (`docs/release-engineering.md` in LevitateOS; FOLDER holds
`release.json`, `base.tar.zst`, `recipes.tar.zst`, as `recupdate fetch` leaves them)
to the root a system change would write: the other root on A/B, the running root
without A/B, or DIR. It refuses a release of another `os` than the root's os-release
`ID` or with a lower `serial` than the root's `/usr/lib/levitate/release.json`, and
checks both archives' size and SHA-256 against the manifest before anything changes
(the signature is the caller's). Then:

1. The base tree becomes the reserved receipt entry `base`, through the same engine:
   files only the old base had are removed, changed ones replaced, configuration
   edited under `etc/` kept with the release's version beside it as `NAME.recipe-new`
   (and not even that when the release did not change the file). The machine's own
   files (`etc/fstab`, `crypttab`, `hostname`, `machine-id`, `localtime`, `adjtime`,
   `mtab`, `resolv.conf`, `subuid`, `subgid`, `ld.so.cache` and the account files) are never the
   base's, nor is a staged tree's target metadata (`.recipe-target-metadata.json`,
   bookkeeping like the receipt); an older base entry that recorded one of them
   forgets it and leaves the file as it is. The release's accounts the root lacks are created with the release's ids
   where free (the users made at installation stay); file owners are resolved by
   name. A file no package owns where the release now has one (made by the
   installation) is kept beside under `etc/` and replaced elsewhere. The manifest
   becomes the root's `/usr/lib/levitate/release.json`. On the running root the old
   base's modules of the running kernel stay, as the hidden package
   `previous-kernel` (at most one kernel's; the next apply removes them).
2. Every package the receipt holds that the release has a recipe for
   (`NAME.rhai`) is installed from it; one held from the same recipe is skipped,
   one the release lacks stays and is reported. The new base is in place by then
   (step 1 wrote it, or the root had it), so the recipes are the root's own
   `usr/lib/recipe/release-recipes/` (the same recipes as `recipes.tar.zst`): what
   the receipt records, and the release source of every later command. Only a base
   without that folder has `recipes.tar.zst` unpacked under
   `/var/lib/recipe/release/SERIAL/` for the apply, removed at its end.
3. The effects run once, and the library cache is rebuilt.

`base` is shown by `installed --json` with `version` and `serial`, not in the human
list; it cannot be installed, updated or removed. A root without `base` was installed
before releases and is refused (install the release from its ISO). Image builders
write `base` as the last step: `recipe --system --root TREE record-base --release
release.json`. Without A/B the step in progress is recorded in
`/var/lib/recipe/apply-release.json`, and running the same command again finishes an
interrupted apply. On A/B, waiting changes are written again first, and record
entries the release replaced are dropped. With `--json`, step lines
(`{"event":"step","n":2,"of":4,"text":...}`) and `{"event":"failed","error":...}`
precede the result object.

`RECIPE_TIMING=1` prints how long each stage of a system change and of each host
run takes. A system change's producer outputs are moved, not copied: out of the run
directory into publication, and from publication into the root, when the scratch
pool, the state directory and the root share a filesystem (the installer line
above puts all three on the target); each file's digest is still checked against
the recipe's record before it is moved, and the root is flushed once before its
receipt is written.

The producer runs under `--state-dir`/`--build-dir` (default
`/var/lib/recipe/system-producer` for root) and unpacks in the scratch pool
(`~/.cache/recipe/scratch`). On a live medium whose `/var` and home are in
memory, the installer runs exactly:

```sh
RECIPE_SCRATCH_DIR=/mnt/var/tmp/recipe-install recipe --system --root /mnt \
  --state-dir /mnt/var/tmp/recipe-install/state --build-dir /mnt/var/tmp/recipe-install/build \
  --no-persist-state install <the default set>
rm -rf /mnt/var/tmp/recipe-install
```

so scratch, producer state, logs and published outputs land on the target disk and
nothing is kept (the published outputs are deleted once written anyway).
The offline cache is found at `/usr/share/recipe/cache` on the medium without a
flag (`RECIPE_CACHE_DIR` names another). Nothing else in the environment is needed:
the target root decides what the recipes see of "this system" (below).

**The target decides.** For every system change, the root written into (the
`--root` directory, the other root of an A/B system, or the running root) is
"this system" for the recipe: Recipe hands it to the producer
(`RECIPE_HOST_LIBC_ROOT`), so `HOST_LIBC`, the `ABOUT.libc` check and the
`ABOUT.systems` check use that root's loaders and `etc/os-release`, not those of
the machine Recipe runs on.

Writing into any root (the other root, the running root, or `--root`):

- **Accounts.** File owners in a `system` output are names, resolved against the
  root's `etc/passwd` and `etc/group`. Accounts the change needs are created first,
  from its own `usr/lib/sysusers.d/*.conf` files and the `accounts` its recipe
  declares (`ACCOUNTS`), the way
  the OS builder creates them (a `u!` line's account fully locked, as
  systemd-sysusers makes it); the receipt records them with the package
  (`accounts`), and removing the package keeps them. Ownership is applied when
  Recipe runs as root.
- **Edited configuration.** A file under `etc/` changed since it was installed is
  kept and reported: removal leaves it, an update writes the new version beside it
  as `NAME.recipe-new` (JSON: `kept_config`, `new_config_beside`). Any other
  changed file still refuses the change. A `NAME.recipe-new` still as Recipe wrote
  it goes when its package is removed, or when an update no longer needs it.
- **Group membership.** Account lines may be `m USER GROUP` (sysusers.d): USER is
  added to GROUP's member list in `group` and `gshadow` (creating a missing user or
  group as `u USER -` / `g GROUP -`); already a member, nothing changes. The receipt
  records it as `{"kind": "member", "name": USER, "group": GROUP, "id": GID}`.
- **State seeds under `/var`.** Recipe finishes every `system` tree with `var/`
  kept as seeds: each directory, file or link
  there is created in the system's `/var` with its mode and owner only where
  nothing exists; it is never overwritten, never owned by the package and never
  removed (JSON: `var_seeded`). A seed below a link (`var/run -> ../run`) is left
  out. On an A/B system the seeds go to the running root's `/var`, the shared
  partition, not into the other root; at install time `/mnt/var` is that partition.
- **Tidiness.** Directories a removal leaves empty are removed; directories the root
  had before the install are never recorded or removed. Each change is flushed with
  one `syncfs` before the receipt is written.

## Offline cache

A read-only directory of pinned payloads, `CACHE/sha256/<hex>`, each named by the
SHA-256 of its bytes: `RECIPE_CACHE_DIR` (absolute; empty turns it off), else
`/usr/share/recipe/cache` when it exists (an install medium carries it).
`download` and `download_exact` take a payload whose pin they find there from it,
with no network; a cached file whose bytes do not match is ignored. Host runs read
it in place; a Ginux run gets copies of the payloads its recipe declares by pin
(a `download` whose digest the recipe names; one it computes at run time does not
find the cache). Both run as an unprivileged identity, so only files everyone may
read are used. The cache is
not an input and changes no saved-state identity. With a cache present, a `NEEDS.network` host that
cannot be resolved is left out with a warning rather than failing the run.

A download that fails the way a network does (a name lookup, a refused, reset or
timed-out connection, a body cut off, HTTP 408, 425, 429, 500, 502, 503, 504) is
tried again up to 4 attempts in all, after 1, 3 and 9 s, each retry printed with
its host, path and reason; a 404, a wrong checksum, a refused host or a size limit
fails at once. This holds for `download`, `download_exact`, the metadata fetches
and the download step of `SOURCES` and `PACKAGES`.
`--package-root DIR` retains an independent profile and `--no-activate` retains
output-only builder execution. Other commands, including catalog listing and
validation, keep their normal routes.

## Install on another Linux

Releases of `recipe-pm` start with 0.2.0 (the older 0.1.4 GitHub release from March 2026
is the old program). On any x86_64 Linux:

```sh
curl -fsSL https://levitateos.org/recipe/install.sh | sh
```

`scripts/web-install.sh` reads `/etc/os-release`, picks an RPM, `.deb`, APK or tarball,
downloads it from the newest release, checks its SHA-256 against `SHA256SUMS`, and installs it
with the system's own tool (root only for that step). Arch, Void, Gentoo, NixOS and
image-based systems (Silverblue, Bazzite, SteamOS, MicroOS) get the tarball, which installs
into `/usr/local`. `--dry-run` shows what it would do without a network request;
`RECIPE_INSTALL_TYPE` and `RECIPE_VERSION` override its choices. The six formats are described
in the superproject's `docs/recipe-standalone-release.md`.

## Independent user software

On a mutable system, or with an explicit independent package root (use `doas` on AcornOS):

```sh
sudo recipe install ripgrep
rg --version
sudo recipe installed
sudo recipe update ripgrep
sudo recipe remove ripgrep
```

`update` installs the version the recipe as fetched names, replacing the installed
one. It never edits a recipe and never installs a version the recipe does not
name. A reviewed newer recipe can be placed in the user's override catalog
independently of an OS image update. Explicit `--recipes-path` or `$RECIPE_PATH`
selects only that catalog; otherwise the XDG recipe directory overrides matching
shipped recipe filenames. `recipe updates --upstream [NAME...]` asks upstream
(each recipe's `UPSTREAM` or `latest()`) whether a newer version exists and only
tells; nothing is changed.

Each step also runs alone:

```sh
recipe download ripgrep     # sources into the download store
recipe build ripgrep        # download what is missing, then build; no install
recipe state ripgrep        # none, downloaded, built or installed
recipe install ripgrep --keep   # keep downloads and build material afterwards
recipe cleanup ripgrep      # delete its downloads and build material
recipe cleanup              # the same for every program
recipe cleanup --logs       # the diagnostic logs
recipe remove ripgrep       # the program and all its material (--keep: only the program)
recipe runtime remove       # the downloaded image layouts (not the imported image)
```

States are `none`, `downloaded` (all sources and pins for this system are in the
download store), `built` (a result with the current identity is kept) and
`installed` (the receipt lists it); Recipe decides them from its own records and
no recipe code runs. `--keep` is remembered per program; `--no-keep` turns it off
and deletes the material at once; `remove` forgets it; `cleanup` deletes kept
material but leaves the setting on. To redo a step, clean it up and run it again.
Downloads two programs name are deleted only when neither needs them, and a placed
file is moved from the unpacked source rather than copied where both are on one
disk. Nothing is hidden: `recipe storage` lists seven kinds (installed,
downloaded, built, kept, unfinished, build_image, logs), each with the command
that removes it (`unfinished` is published outputs no command took).

Packages publish regular `bin/COMMAND` and `share/PATH` files, with a name and
version. The coordinator copies them into an owned profile generation and commits
the files and installed receipt through one atomic symlink switch. File conflicts,
symlinks, special files and setuid/setgid payloads fail before switching. No
activation script executes on the host. Failed acquisition or activation preserves
the previous profile. The store keeps exactly one generation: an install, update or
remove deletes the generation it replaced once the new one is active (after the
switch and its directory sync), and the next change deletes any generation an
interrupted one left behind. There are no earlier versions and no go-back.

Outside the A/B system route, root's package root is `/var/lib/recipe`; an ordinary user's is
`$XDG_DATA_HOME/recipe` (normally `~/.local/share/recipe`). `--package-root PATH`
selects another store. Its `profile/bin` contains active commands; installed login
profiles add the booted root's system path and ordinary user paths to PATH. On a host without an A/B system, software installed for this computer (`/var/lib/recipe/profile/bin`) is on every user's PATH, and a user's own store comes first. `remove` uses the installed receipt
without the recipe or runtime. `installed` only reads, so an ordinary user may
also read a store root owns (`recipe installed --package-root /var/lib/recipe`);
changing a store still requires owning it. Removed packages disappear from the
profile and their files are deleted, with their downloads and build material
unless `remove --keep`.

For a front end, all read-only and without Ginux: `recipe installed --json`
adds to each package what its recipe was allowed to use when it was installed
(`needs`), when (`installed_on`) and the length of its files (`size_bytes`).
`recipe installed --json` also gives each package its `keep` option, what it
holds on disk beside its files (`on_disk`: downloads, build) and an
`update` mark (`null`, or the version and why, known offline). `recipe updates` says which installed
packages `recipe update` would change, from the receipts, the collection and
the sources as last fetched, and lists under `changes` every NEEDS field in
which the newer recipe differs from what was recorded (`added` and `raised`
mean it may use more; `null` when the receipt predates the record).
`recipe storage` reports everything Recipe holds, by kind.
`recipe history [--json]` lists what was done to the store,
newest first (install, update, remove),
from `history.jsonl` in the package root: one JSON object per line with `at`
(seconds since 1970), `what`, `package`, `from`, `to` and `source`, null where
one does not apply. It is written under the store's lock after each change; a
failure to write it is reported on stderr and does not undo the change. It keeps
the newest entries (about 256 KiB when it passes 512 KiB). `--json` (and piped
output) prints `{"schema_version": 1, "history": [...]}`.
With `--machine-events`, a failed
install used to carry `"refused": {"endpoint", "declared"}` when Ginux's log of
that run recorded a denied connection. The public lifecycle's audit lines do
not name the refused address, so `refused` is not reported any more. The
history command was run on a hand-made store; the record of `needs` at install
has compiled and not run, since an install needs Ginux.

The OS builder packages the public catalog, the `ginux` CLI
(`/usr/libexec/recipe/ginux`), the Ginux helper, image tool, runtime and
allocator (`/usr/libexec/ginux/{ginux-helper,ginux-image,ginux-runtime}`,
`/usr/lib64/ginux/libginux_allocator.so`), Recipe's guard
(`/usr/libexec/recipe/recipe-ginux-guard`), the sudoers rule
(`/etc/sudoers.d/recipe-ginux`, which names the guard), and
`/usr/share/recipe/runtime/{compiler.oci.tar,root-reference,minimal.oci.tar,minimal-reference}`
(the compiler image and the minimal image, as OCI image layouts). These are
runtime defaults; explicit `RECIPE_*` selections still take precedence. The
import into Ginux verifies every blob and the manifest digest must match the
reference before anything runs. Existing images must be rebuilt to carry these
files; a source change alone does not update a running installation. Running
anything in Ginux needs root or the sudoers rule, as described under
[Privileges](#privileges).

## Shared execution contract

The executable interface is API 1 (`const RECIPE_API = 1;`) with a literal `const NEEDS = #{...};`.
Recipes own their typed inputs/outputs, endpoint grants and resource limits.
Callers bind declared inputs and may explicitly narrow authority. A caller cannot
add an input, endpoint or larger limit that the named recipe did not declare.
Restrictions must first be valid lowerings/subsets for the named recipe. They
also cover every dependency: limits take the stricter bound, and endpoint sets
intersect each dependency's own declaration. Limits remain per instance.

Ginux runs each recipe as an instance on its public lifecycle (`create`,
`inject`, `start --wait`, `status`, `extract`, `delete`) with upstream's native
configured runtime and contract catalog 2. Recipe uses the upstream
`[contract]` and `[contract.*]` sections and validates the complete derived
contract (`ginux validate`, its digest checked against the pin) before the
instance is created. What the public lifecycle does not give, compared with the
retired downstream `launch`, is stated plainly in
[supported runtime limits](docs/runtime-limits.md): **no storage limit inside
a run** (only the size of the outputs taken out is bounded), **network grants
are per port** on the shared host network (a program can reach any address on a
granted port, and nothing caps its connections or bytes; only Recipe's own
downloads are held to the declared `HOST:PORT` and the byte cap), and live
system roots are copied, not bound.

The state of the migration (what is written, what has been run) is in
[the migration plan](docs/ginux-public-lifecycle-migration.md), section 6.

## Execution modes

Each recipe of a run goes one of two ways, chosen from its source before
anything runs:

- **On the host.** When no code in the recipe, the base it `extends` or a
  module it imports can call a helper that starts a program or reads the root
  image, the recipe runs as an ordinary process of the invoking user (when
  Recipe runs as root, under a uid/gid of its own from 0x70000000 up, with no
  groups and no capabilities; its directories are then under
  `/var/lib/recipe/host-runs`). It needs
  no Ginux, no image, no sudo rule and no particular kernel.
  Recipe confines it: every file helper resolves its path beneath the run's
  own directories (inputs read-only, work and out read-write; no `..`) and
  refuses anything else; the HTTP client reaches only the `NEEDS.network`
  host:port pairs, at the addresses Recipe pinned (never a loopback or
  private address a host name resolved to), within the run's network byte cap
  (`NEEDS.network_max_bytes` with Recipe's allowance, docs/recipe-format.md 2.1); the
  program helpers are not registered at all; and a seccomp filter refuses
  `execve` and new processes. The run's inputs, work folder and output are host
  directories there.
- **In Ginux.** Everything else runs in a Ginux domain exactly as before.

The helpers that put a recipe in Ginux are `exec`, `exec_checked`,
`exec_output`, `shell*`, `git_checkout`, `target_extract_tar`,
`command_exists`, `rpm_installed`, `rpm_version`, `dnf_*`, `apk_*`,
`carry_link_flags` and `carry_libraries` (one list: `PROGRAM_HELPERS` in
`src/helpers/mod.rs`). So does anything the analysis cannot read: `eval`, a
function pointer built from a computed name (`Fn(name)`) or a dynamic
`import`. The run says which way each recipe went and why, and `recipe info`
shows it:

```text
recipe: ripgrep: runs on the host (starts no program)
recipe: cmake: runs in Ginux (calls exec_checked at cmake.rhai:8873)
```

`--execution auto` (the default) applies this rule. `--execution ginux` keeps
every recipe in Ginux. `--execution host` requires the host and refuses a
recipe that is not eligible, naming the call that keeps it in Ginux. A host
run restores kept downloads, publishes and enforces its deadline like a Ginux
run; its storage bounds are checked at the end of the run. Its kept records
have an identity of their own.

## Build and prepare the environment

[Linux setup and bootstrap](docs/setup-linux.md) records the verified host,
input reproduction, package roles, required privileges and output ownership.
Select the recorded source pair and matching worker; final delivery of later
local corrections remains separately tracked.

From the LevitateOS workspace:

```sh
cargo build --offline -p levitate-recipe --bin recipe --bin recipe-image
```

Prepare a compiler image explicitly from pinned, already acquired inputs. The
[bootstrap guide](bootstrap/README.md) describes the manifest and provenance:

```sh
target/debug/recipe-image \
  --manifest tools/recipe/bootstrap/ginux-build-image.json \
  --input-root .artifacts/ginux-env \
  --output .artifacts/ginux-env/prepared-image
```

The output directory must be new. It holds an OCI image layout
(`oci-layout`, `index.json`, `blobs/sha256/…`), an uncompressed `image.oci.tar`
of it and `provenance.json`, whose `manifest_digest` is the image's identity.
Configure the built `ginux` CLI and the image before executing recipes:

```sh
export RECIPE_GINUX=/absolute/path/to/ginux
export RECIPE_IMAGE_LAYOUT=/absolute/path/to/prepared-image/image.oci.tar
export RECIPE_IMAGE=local/recipe-compiler@sha256:MANIFEST_DIGEST_FROM_PROVENANCE
export RECIPE_WORKER_BIN=/absolute/path/to/recipe
```

`RECIPE_IMAGE_LAYOUT` (the uncompressed layout tar, `image.oci.tar`) is
needed only while the image is not yet imported into Ginux's state root; the
import verifies every blob and its printed manifest digest must equal the one
`RECIPE_IMAGE` names. Without `RECIPE_IMAGE` and without an installed
`root-reference` there is no default image: Recipe stops with the way forward.
The variables `GINUX_BIN`, `GINUX_ROOT`, `GINUX_ROOTFS`, `GINUX_MINIMAL_ROOT`
and `GINUX_MINIMAL_ROOTFS` of the retired runtime are not translated: Recipe
stops when one is set and names its replacement.

| Variable | Meaning |
| --- | --- |
| `RECIPE_GINUX` | The `ginux` CLI. Default `<prefix>/libexec/recipe/ginux`, else `/usr/libexec/recipe/ginux`. |
| `RECIPE_GINUX_STATE_ROOT` | Ginux's state root (images, base roots, instances). Default `/var/lib/recipe/ginux`; must be a root-owned folder chain. |
| `RECIPE_GINUX_RUN_ROOT` | Ginux's run root. Default `/run/recipe/ginux`; same rule. |
| `GINUX_INSTANCE_HELPER` | The CLI's own variable, passed through unchanged: a root-owned wrapper around the helper (the OS builder and the test fixtures use it; the default is `/usr/libexec/ginux/ginux-helper`). |
| `RECIPE_IMAGE`, `RECIPE_IMAGE_LAYOUT` | The compiler image (`local/recipe-compiler@sha256:<manifest>`) and its layout. |
| `RECIPE_MINIMAL_IMAGE`, `RECIPE_MINIMAL_IMAGE_LAYOUT` | The same for the minimal runtime image. |
| `RECIPE_WORKER_BIN` | The worker, a `recipe` binary of the same build. |
| `RECIPE_RUNTIME_URL` | Where `recipe runtime fetch` downloads `image.oci.tar.zst` (`{digest}`, `{short}`). |
| `RECIPE_OUTPUT_TIMEOUT_SECS` | How long a worker may go without finishing an output line. |

A recipe that declares `NEEDS.runtime: "minimal"` (one whose programs are the tools of
a root it is given, such as a root's effects recipe) runs in the minimal runtime image
instead: a few megabytes of the same Rocky base with a shell, GNU coreutils, findutils,
sed and grep (`bootstrap/ginux-minimal-image.json`). `RECIPE_MINIMAL_IMAGE` and
`RECIPE_MINIMAL_IMAGE_LAYOUT` select it as the two variables above select the full
image; without them Recipe reads the `minimal-reference` its install ships and uses
`minimal.oci.tar` beside it. Where no minimal image is named at all, such a recipe
runs in the full image, which has every program the minimal one has, and Recipe says
so in one line. If `RECIPE_IMAGE_LAYOUT` is omitted, Recipe uses the layout its install
ships (`<prefix>/share/recipe/runtime/compiler.oci.tar`, where the prefix is the
parent of the directory holding the `recipe` binary, for example `/usr`), else
`$XDG_CACHE_HOME/recipe/images/<manifest digest>.oci.tar`. When neither exists, Recipe
downloads `image.oci.tar.zst` on first use and stores the decompressed layout there;
the import checks it. `recipe runtime fetch` does this now; `recipe runtime status`
shows what is selected. `RECIPE_RUNTIME_URL` names another address (`{digest}` and
`{short}`, the first 16 digits, are replaced); the default is a
`recipe-runtime-{short}` release of `LevitateOS/prebuilt`. The CLI and the shipped
sources are found the same way under the prefix, then under `/usr`. The worker is
staged separately; changing Recipe does not require rebuilding an unchanged image.

### Privileges

The native runtime requires Linux x86_64. Every privileged step of the public
lifecycle is the Ginux helper run as root, and the only thing `sudo` runs is
Recipe's guard
(`sudo /usr/libexec/recipe/recipe-ginux-guard --protocol 1 --state-root … --run-root … VERB`),
which runs the helper at `/usr/libexec/ginux/ginux-helper`. There is no longer an
ordinary-user path: the user-namespace launch and its AppArmor profile are gone.
What runs the guard:

- **root** (the OS builder, CI, `sudo recipe …`): needs no rule, only a `sudo`
  binary on the host;
- **an administrator**, after `recipe runtime stage --sudoers GROUP`
  (`%wheel` on Fedora, Rocky, LevitateOS and AcornOS, `%sudo` on Ubuntu) has
  written `/etc/sudoers.d/recipe-ginux`: members of GROUP may run the guard
  without a password, pinned to Recipe's state and run roots. The rule names the
  guard, never the helper. The guard allows only Recipe's verbs (`import`,
  `inject`, `start`, `status`, `stop`, `delete`, `extract`) on instances named
  `recipe-<uid>-*` (the caller's own uid; uid 0 may use any uid's), takes the
  caller only from `SUDO_UID`/`SUDO_GID`, clears the environment, and re-renders
  the instance config Recipe imports from validated fields (the worker as init,
  no forwards, no hooks, no unknown keys, an image named by `@sha256:`, the
  unprivileged `[access]` identity). For a caller other than root the image's
  exact reference must be listed in the root-owned allowlist
  `/usr/libexec/recipe/runtime-images`, which `recipe runtime stage`,
  `sudo recipe runtime fetch` and the installer write. `import-image`,
  `prepare-root`, `run-runtime` and every other verb are refused: images are
  imported as root by `sudo recipe runtime fetch` (the installer runs it), and a
  run by anyone else without an imported image stops and says to run it. The
  result is a passwordless sandbox for Recipe-shaped instances from trusted
  images, not a guarantee that it is harmless to the host: the code that unpacks
  a trusted image runs as root and follows symlinks in it (R13, R14), every
  user's workload runs as the same host uid (R15), and there are no per-user
  caps (`docs/runtime-limits.md`, "What a shared host still exposes"). Any other
  user cannot run a recipe in Ginux. The installed LevitateOS and AcornOS images
  ship the rule and the standalone installer writes it for the group it picks;
- **anyone else**: Recipe stops with the way forward (run as root, or ask an
  administrator to install the rule). Recipes that start no program still run on
  the host without any of this (`--execution auto`).

The guard, helper, runtime, image tool and allocator must be root-owned files
that no other user can write (`/usr/libexec/recipe`, `/usr/libexec/ginux`,
`/usr/lib64/ginux`): a user-writable copy reached through the passwordless rule
would be a root escalation. The state root `/var/lib/recipe/ginux` is created by
the helper on first use and is shared by all users (one image import per host);
instance names carry the uid and the pid. Instance directories are root-private
(0700) and the helper streams an instance's console output to the caller, so one
user cannot read another's builds through the host file system (workloads of
different users still share one host uid: see R15). `docs/runtime-limits.md`
lists what a shared host still exposes.

```sh
sudo env RECIPE_GINUX="$RECIPE_GINUX" RECIPE_IMAGE="$RECIPE_IMAGE" \
  RECIPE_IMAGE_LAYOUT="$RECIPE_IMAGE_LAYOUT" RECIPE_WORKER_BIN="$RECIPE_WORKER_BIN" \
  "$RECIPE_WORKER_BIN" install tools/recipe/examples/hello.rhai --build-dir ./hello-work
```

Only install commands required by a recipe in its compiler image. For example,
a Git recipe needs Git there. An explicit Rust musl target needs its matching
standard library/linker support. GNU and musl outputs are distinct products.
The verified GNU and musl Cargo fixtures use one job per client and the local
Ginux integration's 32-task bound. GNU uses its default linker configuration;
musl selects the compiler image's `rust-lld` as described in
[the bootstrap instructions](bootstrap/README.md). Both fixtures compile once
across two cache clients and execute the resulting threaded program.

## Run a recipe

Execution and inspection commands print readable summaries when stdout is a
terminal. Add `--json` for the full result, or `--json-output result.json` to save
it. Redirected or piped execution/inspection output keeps the existing JSON
format. Progress and diagnostics go to stderr; `list`, `info` and `hash` print
text. Each command's `--help` includes examples.

```sh
recipe install tools/recipe/examples/hello.rhai \
  --build-dir .artifacts/work/example \
  --state-dir .artifacts/state/example \
  --json-output .artifacts/example-result.json
recipe state tools/recipe/examples/hello.rhai --build-dir .artifacts/work/example \
  --state-dir .artifacts/state/example
```

Pass the same recipe, inputs and destination for retained operations.
Use the result's `outputs` paths; they identify published host files or
directories (output name to absolute path).
Logs and tool output go to stderr; stdout or `--json-output` contains final JSON.
Published paths and retained state keep the coordinator's ownership and private
export-directory permissions. A privileged producer needs an appropriately
privileged consumer, or explicit installation of selected product files with
the desired ownership. Selecting a publication with a symlink does not grant
another user access. No automatic privilege bridge is provided.


For the repository's actual offline live-tool producer:

```sh
cargo run --locked -p distro-builder --bin distro-builder -- \
  --repo-root "$PWD" artifact prepare-tool-source recstrap .artifacts/recstrap-source
recipe install distro/builder/os-recipes/install-tools.rhai --define BIN=recfstab \
  --input source=.artifacts/recstrap-source \
  --build-dir .artifacts/work/recfstab --json-output .artifacts/recfstab.json
```

Source preparation vendors only the selected production closure, offline, with
versions from the workspace lock. The managed recipe performs compilation.
`recfstab` is a binary of the `recstrap` package, so source preparation selects
`recstrap` while `BIN` selects `recfstab`.
Canonical product preparation calls this same producer. `cargo xtask setup`
explicitly prepares the separate QEMU/OVMF product; scenario commands consume
existing prepared artifacts.

## Recipe interface

[WRITING_RECIPES.md](WRITING_RECIPES.md) is the practical guide and
[docs/recipe-format.md](docs/recipe-format.md) the full contract. [HELPERS_AUDIT.md](HELPERS_AUDIT.md) lists registered helpers.

- A recipe is declarations (`NEEDS`, `ABOUT`, `PACKAGE`, `SOURCES`, `FILES`,
  `PACKAGES`, `ACCOUNTS`, `UPSTREAM`, `RERUN`, `deps`, `build_deps`) plus the
  optional functions `build(src, out)`, `configure(tree[, src])`, `verify(out)`,
  `latest()` and `cleanup(reason)`. Functions read the engine's constants
  (`PREFIX`, `JOBS`, `HOST_LIBC`, `PACKAGE_FORMAT`, `INPUTS`, `DEFINES`, `DEPS`,
  `WORK`). There are no hooks, readiness checks or `ctx`.
- Recipe decides what is done from its own records (downloads by checksum, results
  by identity, installed programs by receipt). An interrupted step (download,
  build, install, cleanup) starts again from its beginning; the steps that
  finished keep their results. Setup (top-level code) runs again and current
  facts remain current.
- The output is the folder or file Recipe hands to `build`. Successful
  execution, a validated result file and validated archive data are
  required for publication. Failed work cannot replace earlier publications.
- Dependencies run in separate instances with their own NEEDS. Declared products
  are snapshotted into the consumer (`DEPS`); a directory output named `tools`
  is put on its `PATH`. No parent grant is lent. A dependency is never
  installed implicitly by `remove` or `cleanup`.
- `list` and `info` inspect loader-owned declared metadata without running setup
  or helper code. Dynamic metadata is reported unavailable.
- `list --json` prints one entry per recipe for a front end (`schema_version` 1):
  name, version, description, path, the recipe's `ABOUT` block, NEEDS, dependency
  names, the `steps` it has, the `functions` it defines, its `upstream`, its
  `state` on this system and its `keep` option, plus `runs_here` and `not_runnable_reason`
  (whether this system's C library can run it, from the same comparison
  `install` makes, and the message `install` would give, else null; the index
  does not carry them). The top level has `host_libc`, this system's C library
  (`glibc`, `musl` or `unknown`). It needs no Ginux, and a recipe that
  cannot be read is reported under `problems` instead of failing the list.
- `index [FOLDER]` writes `index.json`, the file a source of recipes publishes
  so its recipes can be listed without downloading them: the folder's
  `repo.toml` (`name`, `title`, `about`, `address`, `keeper`, optional `uses`),
  every `list --json` entry without its path, `runs_here` and `not_runnable_reason`, the SHA-256 and size of each
  recipe file and of the files beside it that share its name, and the commit
  they are at. The folder must be in a Git repository with full history and
  the listed files committed. `--check` fails when the file is out of date.
- Sources add recipes beyond the recipe folders. `source add ADDRESS` adds a
  remote source: a repository on github.com or gitlab.com, or the address its
  files are fetched from with `{rev}` and `{path}` in it. Only its published
  index is kept; `refresh [NAME]` fetches it again over HTTPS. `source add
  --path FOLDER` adds a checkout that is read as it is. `source list` shows
  them and `source remove NAME` removes one. A source is one file,
  `sources/NAME.toml` under the package root (`/var/lib/recipe/sources` for
  root), or `/usr/share/recipe/sources` when the image ships it. An image
  ships a copy of its source beside that file, `NAME/` with the published
  index and its files: it stands in for the index until one is fetched and
  spares downloading a file it holds, so shipped recipes install offline.
- `SOURCE:NAME` names one source's recipe; `local` names the recipe
  folder. A bare name means a recipe folder first, then the one source that
  has it; when several do, Recipe asks for `SOURCE:NAME`. `list` never touches
  the network: it shows a remote source from its kept index, and `list --json`
  adds `sources` and each recipe's `source`. An explicit `--recipes-path` or
  `$RECIPE_PATH` uses no sources.
- The release source, named `system`, is built in: the group recipes of the
  system release run by the root a system change targets (the other root on A/B,
  `/` without A/B, `DIR` with `--system --root DIR`), which that root carries as
  base files in `usr/lib/recipe/release-recipes/NAME.rhai` (the builder puts them
  there; `apply-release` replaces them with the base). It is read from disk: no
  index, no refresh, and `source remove system` refuses. `source list` and
  `list --json` show it first, with `kind` `release`, `title` "OS-NAME VERSION"
  (os-release `NAME`, the version from `usr/lib/levitate/release.json`), `path`,
  `os` and `version`; its recipes carry `"source": "system"` like any other, and
  `system:NAME` names one. Where the root has no such folder (Recipe on another
  distribution) or the collection is explicit (`--recipes-path`, as the image
  builder and the installer use), there is no release source and nothing below
  applies; a recipe given as a file path is always used as it is.
- System groups change only with a system release (a system group is a recipe of
  `ABOUT.kind` `system`, or `library` with a `system` output; base and groups are
  made from one package snapshot). On a root with a release source:
  - A system change (`--system install|update|remove`) resolves a name the
    release source carries to its recipe, whatever other source carries the name;
    `SOURCE:NAME` of a remote source then takes the release's recipe too, with a
    note on stderr. A release recipe's dependencies resolve beside it, within the
    release, and a remote recipe's dependencies that the release carries are
    taken from the release, so a catalog's copy of a group never replaces the
    release's.
  - A system group of a remote source (the LevitateOS catalog, say) is not
    installed, whether or not the release carries the name. `list --json` marks
    it, and any remote recipe the release carries by name, `runs_here: false`,
    `not_runnable_override: null`, with a one-sentence `not_runnable_reason`;
    `--any-system` does not lift it. A newer system release brings newer groups;
    a folder of one's own (`source add --path`, or the recipe folder) is not
    restricted.
  - `recipe updates` never lists a package whose name the release carries: it is
    at the release's version by definition. `--system update NAME` takes the
    release's recipe; a package installed from a release's recipes that the
    current release no longer carries is reported (`"not_in_release": true`,
    `"changed": false`, exit 0) and not updated from anywhere else.
    `--system installed --json` gives each package `from_release` and
    `not_in_release`. A package is from the release when it was installed from
    a release's recipes, or from any other copy (the install medium's) of the
    recipe the root's release carries with the same content. The recipe path a
    receipt records is a note of where that copy lay; an update by name finds
    the release's or a source's recipe by its name, and falls back to that path
    only while the file is still there.
- Naming a recipe of a remote source downloads it with every recipe it needs
  (`deps`, `build_deps`), each file at the index's commit and checked against
  the index's SHA-256 and size; a file that differs stops before anything
  runs. `install` and `update` fetch the index first and keep the copy in
  `fetched/SOURCE/NAME-KEY/` under the package root; other commands use the
  kept index and the cache. The receipt records `source`, `commit` and the
  recipe file's `sha256`. `update NAME` follows the receipt's source and runs
  nothing when the source has not changed; `remove` deletes the kept copies.
- `install --sha256 HASH` runs only when the recipe file has that checksum:
  what a person looked at in a front end is what runs.

## Inputs and selected targets

`NEEDS.inputs` maps names to `file`, `directory`, `file?` or `directory?`.
`--input NAME=PATH` stages one declared input as a read-only copy at `/inputs/NAME`; missing
required, undeclared, duplicate or wrong-kind inputs fail before setup. Use
`--input DEP::NAME=PATH` for a dependency: inputs are never implicitly inherited.
A host path in a define grants no filesystem access. Root `.git` and `target`
directories are omitted from ordinary source imports. Input bytes, modes, link
identity and link targets participate in state identity. Confined relative links
are preserved; hard links are not (the public lifecycle carries only directories,
regular files and symlinks, so a file linked twice arrives as two files). Named `rootfs`/`target`/`root` trees may contain absolute
rootfs symlink bytes, resolved within the tree; they never authorize following host
paths. One invocation stages at most 100,000 entries, or, with such a tree,
`NEEDS.storage_max_objects` when that is larger.

`--input target=PATH` seeds the output before `build` runs (with one output,
`out` starts as a copy of the input). Compiler-image files under `/usr` cannot
establish that a selected product is installed. The imported input and earlier
published generations stay unchanged. The CLI handles package activation
separately; `--no-activate` selects only the producer route.

## State, diagnostics and recovery

`--build-dir` selects a stable logical destination and its shared lock. Domain
work remains `/work/build`. The default destination is under the selected state
root, which defaults to `$XDG_STATE_HOME/recipe` (normally `~/.local/state/recipe`).
Use `--state-dir PATH` to choose a different state root.

Identity includes the recipe/module/dependency closure, declared input content,
custom defines, execution options and the root image, by content. Changed inputs
start fresh state. Recipe itself (the coordinator and the worker) and the Ginux
tools enter it by declared behaviour versions, not by their bytes, so a
rebuilt `recipe` or `ginux` that behaves the same reuses saved state. The
versions live in `src/identity.rs`: `ENGINE_BEHAVIOUR` (bumped with any Recipe
change that can alter what a recipe produces: helper semantics, target metadata,
staging, how inputs and outputs are materialized, the rendered Ginux config, a
Rhai or archive-crate upgrade that changes results; not for a pure speed-up),
`GINUX_BEHAVIOUR` (bumped when a Ginux change can alter what a workload observes:
syscall results, root layout, credentials, clocks, signals, the injected and
extracted archive formats; 2 is the public lifecycle), the coordinator/worker records version, and the Ginux catalog
and build-base contract digests Recipe pins and checks. `--exact-identity` (or
`RECIPE_EXACT_IDENTITY=1`) also names the worker's bytes and the four Ginux files'
(CLI, helper, image tool, runtime) bytes, so
only byte-identical tools reuse saved state; release builds use it. Exact and
default identities never share saved state. A kept download or result becomes
recoverable only after its archive and its record are durable together; an
interrupted step keeps nothing and starts again from its beginning. A
`cleanup(reason)` function runs once at the end of a run, after Recipe's own
clean-up; its diagnostics preserve the original failure.
A corrupt current record/snapshot fails before workload startup; retain it for
diagnosis and choose a new state directory to start explicitly fresh.

`--no-persist-state` turns the kept downloads and results off; it does not disable managed
execution or publication. Published products are not saved state.

Diagnostics live in `<state-root>/logs/<invocation-id>/`, outside invocation
scratch: `worker.log`, `supervisor-stderr.log` (the instance's audit log: the
worker's stderr and the runtime's audit lines), `authority.json` (with the
instance's name) and `result.json`.
The authority receipt saves the actual declaration, effective constraints, pinned
endpoints, rendered config and catalog/digest used for this instance. Logs are bounded;
active leases protect running invocations, and up to 32 recent completed
invocations are retained. The coordinator prints the diagnostic directory.
Diagnostics are not fsynced: after a crash the last records may be missing or
empty. Private leased scratch is reclaimed after abandoned invocations; arbitrary
workspace directories are never cleanup candidates.

The scratch pool is a private directory on disk, `~/.cache/recipe/scratch`
(`$XDG_CACHE_HOME/recipe/scratch`), because `/tmp` is RAM on many systems. Set
`RECIPE_SCRATCH_DIR` to an absolute path to put it elsewhere. A set `TMPDIR`
still selects `$TMPDIR/recipe-scratch-<uid>`, as recab uses it. Published outputs are kept in
`<state-root>/exports` (normally `~/.local/state/recipe/exports`), not in the
current directory. The compiler image is imported once per host into Ginux's
state root (`/var/lib/recipe/ginux/cache/oci`, root-owned, about 1.3 GB). Each
run is a new instance, `recipe-<uid>-<pid>-<random>`, with a private root:
copy-on-write clones of a base root where the state root's file system has
reflinks (XFS, Btrfs), a full expansion of the image elsewhere (slower: the
whole image is expanded and re-owned per run). The instance is always deleted
when the run ends, however it ends; if Recipe or the helper dies first, the next
Recipe command that uses Ginux stops and deletes the leftover instances of dead
runs of the same user (root: of every user). `recipe runtime remove` frees only
the downloaded layouts: the imported image and base roots sit under the
root-owned state root, which no public Ginux verb can shrink (`recipe runtime
status` reports them as `system_bytes`).

SIGINT/SIGTERM cancel execution and await descendant teardown. Cancellation and
deadline failure keep nothing of the interrupted step and do not publish failed
work. `NEEDS.timeout_secs` bounds each recipe instance, including legitimate silent
commands (default 3600, range 1–86400); `--timeout-secs` can only lower it. A
worker that prints nothing is bounded separately by `RECIPE_OUTPUT_TIMEOUT_SECS`
(default 300, range 1–86400): before its first output line, and for a line it
has started but not finished, it has that long; a silent phase after the first
line runs until the execution deadline.

## Options

Run `recipe --help` and `recipe install --help` for the complete current syntax.

| Option | Meaning |
| --- | --- |
| `-r, --recipes-path DIR` | Explicit name/module/dependency search directory. Otherwise the user's recipe folder precedes the sources; `$RECIPE_PATH` selects one directory exclusively. |
| `--package-root DIR` | Store for installed receipts, generations and the active `profile` symlink. |
| `--no-activate` | Produce outputs only, without installing/removing an active package. |
| `-b, --build-dir DIR` | Logical destination identity and lock. |
| `--input NAME=PATH` | Explicit immutable file/tree input. |
| `--define KEY=VALUE` | Recipe-specific constant; not a runtime path substitute. |
| `--target-os linux --target-arch x86_64` | Currently supported native execution platform. |
| `--execution auto\|ginux\|host` | Where recipes run (see Execution modes); default `auto`. |
| `-j, --jobs N` | Requested jobs, 1–16, default 1; Ginux task limits still apply. |
| `--keep`, `--no-keep` | Keep the downloads and build material after a successful install (remembered per program), or turn that off and delete them. |
| `--memory-mib N` | Lower `NEEDS.memory_mib` (default declaration 2048 MiB). |
| `--storage-max-objects N` | Lower `NEEDS.storage_max_objects` (default declaration 16384). |
| `--storage-max-bytes N` | Lower the declared writable-data cap within effective memory. |
| `--network-host HOST:PORT` | Select a subset of `NEEDS.network`, repeatable; `--no-network` selects none. |
| `--network-max-bytes N` | Lower `NEEDS.network_max_bytes` (default declaration 8 GiB). |
| `--json-output FILE` | Final context result, separate from stderr diagnostics. |
| `--progress-file PATH` | Keep PATH current with the recipe's `progress(done, total, what)` reports: one JSON object (`version`, `invocation`, `recipe`, `phase`, `what`, `done`, `total`, `started`, `updated`, `state` = `running`/`finished`/`failed`), replaced atomically at most once a second. |

Inputs, restored kept files and publications are bounded and validated;
increasing a bound does not add a missing syscall or compiler capability.
**There is no storage limit inside a run**: `storage_max_bytes` and
`storage_max_objects` bound the archive taken out of the instance (an output
over them fails the run with the storage message, after the fact), and a
free-space watchdog cancels a run whose host disk drops below `max(2 GiB, 5 %)`,
but a recipe can fill the host file system holding the state root while it runs.
Network grants resolve and pin numeric TCP endpoints before the instance
is created and install the hostname map (`/etc/hosts`, always present, with
`localhost`). **A grant is a port**, not an address: the instance shares the
host network and the contract lists `tcp:PORT` for each distinct granted port,
so a program a recipe starts (`git`, `curl`, a build system) can connect to any
address on a granted port, with no connection or byte cap, and can see the
host's network. Recipe's own downloads are still held to the declared
`HOST:PORT` at the pinned addresses and to `NEEDS.network_max_bytes` (the worker
enforces both), and `network_usage` reports only what the worker counted.
There is no UDP, proxy or listening service.

`download(url, dest, sha256[, resume])` verifies staged bytes before replacing a
valid destination. `git_checkout(url, exact_dest, full_commit)` requires a full
40/64-hex commit and a direct repository URL: Git redirects are disabled, and
cached repository settings that override transport or execute hooks/filters are
rejected. HTTP download redirect destinations need explicit endpoint grants.
Scoped HTTPS acquisition retries transient 429/502/503/504 responses at most
twice. Numeric `Retry-After` delays up to five seconds are respected; longer
delays, HTTP-date values and malformed values defer automatic retry instead of
retrying before the origin's requested deadline.
Recipes own version/URL/digest facts and literal NEEDS authority. Variant TOML
keeps recipe selection and input path bindings. The Fedora rootfs/preseed declare
8192 MiB; Alpine declares 4096 MiB. Fedora rootfs declares 196608 objects and
Alpine rootfs declares 131072; use `inspect` on the selected recipe for its limits.
`memory_mib` bounds each process's virtual address space, not aggregate tree RAM.
Jobs select eligible real CPUs, not a virtual topology. Numeric IP/port grants
are not DNS/HTTP identities. Archive timestamps have whole-second precision. See
[supported runtime limits](docs/runtime-limits.md) before claiming isolation or
Linux compatibility beyond these boundaries.

`remove` and `cleanup` never install a dependency and never run a dependency's
cleanup function; they act on the named program only. Use `install` to prepare
dependencies.

## Read authority without executing setup

```sh
recipe validate tools/recipe/examples/hello.rhai
recipe inspect tools/recipe/examples/hello.rhai
recipe explain tools/recipe/examples/hello.rhai network.connect
recipe explain /absolute/state/logs/INVOCATION network.connect
recipe diff tools/recipe/examples/hello.rhai tools/recipe/examples/threaded-hello.rhai
```

These commands never run recipe setup or launch a workload. Validation also
checks dependency declarations, their input bindings and compiled contracts.
Inspection lists resolved authority and pinned contract digests; it cannot know
which endpoints a future execution will use. Diff shows added/removed/changed
typed paths, endpoints, raised/lowered limits and contract digests. Invocation
explain reads the retained receipt and available audit observations, so later
source edits do not rewrite the explanation. A missing/truncated observation is
reported as unknown. Finished results include `contract` and `network_usage`;
`network_usage` is what the worker counted for Recipe's own downloads, so a
program's connections are not in it (they are `unobserved`, never labeled
unused), and a complete summary produces warnings for unused grants. Diagnostics do
not become success/protocol evidence or grant additional authority.

## Validation and limits

[Managed integration CI](docs/ci.md) describes Ginux/image pins, runtime
privileges, catalog coverage and explicitly opted-in publication.

Run focused real CLI/integration cases with the actual Ginux and prepared
image (the preconditions are in the header of `tests/e2e.rs`: root or the sudoers
rule, the five Ginux files installed root-owned, `RECIPE_GINUX`, `RECIPE_IMAGE`,
`RECIPE_IMAGE_LAYOUT`). Build a worker snapshot before a run and keep it
unchanged for that run:

```sh
cargo test --offline -p levitate-recipe --test e2e --no-run
RECIPE_TEST_BIN=/absolute/path/to/recipe cargo test --offline \
  -p levitate-recipe --test e2e managed_target:: -- --test-threads=1
```

The checkout's workspace and the privileges above are required. See the
superproject implementation checkpoint for exact tested revisions and remaining
production/VM evidence. Real tool lifecycle, custom-rootfs, selected-target,
link preservation and mediated acquisition are separately exercised;
none implies general Linux syscall or every distro package support.

For a release, set the version in source before verification, and retain
artifact hashes with the matching source, worker, Ginux tools and image provenance.
CI validates the selected commit. Publish the reviewed artifacts separately.

License: MIT OR Apache-2.0.
