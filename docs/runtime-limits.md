# Supported execution boundaries

Status: written 2026-11-23 for the public-lifecycle cutover
([migration plan](ginux-public-lifecycle-migration.md), section 6; design
[ginux-cutover-design.md](ginux-cutover-design.md)). Nothing here has been run
yet: the statements about what Ginux enforces are read from its documentation
and code, and the verification pass will confirm or correct them.

Recipe runs a recipe that starts programs as an instance of upstream-shaped
Ginux, through its public lifecycle (`create`, `inject`, `start --wait`,
`status`, `extract`, `delete`), with the native configured runtime and contract
catalog 2. The target is Linux x86-64; macOS and other targets are outside this
release. A catalog permission is a ceiling: it does not implement every Linux
syscall variant, flag or backend. The selected Ginux source's
`docs/configurable-contract.md` owns the precise supported flags, errors and
edge behaviour; this page lists what Recipe depends on and what it gave up.

## What was given up (accepted regressions)

The retired downstream `ginux launch` had owned storage and a mediated network.
The public lifecycle has neither. These are the owner's accepted trade for
running on Ginux's public workflow, and they are real:

- **There is no storage limit inside a run.** The instance root is a directory
  tree on the host file system that holds Ginux's state root
  (`/var/lib/recipe/ginux`). A recipe can fill that file system while it runs.
  What remains: the archive taken out of the instance
  (`ginux extract --max-bytes/--max-members`) is bounded by the recipe's
  `NEEDS.storage_max_bytes` and `storage_max_objects`, and an output over them
  fails the run with the storage message **after the fact**; and Recipe's own
  free-space watchdog (not Ginux's) cancels a run (category `storage`, "the
  host disk is nearly full") when the free space of the state root's file
  system drops below `max(2 GiB, 5 %)`. The watchdog bounds the damage, not
  the write, and it runs in the Recipe client: a caller of the guard that does
  not go through Recipe is not held to it.
- **Network grants are per port.** The instance shares the host network
  (`[network] backend = "shared"`) and the contract lists `tcp:PORT` for each
  distinct port of the pinned endpoints. A program a recipe starts (`git`,
  `curl`, a build system) can connect to **any address on a granted port**, with
  **no cap on connections or bytes**, and can see the host's network. What
  still holds: with no grant the instance has no network at all
  (`backend = "none"`); Recipe's own HTTP client in the worker (`download`,
  `download_exact` and the other fetches) reaches only the declared `HOST:PORT`
  at the pinned addresses, follows redirects only to declared endpoints, and
  counts every body against `NEEDS.network_max_bytes`; and `network_usage` in a
  result is what the worker counted, so a program's connections are
  `unobserved`, never labeled unused. The refused-connection report
  (`refused` in `--machine-events`) is gone: the native audit lines do not
  name the address. There is no UDP, listener or proxy.
- **Live system roots are copied, not bound.** `--system` and a root's effects
  recipe get their roots as copies of the folders they need (no read-only
  bind). That costs the size of the root in disk and time per run, and loses no
  safety.
- **The offline cache reaches a run only by declared pin.** Recipe injects the
  cache payloads of the `download` pins the recipe declares; a download whose
  digest the recipe computes at run time does not find the cache (relevant to
  offline OS builds).
- **Inputs and outputs have no hard links.** They travel as tars of directories,
  regular files and symlinks, so a file linked twice arrives as two files, and a
  FIFO, socket or device left in `/work` or `/out` fails the run at extraction.
- **No privileges without an administrator.** Every privileged step is the
  Ginux helper run as root behind Recipe's guard through `sudo`; see
  "Privileges" below.
- **Per-run cost.** On a file system without reflinks every run expands the
  whole image (about 1.3 GB), re-owns it, then copies inputs in and outputs out.
  Whether this is acceptable for OS builds is to be measured in verification.
- **Seven operations of the old catalog are not in catalog 2:** `lstat`,
  `readv`, `pipe`, `nanosleep`, `sendfile`, `link` and `rt_sigpending` were added
  by the downstream catalog 1002. Whether the native path refuses them for
  programs that need them is unverified (risk R2 of the design).

## Boundaries that Recipe states

| Surface | Boundary |
| --- | --- |
| Memory | `NEEDS.memory_mib` becomes the instance's per-process address-space bound (`[resources] memory_mib`); it is not a combined process-tree RSS or host budget. |
| CPU | Requested jobs (`[resources] vcpus`) select real CPUs by affinity. This is not virtual topology, CPU-time or usage accounting. The per-instance deadline (`NEEDS.timeout_secs`) is separate and enforced by Recipe, which signals the run's process group. |
| Identity | The workload runs as virtual uid/gid 1000 (`[access]`); on the host the root-launched workload drops to uid 65534. Programs that look at uid 0 or at the host's accounts see Ginux's view, not Linux's. |
| Files | Explicit inputs are copies owned by the workload's own host identity (every workload of every user runs as host uid 65534, see below), so file ownership protects nothing: the workload owns its inputs and may change their modes as far as the kernel is concerned. What keeps a recipe from changing them is two things together: the inputs are injected without write bits (files `mode & ~0222 | 0444`, directories 0555), so a plain write fails, and `[contract.filesystem] deny = ["/inputs"]` makes the native runtime refuse, with EPERM, the calls that would undo that or write anyway (write opens, creation, rename, link, unlink and mode changes whose path lies under `/inputs`). `write`/`create` are not enforced by path. The deny is a check of the path strings the runtime reads from the workload at each call, not a kernel barrier: it resolves symlinks itself, including the final one (so unlinking a symlink in `/work` that points into `/inputs` is refused too), it refuses any covered call whose path it cannot resolve (512 bytes or more after joining the working directory, more than 16 symlinks), and a hostile workload that rewrites a path string from a sibling thread between the check and the kernel's use of it can get past it. It stops ordinary recipes and build tools from changing their inputs; it does not fully prevent writes by a workload that sets out to defeat it. `/etc/hosts` is always present (`localhost` and the pinned names) and the image must not contain it. Archive modification times use whole seconds. Target UID/GID, modes, links and capabilities/xattrs use the separate validated [target metadata contract](target-metadata.md). |
| Network | See above. The coordinator resolves declared hostnames once into a bounded hostname map and numeric TCP endpoints; the granted ports are derived from them. Authority covers a port, not DNS names, TLS/HTTP identities or URL paths. Redirect destinations also need declarations. |
| Publication | Consumers require a clean `exited` outcome from `ginux status`, a successful worker result inside the extracted `/work`, and a validated archive. A final path's existence alone is insufficient. Published generations retain coordinator ownership and private directory permissions. |

Explicit contract denial precedes effects. The selected image must contain each
actual tool and its libraries. A helper's registration or a tool's
`--help`/`--version` does not establish that the full consumer works. Recipe
activates user-package publications in an owned profile; recab owns OS image
and A/B activation. Package helpers do not add a general package solver or
global ownership database. The installer UI's PTYs run on the libvirt guest's
Linux kernel; their acceptance does not establish general PTY support inside
Ginux recipe workloads.

## Privileges

Every privileged verb of the public lifecycle is the Ginux helper run as root,
and the only program `sudo` runs is Recipe's guard:
`sudo /usr/libexec/recipe/recipe-ginux-guard --protocol 1 --state-root S --run-root R VERB`.
The guard runs `/usr/libexec/ginux/ginux-helper`; `import-image` is not among
the verbs it allows (see below). There is no ordinary-user path: the user-namespace launch, the
`kernel.apparmor_restrict_unprivileged_userns` workaround and the AppArmor
profile `recipe runtime stage --apparmor` wrote are gone.

| Who | What they need |
| --- | --- |
| Root (OS builder, CI, `sudo recipe …`) | Nothing but a `sudo` binary on the host. |
| An administrator | The sudoers rule `recipe runtime stage --sudoers GROUP` writes (`/etc/sudoers.d/recipe-ginux`, 0440): members of `%GROUP` (`%wheel` on Fedora, Rocky, LevitateOS and AcornOS; `%sudo` on Ubuntu) run the guard without a password, pinned to `--state-root /var/lib/recipe/ginux --run-root /run/recipe/ginux`. The rule names the guard, never the helper. |
| Anyone else | Cannot run a recipe in Ginux. Recipe stops and says to run as root or ask an administrator to install the rule. Recipes that start no program still run on the host. |

The guard, helper, runtime, image tool and allocator must be root-owned files
that nobody else can write (`/usr/libexec/recipe/`, `/usr/libexec/ginux/`,
`/usr/lib64/ginux/`): a user-writable copy reached through the passwordless rule
would be a root escalation. The state root is shared by all users and created as
root by the helper on first use; the compiler image is imported once per host.

What the guard does. It is root-owned and takes the same arguments as the helper
(`--protocol 1`, the two fixed roots, a verb). It allows only Recipe's verbs
(`import`, `inject`, `start`, `status`, `stop`, `delete`, `extract`), only on
instances named `recipe-<uid>-*` where `<uid>` is the uid sudo records for the
caller (`SUDO_UID`/`SUDO_GID` are the only source of the caller; uid 0 may use
any uid's names), and clears the environment before it runs the helper. It
reads the instance config Recipe imports with the caller's identity and
re-renders it from validated fields (the worker as `init`, no port forwards, no
hooks, no keys it does not know, an image reference with `@sha256:`, `[access]`
the unprivileged identity, never uid 0). For a caller other than root the
image's exact reference must be listed in the root-owned allowlist
`/usr/libexec/recipe/runtime-images`, which `recipe runtime stage`,
`sudo recipe runtime fetch` and the installer write; the helper applies an image
as root, so only images root installed may be named. `import-image` is not
available through the guard: images are imported as root by
`sudo recipe runtime fetch`, which the installer runs, and a run by anyone
else without an imported image stops and says to run `sudo recipe runtime
fetch`. `prepare-root`, `run-runtime` and every verb the guard does not list,
and every other instance name, are refused before anything is touched (exit
64; a refused config exits 65). `recipe-ginux-guard --check-config FILE` runs the
config check alone, without root. Instance directories are root-private (0700),
the helper streams the console to the caller with `start --wait
--stream-console --full-audit`, and after every run, whatever its outcome, it
appends the audit log (its last 16 MiB) to standard error, which Recipe keeps
as the invocation's supervisor log within the same 16 MiB bound. A user cannot read another user's instance files
through the host file system (instance directories are root-private); the
workloads are another matter, see the third item below.

### What a shared host still exposes

This is the remaining exposure; it is stated here, not hidden. The guard is not
a guarantee that the passwordless path is harmless to the host.

- **Image unpacking runs as root and follows symlinks (R13, R14).** The code
  that unpacks and prepares a trusted image runs as root: the fork's
  `ginux-oci` `apply_whiteout` and `remove_path` (R13, whiteout symlink
  traversal) and the helper's `prepare_root` (R14, symlink following inside the
  image). A hostile image could delete or overwrite host files; an independent
  review showed that a symlink `h -> /` plus a whiteout for `h/...` deletes
  `/`. The guard's allowlist keeps untrusted images away from the passwordless
  path, but root callers and the OS builder still pass images through that
  code. These are bugs in upstream's code and are not fixed; next step: fix in
  the fork, report upstream.
- **Every user's workload is the same host uid (R15).** Workloads run as host
  uid 65534 whoever started them. Workloads of different users can signal each
  other's `inject-tree`/`export-tree` processes (which stalls the instance
  lock), and a workload that defeats Ginux's mediation could reach other users'
  running workloads.
- **No per-user caps.** There are no per-uid instance, disk or cache caps. The
  free-space watchdog and the byte and extract bounds are enforced by Recipe on
  the client side, so a member who calls the guard directly bypasses them; one
  member's builds can fill the shared disk.
- **A passwordless sandbox for every group member.** Any member of the rule's
  group can create, start and delete Recipe-shaped instances from trusted
  images as root-launched, uid-65534 workloads without a password, including
  the network grants Recipe's contract allows (any address on a granted port;
  see "Network" above). A recipe the member writes can run any program in the
  image.
- **Kernel and runtime attack surface.** The workload runs under Ginux's native
  runtime, not in a virtual machine. A flaw in the runtime, the helper or the
  kernel it uses is reachable by every member of the group.
- **Shared state root.** The image cache and base roots under
  `/var/lib/recipe/ginux` are shared by all users and no public verb deletes an
  imported image.
- **Names and timing are visible.** Instance names carry the uid and the pid of
  the recipe process and the state root lists them; an instance's existence,
  not its files or console output, is visible to other users.
- **The guard and the helper are trusted code.** Their correctness is what the
  boundary rests on; the guard is small and has no unit tests, only the e2e
  cases in `tests/e2e/guard.rs`. It had independent review rounds on
  2026-11-23 (see the migration plan, section 6), none by anyone outside this
  project. Not yet run on a shared host.
- **The administrators' group is trusted anyway.** On a computer where the
  group (`wheel`, `sudo`) may become root, the rule adds a smaller capability to
  people who already have a larger one; the exposure matters where the group is
  wider than the people trusted with root.

Supported hosts: Rocky/RHEL 10 (Ginux's own RPM), Fedora 44 and Ubuntu 24.04
(Recipe's standalone release, which installs the five Ginux files and the rule
as root), LevitateOS (the rule is part of the image) and AcornOS (the helper
needs GNU coreutils, util-linux `script`/`flock`, gawk, procps and `sudo` in the
image, which sits badly with "small and strict": open, risk R4). SELinux
enforcement on Fedora (the helper runs as `unconfined_t` through sudo) and
`sudo`'s `use_pty` with binary stdin/stdout for `inject` and `extract` (and
root's `import-image`) are untested. Not yet run on Fedora 44 or Ubuntu 24.04 at all.

## Hostname budget

Hostname resolution has an eight-endpoint total budget. Explicit numeric grants
are retained first. Sorted hostname grants each receive at least one address;
remaining slots are distributed round-robin in numeric address order (IPv4
before IPv6). The selected subset is identical in the contract's granted ports,
the injected `/etc/hosts` and the invocation's `authority.json` receipt
(`pinned_endpoints` and `host_endpoints`). Selection warns when addresses are
omitted; DNS answer order does not affect it. If explicit grants or one address
per hostname cannot fit, launch fails. Availability of omitted addresses and
runtime DNS refresh are not promised. Ordinary publisher TLS verification is
unchanged.

## Dependencies and disk

A recipe's dependencies run in separate instances with their own authority and
the caller's restrictions. `recipe remove` and `recipe cleanup` act on the named
program only: they never install a dependency and never run a dependency's
`cleanup` function. `recipe state` is Recipe's own record and starts no
recipe code, so it needs no dependency products. Use an explicit `install` to
prepare unavailable dependency products.

Everything a run keeps on disk after it (downloads, unpacked sources, the work
folder, a result not installed) is deleted after a successful install unless the
keep option is on; see [recipe-format.md](recipe-format.md), section 6.5. While
it runs, the work folder is the instance's own and unbounded (above). The
imported compiler image and base roots live in the root-owned state root and are
not freed by `recipe runtime remove` or `recipe remove`: no public Ginux verb
deletes an image (risk R10 of the design), so `recipe runtime status` reports
them as `system_bytes` and an administrator removes
`/var/lib/recipe/ginux` to reclaim them.
