# Supported execution boundaries

The current configured contract targets Linux/x86-64. macOS and other native
targets are outside this release. Configured downstream support does not change
upstream/native Ginux policy. A catalog permission is a ceiling: it does not
implement every Linux syscall variant, flag or backend.

| Surface | Supported boundary |
| --- | --- |
| Memory | `NEEDS.memory_mib` bounds each process's virtual address space through inherited `RLIMIT_AS`; it is not a combined process-tree RSS or host/supervisor budget. Storage has separate byte/object accounting within the declared memory configuration. |
| CPU | Requested jobs select eligible real CPUs, preserving their actual IDs. This is affinity, not virtual topology, CPU-time or usage-percentage accounting. The per-instance deadline is separate. |
| Clocks | Raw queries support host realtime/monotonic clocks. vDSO access remains observable and may expose additional clocks. There is no private epoch or complete clock-observation denial. Relative sleeps are bounded to 300 seconds; supported absolute waits have a 300-second monotonic budget. Scheduling/suspension can delay return. |
| Identity | Configured `sysinfo` is a zero-filled x86-64 structure, not host RAM/load/uptime or synthetic resource accounting. `uname` identifies the configured implementation; it does not promise general Linux ABI compatibility or complete host-topology anonymity. |
| Readiness | Configured `poll` handles at most 16 descriptors through supported virtual owners; image-file readiness is unsupported. Finite waits are at most 300000 ms. `ppoll` accepts a null signal mask, rounds to milliseconds and does not promise general signal interruption/restart or temporary signal masks. Cancellation/teardown is a separate supported lifecycle. |
| Network | The coordinator resolves declared hostnames once into a bounded hostname map and numeric TCP grants. Authority covers IP:port, including other virtual hosts on that address; it does not isolate DNS names, TLS/HTTP identities or URL paths. No UDP/DNS service, listener, proxy or native fallback is granted. Redirect destinations also need declarations. |
| Files | Explicit inputs are immutable snapshots; writable work/output is assigned storage. Archive modification times use whole seconds. Nanosecond fidelity, arbitrary inode kinds and general Linux filesystem behavior are not promised. Target UID/GID, modes, links and capabilities/xattrs use the separate validated [target metadata contract](target-metadata.md). |
| Publication | Consumers require successful worker, supervisor and validated archive completion. A final path's existence alone is insufficient. Published generations retain coordinator ownership and private directory permissions. They are not automatically pruned with checkpoints. |

The selected image must contain each actual tool and its libraries. A helper's
registration or a tool's `--help`/`--version` does not establish that the full
consumer works. Whole-coordinator bootstrap privileges remain required; see
[Linux setup](setup-linux.md). Recipe does not activate a running system; recab
owns A/B activation. Package helpers do not add a general package solver or
global ownership database. Controlled autofix checks establish retry mechanics,
not external AI-provider interoperability.
The installer UI's PTYs run on the libvirt guest's Linux kernel. Their acceptance
does not establish general PTY support inside configured Ginux recipe workloads.

The superproject's `docs/ginux-contributions/prd08-public-contracts.md` records
the detailed policy and original source-bound receipts. Current release and VM
acceptance remain separately tracked in `docs/ginux-contribution-task-list.md`;
earlier receipts do not certify a later delivered source/image combination.
