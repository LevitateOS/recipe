# Supported execution boundaries

The current configured contract targets Linux/x86-64. macOS and other native
targets are outside this release. Configured downstream support does not change
upstream/native Ginux policy. A catalog permission is a ceiling: it does not
implement every Linux syscall variant, flag or backend.

Select the source pair that implements the capabilities your consumer requires.
The [setup guide's delivered baseline](setup-linux.md) predates the local
compiler FD/path/stack/event and manual-readiness corrections below. Their
final source delivery and security/product acceptance remain open in the
superproject's `docs/ginux-contributions/prd15-product-contract.md`; this page
describes their bounded contract, not a completed release acceptance claim.

| Surface | Supported boundary |
| --- | --- |
| Memory | `NEEDS.memory_mib` bounds each process's virtual address space through inherited `RLIMIT_AS`; it is not a combined process-tree RSS or host/supervisor budget. Storage has separate byte/object accounting within the declared memory configuration. |
| CPU | Requested jobs select eligible real CPUs, preserving their actual IDs. This is affinity, not virtual topology, CPU-time or usage-percentage accounting. The per-instance deadline is separate. |
| Clocks | Raw queries support host realtime/monotonic clocks. vDSO access remains observable and may expose additional clocks. There is no private epoch or complete clock-observation denial. Relative sleeps are bounded to 300 seconds; supported absolute waits have a 300-second monotonic budget. Scheduling/suspension can delay return. |
| Identity | Configured `sysinfo` is a zero-filled x86-64 structure, not host RAM/load/uptime or synthetic resource accounting. `uname` identifies the configured implementation; it does not promise general Linux ABI compatibility or complete host-topology anonymity. |
| Readiness | Configured `poll` handles at most 16 descriptors through supported virtual owners; `pselect6` covers the bounded 256-entry public descriptor table and supports temporary signal masks. Image-file readiness is unsupported. Finite waits are at most 300000 ms. `ppoll` accepts a null signal mask and rounds to milliseconds. Cancellation/teardown is a separate supported lifecycle. Named FIFOs remain unsupported; GNU make uses its automatic pipe jobserver fallback. Unsupported `splice` endpoints return `EINVAL` without consuming input, allowing tools such as grep to use ordinary reads. |
| Network | The coordinator resolves declared hostnames once into a bounded hostname map and numeric TCP grants. Authority covers IP:port, including other virtual hosts on that address; it does not isolate DNS names, TLS/HTTP identities or URL paths. No UDP/DNS service, listener, proxy or native fallback is granted. Redirect destinations also need declarations. |
| Files | Explicit inputs are immutable snapshots; writable work/output is assigned storage. Creation stamps realtime access/modification times; nonempty writes and truncation update modification times. Reads leave access times unchanged. Archive modification times use whole seconds. Archive nanosecond fidelity, arbitrary inode kinds and general Linux filesystem behavior are not promised. Target UID/GID, modes, links and capabilities/xattrs use the separate validated [target metadata contract](target-metadata.md). |
| Proc metadata | Exact `/proc/self/fd/N` readlink reports a currently tracked domain name for a live public filesystem descriptor owned by the caller; it grants no reopen or host-FD access. Exact `/proc/self/maps` read opens a read-only snapshot of only that process's initial stack mapping, charged to existing VFS quotas. Other mappings, host paths, proc enumeration, mutation and mmap are unavailable. |
| Path descriptors | Configured `O_PATH` retains metadata-only references under the ordinary public/filesystem FD quotas. Stat/access/readlink metadata, aliases and lifecycle are supported. Directory references can anchor relative open/exec/mutation under the same contract and storage checks as absolute paths. `fchmodat2` supports `AT_EMPTY_PATH` on owned references; direct `fchmod`, data I/O, enumeration, mmap, descriptor execution, fchdir and descriptor-path reopen remain denied. `O_NOFOLLOW` is unsupported. |
| Events | Configured event counters and epoll share the existing 16-object channel pool; an epoll instance has at most 32 registrations over already-owned counters/pipes/packet endpoints and granted TCP sockets. Public FD limits also apply. Host/image/filesystem descriptors and nested epoll watches are unsupported; no host eventfd/epoll is exposed. Finite waits are at most 300000 ms; temporary signal-mask changes and general signal interruption/restart semantics are not promised. |
| Publication | Consumers require successful worker, supervisor and validated archive completion. A final path's existence alone is insufficient. Published generations retain coordinator ownership and private directory permissions. They are not automatically pruned with checkpoints. |

Explicit contract denial precedes metadata allocation and effects. The selected
Ginux source's `docs/configurable-contract.md` owns precise supported flags,
errors, alias lifetimes and edge/one-shot behavior; this is a configured subset,
not a general procfs, descriptor or epoll ABI.

The selected image must contain each actual tool and its libraries. A helper's
registration or a tool's `--help`/`--version` does not establish that the full
consumer works. Whole-coordinator bootstrap privileges remain required; see
[Linux setup](setup-linux.md). Recipe activates user-package publications in an
owned profile; recab owns OS image and A/B activation. Package helpers do not add a general package solver or
global ownership database. Controlled autofix checks establish retry mechanics,
not external AI-provider interoperability.
The installer UI's PTYs run on the libvirt guest's Linux kernel. Their acceptance
does not establish general PTY support inside configured Ginux recipe workloads.

Hostname resolution has an eight-endpoint total budget. Explicit numeric grants
are retained first. Sorted hostname grants each receive at least one address;
remaining slots are distributed round-robin in numeric address order (IPv4
before IPv6). The selected subset is identical in Ginux's endpoint grants, the
immutable `/etc/hosts` view and the invocation's `authority.json` receipt
(`pinned_endpoints` and `host_endpoints`). Selection warns when addresses are
omitted; DNS answer order does not affect it. If explicit grants or one address
per hostname cannot fit, launch fails. Availability of omitted addresses and
runtime DNS refresh are not promised. Ordinary publisher TLS verification is
unchanged.

The superproject's `docs/ginux-contributions/prd08-public-contracts.md` records
the detailed policy and original source-bound receipts. Current release and VM
acceptance remain separately tracked in `docs/ginux-contribution-task-list.md`;
earlier receipts do not certify a later delivered source/image combination.

Manual `isacquired`, `isbuilt` and `isinstalled` checks require matching cached
products from every dependency, including build dependencies. Dependencies run
only their installed checks, in dependency order; acquire/build/install hooks
never run to repair a cache miss. The same checked product snapshots restore the
parent's installed input identity. Missing or changed dependency inputs/products
fail explicitly; use `install` separately to prepare them. These checks preserve
separate dependency authority and caller restrictions.

Explicit `remove`/`cleanup` follows that same cached-dependency rule, restoring
the parent's matching saved state/files. Missing or changed dependency products
fail before the main hook, preserving its prior state and publications. Neither
operation runs dependency acquire/build/install or remove/cleanup hooks. Use an
explicit install to prepare unavailable matching dependency products.
