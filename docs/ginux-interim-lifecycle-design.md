# Stage 3 design: interim public-lifecycle commands in the ginux fork

Scope: drafts U4, U6, U2, U3, U1 of `tools/recipe/docs/ginux-public-lifecycle-migration.md`
(section 5), implemented additively in `LevitateOS/ginux:main` at `e3b71ac0`.
Every citation below was read at that commit. "helper" = `scripts/ginux-host-instance-helper.sh`
(installed as `/usr/libexec/ginux/ginux-helper` by `packaging/rpm/ginux.spec.in`, `%install`).
"CLI" = `crates/ginux-cli/src/lib.rs`. "image tool" = `crates/ginux-oci` (installed as
`/usr/libexec/ginux/ginux-image`). "runtime" = `crates/ginux-init` (installed as `ginux-runtime`).

Fork rule carried into every file: no mention of Recipe or LevitateOS in code, comments, docs,
tests or commit text; no use of `launch`; no change to catalog, digests or contract; no existing
command changes its output, exit codes or behaviour when the new flags/commands are not used.

---

## 0. Findings that shape the design (surprises and contradictions)

1. **The lifecycle cannot classify an outcome today, and the doc understates this.** The
   lifecycle runs `ginux-runtime native RUNTIME.toml ROOT configured` (helper `run_runtime`,
   line 969), not `launch`. That path exits with the workload code or `128+signal` at its single
   normal exit (`crates/ginux-init/src/main.rs:6086`, status computed at 6016-6020) but exits
   `1` on any supervisor failure (`instance_fail`, main.rs:11585-11591). `script -e` then maps a
   killed runtime to `128+n` as well. So `exit.status` (helper:887) cannot tell "workload exited 1"
   from "supervisor failed", nor "workload got SIGSEGV" from "runtime crashed". The migration doc
   (N9 "partial", U4 "Evidence") says `launch --result-file` "already writes exactly this record
   upstream": true (`git show b729915b:crates/ginux-init/src/launch.rs` lines 576-596), but only
   for the `launch` selector. U4 therefore needs a small additive runtime change (WP4) on the
   `native ... configured` path, written independently of `launch.rs`'s writer.
2. The runtime's own end-of-run lines (`GINUX_NATIVE: ...`, `GINUX_INSTANCE: failed reason=`,
   main.rs:6076-6085, 11587) go to fd 1, which under `script(1)` is the same PTY as the workload's
   virtual console descriptors (main.rs:5868, `INSTANCE_CONSOLE_FD` dup of host fd 0). They land
   in `console.log` interleaved with workload output and are forgeable by the workload; they
   cannot be used to derive the outcome.
3. **Only `import`/`create` take the per-instance lock** (helper:470-473, `flock -x 6`). `start`,
   `set`, `lock`, `unlock`, `stop` do not. A `delete` or `extract` racing a `start` could rename
   or read a root that a new runtime is entering. `start` must take the same lock (WP1).
4. **`stop` escalation can orphan the runtime.** After 60 s it sends TERM, then KILL, to the
   supervisor pid only (helper:1176-1182); the supervisor's TERM trap kills the runtime
   (helper:865-881) but a KILL does not run the trap. `delete` and `extract` must therefore check
   `runtime_identity` (helper:130) as well as `supervisor_identity` (helper:94).
5. **Gate 26 forbids "a redundant assembled rootfs.tar beside the source OCI layers"**
   (`hdae/gates-todo/26-rpm-packaging.md:36-38`; `docs/packaging.md` "State and lifecycle").
   U2(b) "clone an already expanded base root" is a persisted redundant expansion in spirit.
   See open question Q3.
6. `materialize` re-hashes every cached blob on each check (`cached_blob`, ginux-oci lib.rs:259-269,
   called from `image_is_fully_cached` 229 and again from `fetch_blob` 288) and reads each layer
   twice more in `apply_archive` (426-454). Each create reads every layer about four times; this is
   the real cost U2(b) removes.
7. `tar` 0.4.46 (the locked version) unpacks character/block/FIFO entries as **regular files**
   (no mknod; `~/.cargo/registry/.../tar-0.4.46/src/entry.rs` falls through to the regular-file
   branch after line 611). `prepare_root` creates `/dev/{console,null,zero,random,urandom}` only
   `if test ! -e` (helper:392-398), so an image that ships `dev/null` gets an empty regular file
   there. Names starting `.wh.` are whiteouts and are never unpacked (lib.rs:475-477, 533-537).
8. `parse_reference` requires a registry component even for a fully cached image (lib.rs:33-38);
   an imported image is named e.g. `local/image@sha256:HEX`. When every blob is cached,
   `materialize` makes no network request (lib.rs:78-82, 288-291) but still builds a client.
9. There are **no sudoers, AppArmor, man-page or completion files** in the tree. AppArmor appears
   only in `launch.rs` error text (1294, 1315). Packaging is `packaging/rpm/{ginux.spec.in,
   ginux.tmpfiles,ginux.rpmlintrc}` only. No new binary is needed, so the spec does not change.
10. The CLI pins its command list in a unit test (`public_commands_are_exactly_the_approved_set`,
    lib.rs:553-566), `scripts/test-cli.sh:18` loops over the same list, and
    `docs/cli-command-model.md` says `delete` is "outside Gate 25". The fork must edit all three;
    the doc change goes in a separately headed section so upstream merges stay readable.
11. `scripts/check-security-inventory.sh:12-40` fails CI if a new file contains `unsafe`; every new
    file with `libc` calls must be added to its allowlist. `docs/security-assurance.md` route and
    state tables are grep-checked by the same script (lines 69-89) and must gain rows.
12. `GINUX_STATE_ROOT`/`GINUX_RUN_ROOT` are read from the environment (helper:4-5); `sudo`'s
    env_reset drops them, so the CLI cannot use them. U3 needs explicit protocol options.

---

## 1. Shared protocol decisions (all packages depend on these; fixed here)

### 1.1 No protocol version bump

The helper keeps `--protocol 1` (helper:1187-1191). Everything is additive:

- New helper verbs `delete`, `extract`, `import-image`; new options `start ... --wait PID START`,
  `status ... --format json`; new leading options `--state-root DIR`, `--run-root DIR` placed
  between `--protocol 1` and the verb.
- New CLI + old helper: an old helper rejects unknown verbs (`*) usage`, helper:1215), unknown
  `start`/`status` options (helper:795, 720) and a leading `--state-root` (it becomes `$command`
  and hits `*) usage`) with exit 64 **before any state change** (the only pre-dispatch mutation is
  `mkdir -p "$state_root/instances"`, helper:1196, on the default root). The CLI maps helper exit 64
  on a new verb/option to `ginux: the installed helper does not support <feature>` and exit 69.
- Old CLI + new helper: no old argv changes meaning; `scripts/test-cli.sh:64-87` (expected argv for
  `create`) stays byte-identical because no root option is forwarded unless given.
- A bump to `2` would break old-CLI/new-helper pairs for no gain; the Gate 25 requirement
  ("incompatible caller fails before any privileged state change", `docs/cli-command-model.md`
  privileged-helper section) is already met by the allowlist.

### 1.2 Helper argv grammar (exact)

```
ginux-helper --protocol 1 [--state-root DIR [--run-root DIR]] VERB ARG...
  delete NAME
  extract NAME --max-bytes N --max-members N --max-path N PATH...      (tar on stdout)
  import-image                                                          (tar on stdin, digest on stdout)
  start NAME [--attach] [--audit] [--wait CALLER_PID CALLER_START]      (--wait excludes --attach)
  status NAME [--format json]
```

The CLI always sends the three `extract` bounds explicitly (defaults live only in the CLI).

### 1.3 Image tool argv (exact; WP3 implements, WP1 calls)

```
ginux-image materialize IMAGE CACHE_ROOT DESTINATION                (unchanged, main.rs:7-14)
ginux-image import-layout CACHE_ROOT          stdin: tar of an OCI layout; stdout: "sha256:HEX\n"
ginux-image export-tree ROOT MAX_BYTES MAX_MEMBERS MAX_PATH PATH...   stdout: tar
```

### 1.4 Runtime argv (exact; WP4 implements, WP1 calls)

```
ginux-runtime native CONFIG ROOT configured --result-file PATH     (argc 7, new)
```

All other `native` forms unchanged (launch.rs:117-134).

### 1.5 Outcome record (one format everywhere)

Exactly the `launch --result-file` version-1 shape (launch.rs:741-744), one line, `\n`-terminated:

```
{"version":1,"outcome":"exited","code":3,"signal":null,"generation":null}
{"version":1,"outcome":"signaled","code":null,"signal":9,"generation":null}
{"version":1,"outcome":"supervisor-error","code":null,"signal":null,"generation":null}
```

Validation regex used by the helper before trusting a record:
`^\{"version":1,"outcome":"(exited|signaled|supervisor-error)","code":(null|[0-9]{1,3}),"signal":(null|[0-9]{1,2}),"generation":null\}$`.

### 1.6 Exit codes (new commands)

| Code | Meaning | Used by |
|---|---|---|
| 0 | success | all |
| 2 | CLI parse error, no sudo call | all (clap, as `docs/cli-command-model.md` "Errors") |
| 64 | helper usage | helper |
| 65 | invalid state, invalid layout/digest, unsupported member type, path not a directory | delete, extract, import-image |
| 66 | instance (or extract PATH, or import input) not found | all |
| 69 | unsupported (old helper, missing compiled image tool, `openat2` ENOSYS) | all |
| 70 | internal failure; for `start --wait`, outcome `supervisor-error` | all |
| 73 | instance running / already running | delete, extract, start |
| 74 | output I/O error (destination write, EPIPE) | extract, import-image |
| 75 | member changed during export (retryable) | extract |
| 76 | extract bound exceeded (bytes, members, path length, depth, link length) | extract |
| 0..255 | `start --wait` only: workload exit code; `128+signal` when signaled | start --wait |

`start --wait` codes overlap by nature; the caller disambiguates with `status --format json`
(section 2.1).

---

## 2. Features

### 2.1 U4 — `start --wait` and the outcome

**CLI surface.**

```
ginux start NAME [--attach] [--audit] [--wait]       (--wait conflicts_with attach)
ginux status NAME [--format text|json]                (default text = today's output)
```

- `start --wait`: stdout empty; stderr carries helper diagnostics (unchanged wording for shared
  failures: `NAME: not found` 66, `NAME: already running` 73). Exit: workload code, or `128+signal`,
  or 70 for `supervisor-error`. Teardown on caller death or signal (below). Matches U4(a): foreground,
  workload code or 128+signal "as the runtime does", tied to the caller.
- `status --format json` prints one JSON object, one line:
  `{"version":1,"name":"N","state":"running|stopped","contract":"locked|unlocked","pid":123|null,`
  `"outcome":<record from 1.5>|null,"paths":{"config":"…","deployment":"…","console_log":"…",`
  `"audit_log":"…","root":"…"}}`. `outcome` is null before the first start, after a `start` that
  failed its pre-launch checks with the old record cleared, while running, and after a run whose
  record could not be written. Matches U4(b) "readable after the fact from status, stable form (U8)";
  the record survives until the next `start` or `delete` because it lives in the run directory and
  `start` removes it (helper:816-821 list gains `outcome result.runtime`).
- U4(c) (bounded force-stop) is delivered as the `--wait` teardown, not a new `stop` flag: the draft
  names no shape and the cancel path the consumer needs is "kill the waiting process". See Q8.

**Caller binding.** The CLI does not `exec` sudo (unlike `attach_instance`, lib.rs:416-419):
a caller's `PR_SET_PDEATHSIG` on the `ginux` process survives exec of the non-setuid `ginux` but
would be cleared by exec of setuid `sudo`. Instead (new `crates/ginux-cli/src/wait.rs`, uses `libc`):

1. Read own pid and start time (`/proc/self/stat`, field 22 counted after the last `)`).
2. Block `SIGINT SIGTERM SIGHUP SIGQUIT SIGCHLD` with `pthread_sigmask`; spawn
   `sudo HELPER --protocol 1 [roots] start NAME [--audit] --wait PID START` (Rust's spawn resets the
   child's mask; verify in the integration test).
3. Loop on `sigwait`: a terminating signal is forwarded with `kill(sudo_pid, sig)` (sudo relays it
   to the helper); `SIGCHLD` → `try_wait`; on exit return the helper's code.

Helper `start_instance` (helper:779) with `--wait`:

1. Parse `--wait PID START` (both `[0-9]+`; `--attach` with `--wait` → usage 64).
2. `lock_instance "$name"` (new, section 2.3) before the existence/running checks (helper:802-806);
   also refuse 73 if `runtime_identity` is alive.
3. Existing setup (helper:815-825) plus `rm -f outcome result.runtime`.
4. `nohup "$0" --protocol 1 supervise "$name" "$start_audit" 6>&- >/dev/null 2>&1 </dev/null &`
   (the `6>&-` keeps the lock out of the supervisor; apply to the non-wait path too).
5. `record_supervisor`, then release the lock (`exec 6>&-`).
6. Non-wait: unchanged `sleep 1` path (helper:830-841). Wait mode: `trap teardown HUP INT TERM`;
   loop `while kill -0 "$pid"; do caller_alive || teardown; sleep 0.2 & wait $!; done` (the
   `wait $!` keeps traps responsive); then `wait "$pid"`; map `outcome` to the exit code (1.6).
7. `caller_alive`: `/proc/$PID/stat` exists and its start time (field 22 after the last `)`; do not
   reuse `awk '{print $22}'` from helper:108, which miscounts on a comm with spaces) equals START.
8. `teardown`: `trap - HUP INT TERM`; if `supervisor_identity` still matches, `kill -TERM $pid`
   (its trap TERMs the runtime, waits 2 s, KILLs it, helper:865-881; tracees die by
   `PTRACE_O_EXITKILL`, main.rs:758; brokers by `PR_SET_PDEATHSIG`, main.rs:5473, 5518); poll 5 s;
   then KILL supervisor and any live `runtime_identity`; write `supervisor-error` if no `outcome`;
   remove pid files; `wait $pid`; exit per outcome. Total bound about 7.5 s.

Helper `supervise_instance` (helper:844): new function `write_outcome` called on the normal path
(after helper:887, keep `exit.status` exactly) and inside `terminate_supervisor` (before
helper:879): if `result.runtime` matches the 1.5 regex, copy it; else write `supervisor-error`; write
to `outcome.tmp.$$` mode 0600 then `mv`. `stop_instance` (helper:1150): after the KILL escalation
(helper:1180-1182) call `write_outcome` if no `outcome` exists. Its output is unchanged.

Helper `run_runtime` (helper:969): `exec "$runtime" native "$directory/runtime.toml"
"$directory/root" configured --result-file "$runtime_directory/result.runtime" 2>>...`. A hook
failure with `failure=stop` (helper:963-965), a config error, a runtime crash or a teardown leaves no
`result.runtime`, so the outcome is `supervisor-error`, which is what the draft asks.

Helper `status_instance` (helper:719): accept `--format json` (arity 1 or 3). JSON is safe to build
in shell because names match `[A-Za-z0-9_.-]` (helper:27-34) and root paths are restricted to
`[A-Za-z0-9_./-]` (2.4); the outcome is embedded only if it matches the 1.5 regex, else `null`.

**Runtime (WP4).** `crates/ginux-init/src/launch.rs:117-134`: accept `argc == 7` when
`argv[4] == "configured"` and `argv[5] == "--result-file"`; carry the path as a new
`Command::NativeResult(config, root, path)` variant (enum at launch.rs:44). `main.rs:283` dispatches it
to `native_main(config, root, 3, None)` after storing the pointer in a static owned by a new module
`crates/ginux-init/src/native_result.rs`. At the single normal exit (main.rs:6086, raw `status` in
scope) call `native_result::finish(status)`: if a path is stored, `openat(AT_FDCWD, path,
O_WRONLY|O_CREAT|O_EXCL|O_NOFOLLOW|O_CLOEXEC, 0600)`, write the 1.5 record (`exited` with
`status>>8 & 0xff`, or `signaled` with `status & 0x7f`), `fsync`, `close`; on any failure print a
`ginux:` line to stderr and `exit_process(1)` (absence then reads as `supervisor-error`). The file is
opened only after the workload is gone, in a root-owned 0700 directory, so no workload descriptor or
path race exists. Hooks (`NativeHook`) never get a result path. Poweroff from systemd is turned into
`exit_group(0)` (main.rs:9137-9140) and reaches the same exit, so a clean systemd shutdown records
`exited 0`. No other `exit_process` lies inside `supervise_native` (main.rs:5661-11120 has one, at
6086). `native_result.rs` joins the unsafe allowlist.

**State.** `/run/ginux/instances/NAME/` (or the private run root), created by `start`
`install -d -m 700` (helper:815): `result.runtime` (runtime, root 0600, O_EXCL), `outcome` (supervisor,
root 0600, atomic rename), `exit.status` (unchanged). `start` removes all three after its checks;
`delete` removes the directory.

**Threat note.** The workload cannot reach the run directory (root 0700, outside its chroot). A
spoofed `CALLER_PID` only ties the instance to another process's life, which the caller could end
anyway. A SIGKILL to the helper (only root can) leaves today's detached behaviour. The waiting helper
never reads instance-root paths.

### 2.2 U6 — result export: `ginux extract`

**CLI surface.**

```
ginux extract NAME --output FILE [--max-bytes N] [--max-members N] [--max-path N] PATH...
```

- `PATH`: one or more absolute directory paths inside the instance (`/` allowed). CLI validation
  (exit 2): absolute, no empty, `.` or `..` component, no NUL, at most `--max-path` bytes. Duplicates
  and paths below another listed path are dropped; the rest are sorted bytewise.
- `--output FILE` required; must not exist. Defaults: `--max-bytes 68719476736` (64 GiB),
  `--max-members 1000000`, `--max-path 4096`. Depth bound is fixed at 256 directories.
- stdout empty; stderr one `ginux: extract: …` line on failure naming the path and cause; exit codes 1.6.
- "Nothing partial in the destination": the CLI (`crates/ginux-cli/src/extract.rs`, with `libc`
  in `sys.rs`) opens `O_TMPFILE|O_RDWR` 0600 in FILE's directory, passes it as the helper's stdout,
  and on helper exit 0 `fsync`s and links it with `linkat(AT_FDCWD, "/proc/self/fd/N", AT_FDCWD, FILE,
  AT_SYMLINK_FOLLOW)` (EEXIST → 73-style refusal "output exists", exit 74). On failure the unnamed
  inode disappears, even on SIGKILL. If the filesystem refuses `O_TMPFILE` (EOPNOTSUPP/EISDIR),
  fall back to a `create_new` file `.FILE.ginux-partial.PID`, `std::fs::hard_link` to FILE (fails if
  FILE exists) and unlink; this fallback can leave the dot-file behind on SIGKILL (documented).
- Why this matches U6: named absolute directories of a stopped instance → one tar stream; type,
  link, mode, path and bound rules below; deterministic order; no partial output.
- Name: the draft leaves it to upstream; `export` already prints the recipe (lib.rs:277-279). See Q1.

**Helper `extract`** (new function `extract_instance`): parse exact grammar (1.2; bounds `[0-9]+`,
non-zero), `validate_name`; `lock_instance`; `test -d "$directory"` else 66; refuse 73 if
`supervisor_identity` or `runtime_identity` is alive (finding 4); `test -x "$image_tool" && test ! -d
"$image_tool"` else 69 (fixture fallback has no exporter, helper:15-17); run
`"$image_tool" export-tree "$directory/root" MAXB MAXM MAXP PATH...` with stdout inherited, holding
fd 6 for the whole run so `start`/`delete`/`import` wait (all take the lock). Propagate its exit code.

**Image tool `export-tree`** (new `crates/ginux-oci/src/export.rs`, `libc` for `openat2`,
`fstatat`, `readlinkat`, `fdopendir`, `setres[ug]id`, `prctl`):

1. `root_fd = open(ROOT, O_PATH|O_DIRECTORY|O_NOFOLLOW|O_CLOEXEC)`. ROOT's own name is safe to
   resolve: its parent `instances/NAME` is root-owned (staging `mkdir`, helper:554, moved at 565)
   and only `root` itself is chowned 65534 (helper:401).
2. If euid is 0: `setgroups(0)`, `setresgid(65534×3)`, `setresuid(65534×3)`,
   `prctl(PR_SET_DUMPABLE, 0)`, `prctl(PR_SET_NO_NEW_PRIVS, 1)`. From here the walker can read only
   what host uid 65534 (the workload identity, helper:399-401) can read, so a resolver bug cannot
   disclose host files. Non-dumpable blocks ptrace by other 65534 processes. (Skipped when not root,
   so unit tests run unprivileged.)
3. Every lookup is `openat2(parent_fd, name, how)` with `resolve = RESOLVE_BENEATH |
   RESOLVE_NO_SYMLINKS | RESOLVE_NO_MAGICLINKS | RESOLVE_NO_XDEV`, flags
   `O_NOFOLLOW|O_CLOEXEC` plus `O_DIRECTORY|O_RDONLY` for directories or
   `O_RDONLY|O_NONBLOCK|O_NOCTTY` for regular files. ENOSYS → 69. Requested PATHs are opened the same
   way relative to `root_fd` (any symlink component → ELOOP → 65 "symlink in requested path";
   ENOENT → 66; ENOTDIR → 65).
4. Walk: per directory, read all names (`fdopendir(dup(fd))`), skip `.`/`..`, abort with 76 once
   the running member count passes MAX_MEMBERS (bounds memory too), sort bytewise, then for each name
   `fstatat(fd, name, AT_SYMLINK_NOFOLLOW)`:
   - directory: open as in 3, `fstat`, require same `(st_dev, st_ino)` else 75; depth > 256 → 76;
     emit `name/` member; recurse (one open fd per level, at most 257).
   - regular file: open as in 3, `fstat`, require `S_ISREG` and same `(dev, ino)` else 75; add
     `st_size` to the byte total **before** writing the header (exceeding MAX_BYTES → 76); write
     header, copy exactly `st_size` bytes; short read → 75; one extra byte readable after → 75 (grew).
     `st_nlink > 1` is ignored: each name is emitted as its own regular member, so **no hardlink
     member is ever written** (duplicated bytes count against MAX_BYTES).
   - symlink: `readlinkat` into a 4096-byte buffer; length ≥ 4096 → 76; length differs from
     `st_size` → 75; emit a symlink member with the target bytes verbatim; never followed.
   - FIFO, socket, character or block device: 65 "unsupported member type: PATH". (A FIFO is never
     opened, so it cannot block the exporter.)
   - Any member path (relative, no leading `/`) longer than MAX_PATH → 76.
5. Header rules (via `tar::Builder` with `Header::new_gnu()`; long names and link targets through
   GNU LongName/LongLink as `append_data`/`append_link` do): mode `st_mode & 0o777` (setuid, setgid,
   sticky **cleared**), uid/gid 0, empty user/group names, device numbers 0, mtime `max(st_mtime, 0)`,
   no xattrs, no PAX global headers. The requested directory itself is emitted (`out/`); its
   ancestors are not. Then `Builder::finish()` and a checked flush (EPIPE/EIO → 74).
6. Output is identical for identical trees (sorted names, normalized headers).

**Can `storage_export.rs` be reused?** No, beyond its ideas. It is a `no_std` exporter inside the
runtime that walks owned memory storage via `ginux_vfs::FileMetadata` and raw syscalls
(`crates/ginux-init/src/storage_export.rs:1-5, 4-5`), writing ustar with GNU LongLink
(lines ~290-330 of the file) to an `O_TMPFILE` it publishes by `linkat`. The public root is a host
directory written by an untrusted uid, so the hard part (race-free host traversal) does not exist
there. Reuse: the `O_TMPFILE` + link publication idea (moved to the CLI side) and the LongLink header
layout (the `tar` crate already provides it). The walker is new code.

**Threat note.** Adversary: the workload (uid 65534) and any other host process running as 65534
(e.g. another instance's workload, other `nobody` daemons) that can modify the root while export
runs. Symlink swaps: refused by `RESOLVE_NO_SYMLINKS` on every component plus inode re-check
(75). `..` or absolute names: names come from `readdir`, never contain `/`, and `RESOLVE_BENEATH`
pins them below the parent fd. Hardlinks to host files: impossible to read outside 65534's rights after
the privilege drop, and never emitted as hardlinks. Special files: never opened. Huge sizes and
counts: bounded with a distinct 76. Growing/shrinking files: exact-size copy with 75. Mount points:
`RESOLVE_NO_XDEV`. Bind mounts the host put inside the root are not crossed. A `chmod 000` file or
directory makes the export fail with a readable error; the workload can only spoil its own export.
The helper never follows instance paths itself; it passes only `$directory/root`.

### 2.3 U2 — `ginux delete` and cheap create

**CLI surface.** `ginux delete NAME` → stdout `Instance "NAME" deleted.` exit 0; `NAME: not found`
66; `NAME: stop the instance before deleting it` 73; removal failure after the atomic rename 70
(instance already gone from `list`). Matches U2(a): refused while running, atomic, removes recipe,
deployment state, root, logs, run state; `not found` is the idempotent error.

**Helper.**

- New `lock_instance NAME` (replaces helper:470-473 inside `import_instance`, and is used by
  `start`, `delete`, `extract`):
  ```
  lock=$state_root/work/instance.$1.lock
  while :; do exec 6>>"$lock"; flock -x 6
    test "$(stat -L -c %d:%i "$lock" 2>/dev/null)" = "$(stat -L -c %d:%i /proc/$$/fd/6)" && break
    exec 6>&-; done
  ```
  The inode re-check makes it safe for `delete` to unlink the lock file while holding it.
- New `delete_instance` (dispatch `delete) delete_instance "$@" ;;`): exact arity 1;
  `validate_name`; `lock_instance`; entry absent (`test ! -e && test ! -L`) → 66; running
  (`supervisor_identity` or `runtime_identity`) → 73; `trash=$(mktemp -d
  "$state_root/instances/.delete:XXXXXX")` (0700 root; `list` ignores dot names, helper:701);
  `mv -T --no-copy -- "$directory" "$trash/instance"` (one `rename(2)` in one directory: atomic);
  `rm -rf -- "$run_root/instances/$name"`; `rm -f -- "$lock"`; release fd 6; print the message;
  then `rm -rf --one-file-system -- "$trash"` (failure → message, exit 70); finally best-effort
  `rm -rf --one-file-system` of other `.delete:*` older than 1 hour (left by crashes).
- Threat note: GNU `rm -rf` removes symlinks without following them and walks descriptor-relative
  (fts), so a symlink or rename race inside the root cannot redirect it; `--one-file-system` stops
  at mounts; the renamed tree is reachable only through a root-0700 directory, so path-based meddling
  stops at the rename. A 65534 process holding a cwd/fd inside the tree can keep creating files and
  make `rm` fail (exit 70, garbage swept later); it cannot make root delete anything outside.
  Two racing `delete`s serialize on the lock; the loser sees 66.

**Cheap create (U2(b)).** New helper function `materialize_root REFERENCE DEST` replaces the single
call at helper:559:

1. `hex` = the 64 hex digits after `@sha256:` (validated). `roots=$state_root/cache/roots`
   (`mkdir -m 700`, root).
2. Reflink probe: write a 1-byte file in `$roots`, `cp --reflink=always` it into the staging parent
   (`$state_root/instances/.create:…`, helper:547); failure → step 5.
3. Under `flock` on `$state_root/work/root.$hex.lock`: if `$roots/sha256/$hex` is absent,
   `materialize_image REFERENCE "$partial/root"` into `mktemp -d "$roots/.partial:XXXXXX"`, then
   `mv -T --no-copy --update=none` to `$roots/sha256/$hex` (rename = completeness marker).
4. `cp -a --reflink=always "$roots/sha256/$hex/." "$DEST/"`; then the existing
   `root_path_is_executable` and `prepare_root` run on DEST as today (helper:560-564). The base is
   the pristine expansion, never prepared, never visible to a workload.
5. No reflink: `materialize_image REFERENCE DEST` exactly as today; nothing is persisted.

Isolation (Gate 17): reflink copies share no inode with the base or each other; `cp -a` keeps
symlinks unfollowed and hardlinks only within the copy; `prepare_root`'s `chown -R` (helper:401)
applies per instance. `delete` never touches `cache/roots`; an operator may remove any base at any
time (rebuilt on demand). Key is the reference digest, which fixes the platform choice
(`select_manifest`, lib.rs:354-378). Conflict with Gate 26: Q3.

### 2.4 U3 — private per-caller state: `--state-root`, `--run-root`

**CLI surface.** Global options on `Cli` (`#[arg(long, global = true)]`):
`ginux [--state-root DIR] [--run-root DIR] COMMAND …`. `--run-root` without `--state-root` is a parse
error; `--state-root` alone implies `--run-root DIR/run`. CLI validation (exit 2): absolute, only
`[A-Za-z0-9_./-]`, no `.`/`..`/empty component, ≤ 256 bytes. When given, the CLI inserts
`--state-root DIR --run-root RDIR` after `--protocol 1` in **every** helper call, including the
unprivileged `attach` exec (lib.rs:416-418) and `edit`'s export/import (lib.rs:437-456). The draft
asks for "a documented state-root option on the CLI"; this is it, with the run root the draft also
names. Instance names, `cache/oci`, locks, logs and run files all live under the chosen roots, so
two roots never collide.

**Helper.** New option parsing between the protocol check (helper:1187-1191) and `mkdir`
(helper:1196). New function `trusted_root DIR create`:

- Same charset/shape rules as the CLI.
- For DIR and every ancestor to `/` that exists: `test -d && test ! -L`, owner uid 0
  (`stat -c %u`), and `stat -c %a` has no group/other write bit (so `/tmp`, `/var/tmp` and home
  directories fail: Gate 26 non-goal "storing … in an invoking user's home directory").
- Missing levels are created one at a time with `mkdir -m 0755` (state) only after their parent
  passed; then `install -d` the layout from `packaging/rpm/ginux.tmpfiles`: `instances` 0755,
  `work` 0700, `cache/oci/blobs/sha256` 0755; run root `instances` 0755.
- Failure → `untrusted state root: DIR (REASON)` exit 64. The unprivileged `attach` verb checks but
  never creates.
- On success set `state_root`/`run_root` and `export GINUX_STATE_ROOT GINUX_RUN_ROOT` so the
  self-invocations `supervise` and `run-runtime` (helper:827, 862) inherit them; the
  `sudo -n "$0" --protocol 1 resize` call (helper:1021) must pass the options explicitly (sudo resets
  the environment).

**Threat note.** The helper is root; a path from the caller is the classic redirection vector (e.g. a
state root under a user-writable tree where `instances/NAME` is a symlink to `/etc`, then
`delete` or `prepare_root`'s `chown -R`). Requiring every component root-owned and not group/other
writable means only root can rename or plant entries anywhere on the path, so later path-based
helper operations (which are not descriptor-relative) cannot be raced. Isolation between callers is
by root choice, not enforced: anyone who may run the helper through sudo is administrator-equivalent
today (Gate 26 ships no sudoers rule), so this prevents collisions, not malice.

### 2.5 U1 — offline image import: `ginux import-image`

**CLI surface.** `ginux import-image PATH` where PATH is an OCI image-layout directory or an
uncompressed tar of one (gzip magic → exit 65 "compressed layout archives are not supported").
stdout: exactly `sha256:<64 hex>\n`, the image **manifest** digest; exit 0. After it,
`instance.root = "REGISTRY/REPO@sha256:<that>"` (any syntactically valid registry, e.g.
`local/image`) resolves with no network (finding 8). Matches U1: explicit, documented, verified,
manifest digest, cache unchanged on failure; the draft leaves command vs `ginux-image` entry point
open, and the cache is root-owned, so a public command through the helper is the only route.

The CLI reads PATH **with the caller's own rights** and streams a tar to the helper's stdin
(`crates/ginux-cli/src/image.rs`, `tar` crate): a directory is walked with `symlink_metadata`, and only
`oci-layout`, `index.json`, `blobs/`, `blobs/sha256/`, `blobs/sha256/<64 lowercase hex>` regular
files and directories are sent (anything else → 65 before sudo); a file is copied as-is. This keeps
the root helper from reading caller-named host paths (it would otherwise copy root-only files into
the cache and, via a crafted manifest, into an instance root).

**Helper `import-image`**: arity 0; `test -x "$image_tool" && test ! -d "$image_tool"` else 69;
`mkdir -p "$state_root/cache/oci/blobs/sha256"`; `exec "$image_tool" import-layout
"$state_root/cache/oci"` (stdin/stdout inherited).

**Image tool `import-layout`** (new `crates/ginux-oci/src/layout.rs`; makes `select_manifest`,
`descriptor`, `validate_digest`, `cached_blob`, `read_json` `pub(crate)` in lib.rs):

1. `staging = cache_root/.import-PID-NONCE` (`create_dir` 0700; same filesystem as `blobs/sha256`,
   so commit is `rename`).
2. Iterate the stdin tar with `tar::Archive::entries` (never `unpack`). Accept only the five path
   forms above (leading `./` stripped), entry types Regular/Directory; reject symlink, hardlink,
   device, FIFO, PAX global, sparse, duplicate names, unknown paths (65). Limits: ≤ 4096 members,
   `oci-layout` ≤ 4 KiB, `index.json` ≤ 4 MiB; blobs streamed into `staging/HEX` (`create_new`)
   while hashing; computed digest ≠ name → 65.
3. `oci-layout` must be JSON with `imageLayoutVersion == "1.0.0"`.
4. `index.json`: `manifests` array; one entry → it; several → `select_manifest` (linux/amd64, no
   variant). If the chosen descriptor is an index, resolve one more level the same way (depth 1).
   The final descriptor's `mediaType` must be `application/vnd.oci.image.manifest.v1+json` or
   `application/vnd.docker.distribution.manifest.v2+json`.
5. Every referenced blob (manifest, config, layers) must be in staging (the layout is complete; the
   cache is not consulted) with file size == descriptor `size` and name == digest. Manifest:
   `schemaVersion == 2`, config descriptor, non-empty `layers`, each `mediaType` in the set
   `apply_layer` handles (lib.rs:401-421) excluding the empty string. Config: JSON object with
   `os == "linux"`, `architecture == "amd64"`; if `rootfs.diff_ids` has the same length and a layer
   is uncompressed, its diff_id must equal its digest. JSON blobs ≤ 4 MiB (config ≤ 16 MiB) before
   parsing.
6. Commit in order layers → config → manifest: target `blobs/sha256/HEX`; if `cached_blob` already
   verifies it, drop the staged copy; else `fsync` and `rename` over it, recording it in `added`.
   Then `fsync` the `blobs/sha256` directory. Any error → unlink every `added` target, remove staging,
   exit 65/74/70. Because the manifest is committed last, an interrupted commit never leaves a
   manifest whose dependencies are missing; extra content-addressed blobs are harmless.
7. Print the manifest digest. Unreferenced layout blobs are discarded.

**What an assembled root filesystem tar must look like as a one-layer image layout.**

```
oci-layout                {"imageLayoutVersion":"1.0.0"}
index.json                {"schemaVersion":2,"manifests":[{"mediaType":"application/vnd.oci.image.manifest.v1+json",
                            "digest":"sha256:M","size":S_M,"platform":{"architecture":"amd64","os":"linux"}}]}
blobs/sha256/M            {"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json",
                            "config":{"mediaType":"application/vnd.oci.image.config.v1+json","digest":"sha256:C","size":S_C},
                            "layers":[{"mediaType":"application/vnd.oci.image.layer.v1.tar","digest":"sha256:L","size":S_L}]}
blobs/sha256/C            {"architecture":"amd64","os":"linux","rootfs":{"type":"layers","diff_ids":["sha256:L"]},"config":{}}
blobs/sha256/L            the root filesystem tar itself (uncompressed, so digest == diff_id)
```

Layer rules from the expander: member paths relative (no leading `/`, no `..`; `safe_tar_path`,
lib.rs:521-531); no names beginning `.wh.` (treated as whiteouts, finding 7); no device/FIFO entries
(become empty regular files, and suppress `prepare_root`'s device nodes); ownership in the tar is
ignored (unpacked as root, then `chown -R 65534`, helper:401, which also clears setuid bits and file
capabilities); modes, mtimes, symlinks, in-tar hardlinks and xattrs are kept (lib.rs:469-471). The
manifest digest is a hash of the exact manifest bytes, so the producer must write them byte-stable
(fixed key order, no whitespace) for the digest to be a stable identity.

**Threat note.** The stdin bytes are fully caller-controlled. Member names select nothing on disk
(only validated hex names in a root-0700 staging dir created with `create_new`); sizes are bounded
for metadata and streamed for blobs (disk exhaustion by a sudo-capable caller is possible but no
worse than `create`); layers are not decompressed at import; at materialize time imported layers have
the same trust as registry layers (digest pinned by the caller). Concurrent imports or fetches of the
same digest race only on `rename` of identical content.

---

## 3. Files to change, per work package (no file shared without an owner)

### WP1 — helper and lifecycle (owner of `scripts/ginux-host-instance-helper.sh`)

- `scripts/ginux-host-instance-helper.sh`: root options and `trusted_root` (before line 1196);
  `lock_instance`; `import_instance` lock lines 470-473 → `lock_instance`, line 559 →
  `materialize_root`; `start_instance` (`--wait`, lock, `6>&-`, rm list 816-821); `supervise_instance`
  (`write_outcome` at 879/887); `run_runtime` line 969 (`--result-file`); `stop_instance` (after
  1180-1182); `status_instance` (`--format json`); `attach_instance` resize call 1021 (pass roots);
  new `delete_instance`, `extract_instance`, `import_image`, `materialize_root`, `write_outcome`,
  `caller_alive`, `teardown`; dispatch table 1197-1216 gains `delete`, `extract`, `import-image`.
- `scripts/test-public-lifecycle.py` (new, root, offline; section 5).

### WP2 — CLI, docs and repository plumbing

- `crates/ginux-cli/src/lib.rs`: `Cli` global root args; `ConfigureCommand::Delete(NameArgs)`;
  `InspectInstanceCommand::Status(StatusArgs)` with `--format`; `InspectInstanceCommand::Extract(ExtractArgs)`;
  new heading `Manage images` with `ImageCommand::ImportImage(PathArgs)` (`import-image`); `StartArgs.wait`
  (`conflicts_with = "attach"`); one `helper_command()` builder used by `privileged_helper_status`
  (347), `attach_instance` (400, 416), `edit_instance_at` (437, 456); exit-64→69 mapping for new
  features; unit tests: extend lib.rs:559-563 list, add parser/negative tests for every new form.
- New `crates/ginux-cli/src/{wait.rs,extract.rs,image.rs,sys.rs}` (`unsafe` only in `wait.rs` and
  `sys.rs`).
- `scripts/test-cli.sh`: command loop line 18; fake-sudo argv checks for each new form; a check that
  no root option is forwarded when none is given (existing expected file unchanged).
- `docs/cli-command-model.md`: inventory markers (public `+delete extract import-image`, helper
  `+delete extract import-image`, runtime unchanged), tables, a new section "Interim lifecycle
  commands" with the shapes, exit codes (1.6), outcome record (1.5) and state layout; leave Gate 25's
  approved text as is above it.
- `README.md` (public workflow section), `docs/packaging.md` ("State and lifecycle": `cache/roots`,
  private roots, `delete`), `docs/security-assurance.md` (route rows "Instance delete", "Instance
  extract", "Image import"; state rows `cache/roots`, private roots; the grep list in
  `check-security-inventory.sh:69-89` must keep matching).
- `scripts/check-security-inventory.sh`: allowlist adds `crates/ginux-cli/src/sys.rs`,
  `crates/ginux-cli/src/wait.rs`, `crates/ginux-oci/src/export.rs`,
  `crates/ginux-init/src/native_result.rs` (WP3/WP4 hand over any other file that ends up with `unsafe`).
- `Makefile`: targets `test-public-lifecycle` (`python3 scripts/test-public-lifecycle.py ...`),
  `test-native-result`; `.github/workflows/check.yml`: `sudo make test-native-result` and
  `sudo make test-public-lifecycle` after `test-native-supervision` (line 84).

### WP3 — image tool (owner of `crates/ginux-oci/**`, all `Cargo.toml` dependency lines, `Cargo.lock`)

- First, alone, a dependency change: `libc = "0.2"` in `crates/ginux-oci/Cargo.toml`;
  `libc = "0.2"` and `tar = "0.4"` in `crates/ginux-cli/Cargo.toml`; `Cargo.lock` regenerated
  (both already locked: libc 0.2.191, tar 0.4.46, so the RPM vendor archive needs nothing new).
- `crates/ginux-oci/src/main.rs`: dispatch `materialize` (unchanged), `import-layout`, `export-tree`
  with exact arity, exit codes from 1.6.
- `crates/ginux-oci/src/lib.rs`: `pub mod export; pub mod layout;` and `pub(crate)` on the helpers
  listed in 2.5; no behaviour change to `materialize`.
- New `export.rs`, `layout.rs` with their unit tests.

### WP4 — runtime outcome record (owner of the `ginux-init` edits)

- `crates/ginux-init/src/launch.rs`: argv parse (117-134), `Command` enum (44).
- `crates/ginux-init/src/main.rs`: dispatch (283), exit hook (6086), `mod native_result;`.
- New `crates/ginux-init/src/native_result.rs`.
- New `scripts/test-native-result.py` (root; follows `scripts/test-native-supervision.py`).

Hand-offs: WP3 → WP2 the dependency commit (before WP2 compiles); WP3/WP4 → WP2 their list of
`unsafe` files and the two Make target command lines; WP1 → WP2 nothing (helper verbs are fixed in
1.2). WP1's integration test needs WP2–WP4 merged to pass, not to be written.

## 4. Implementation order

1. WP3 dependency commit (minutes; unblocks WP2).
2. In parallel, against sections 1.2-1.6: WP4 (runtime record), WP3 (`export-tree`, then
   `import-layout`), WP1 (helper: lock + U4, then U6 verb, then U2, U3, U1 verb), WP2 (CLI U4 → U6 →
   U2 → U3 → U1, then docs and plumbing).
3. WP1 writes `test-public-lifecycle.py` last; then everything is verified together (repository rule:
   no build or run between changes). Feature order inside each package follows the doc's filing order
   (U4, U6, U2/U3, U1).

## 5. Tests

| Feature | Test | Root? | Where |
|---|---|---|---|
| all CLI forms | clap parse/negative tests; `public_commands_are_exactly_the_approved_set` extended; `--wait`+`--attach` → 2; bad `--state-root` → 2 | no | `crates/ginux-cli/src/lib.rs` `#[cfg(test)]` |
| CLI → helper argv | fake `sudo` records argv for `delete`, `extract` (bounds always sent), `import-image`, `start --wait` (PID and start time numeric), `status --format json`, every verb with roots; no roots → unchanged argv | no | `scripts/test-cli.sh` |
| U4 signal forwarding | fake `sudo` that traps TERM and exits 7: `ginux start x --wait` killed with TERM exits 7 | no | `scripts/test-cli.sh` |
| U6 walker | deterministic bytes for a fixed tree; setuid file → mode 0o755 in tar; symlink to `/etc/passwd` emitted as link; symlink component in PATH → 65; FIFO → 65 (no hang); each bound → 76; hardlinked file → two regular members; long names round-trip through `tar::Archive` | no (drop skipped) | `crates/ginux-oci/src/export.rs` tests |
| U1 import | one-layer layout → digest printed, three blobs committed; then `materialize("local/t@sha256:M", …)` succeeds with no network (registry `invalid.invalid`); tampered byte, wrong size, symlink member, extra path, duplicate member, missing config, unsupported media type, nested index depth 2, gzip archive → error and `cache_root` listing identical before/after | no | `crates/ginux-oci/src/layout.rs` tests |
| U4 runtime record | `native … configured --result-file P` with fixture exiting 0, 3, and killed by SIGKILL → three records; existing P → exit 1, no workload run; no flag → no file, exit codes unchanged | yes | `scripts/test-native-result.py` (WP4) |
| end to end | private state root `mktemp -d /var/lib/ginux-test.XXXXXX`; untrusted root under `/tmp` → 64; build a one-layer layout around `.artifacts/launch/native-fixture`; `import-image`; `import` a recipe naming `local/t@sha256:M` (no network); `start --wait` → fixture code; `status --format json` outcome; fixture that self-signals → 128+n; recipe whose hook fails with stop → 70 and `supervisor-error`; caller SIGKILL → instance stopped within 10 s and outcome `supervisor-error`; `extract` while running → 73, stopped → tar listing and bytes as expected, output exists → 74, nothing left on failure; second `create` of the same image uses the base (on reflink filesystems: check `cache/roots/sha256/HEX` exists) or not (ext4: check it is absent); `delete` running → 73, stopped → 0, again → 66; `list` empty; also verifies sudo passes binary stdin/stdout (Q6) | yes | `scripts/test-public-lifecycle.py` (WP1) |
| inventory/docs | `check-cli-command-inventory.sh` (reads updated markers), `check-security-inventory.sh` (allowlist and grep list), `make check` clippy `-D warnings` for ginux-cli/ginux-oci, `cargo fmt --check` | no | existing scripts, unchanged except the allowlist |

`scripts/demo-gate-25.sh` needs no change (it checks only that private selectors do not leak).
`packaging/rpm/*` need no change: no new binaries, and new directories are created on demand.

## 6. Open questions (with recommended answers)

- **Q1. Command names not fixed by the drafts** (`extract`, `import-image`, `--state-root`/`--run-root`,
  `status --format json`; the drafts fix only `delete` and `start --wait`). If upstream names them
  differently the consumer must change. Recommend: keep these names, and keep every name in one
  table on the consumer side so a later rename is a one-line change there; say so plainly in the
  issue drafts.
- **Q2. Outcome of a teardown or a KILL-escalated `stop`.** Recommend `supervisor-error` with the
  unchanged version-1 shape (no `reason` field), because extra fields would break the "exactly this
  record" promise; the caller knows when it cancelled.
- **Q3. U2(b) versus Gate 26's "no redundant assembled rootfs".** Recommend the design above: base
  roots only where reflink works (no extra data blocks shared beyond the base itself), nothing
  persisted elsewhere, base removable at will, documented in `docs/packaging.md`. Alternative if the
  owner prefers strictness: no base at all, accept full expansion per create.
- **Q4. Touching `ginux-init` (WP4).** It is upstream's busiest file. Recommend doing it: one new
  module, one argv form, one call at the single exit; without it `supervisor-error` cannot be told
  from `exited 1`.
- **Q5. Non-root callers.** Everything still goes through `sudo` and Gate 26 ships no sudoers rule.
  Recommend: no change in the fork; a narrow sudoers entry is a host-packaging decision outside these
  files.
- **Q6. sudo with `use_pty` and binary pipes.** `extract` (stdout) and `import-image` (stdin) rely on
  sudo passing non-terminal stdio through unchanged; the existing `edit` already relies on stdout
  (lib.rs:437-442). Recommend: assert it in `test-public-lifecycle.py` on the CI runner (Ubuntu 24.04)
  and on a Rocky 10 host before relying on it.
- **Q7. Special files in `extract`.** Recommend failing (65) rather than skipping: deterministic and
  never silent. Revisit if real outputs contain sockets or FIFOs.
- **Q8. Separate `stop --force`.** Recommend not adding it now; the `--wait` teardown covers
  cancellation. Add only if a detached-start consumer appears.
- **Q9. `extract` mtimes.** Recommend keeping file mtimes (deterministic for an identical tree); a
  consumer wanting reproducible archives normalizes downstream.
- **Q10. Should `start --wait` stream `console.log` to stdout?** Recommend no in this stage: stdout
  stays empty, the log path is in `status --format json`; streaming would merge PTY line-discipline
  output into the caller's protocol.
- **Q11. Default `extract` bounds** (64 GiB, 1,000,000 members, 4096-byte paths, depth 256).
  Recommend these; callers pass their own.
- **Q12. Should private roots share one `cache/oci`?** Recommend no (each state root has its own),
  which keeps `materialize` untouched and avoids cross-root cache writers; importing the same image
  into several roots costs disk once per root.
