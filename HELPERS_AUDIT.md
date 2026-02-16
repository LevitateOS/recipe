# recipe Helper Audit

**Last Updated:** 2026-02-14  
**Scope:** Rhai helpers exposed by `tools/recipe/src/helpers/mod.rs` (what recipe authors can call).

This document audits:

- What helpers are currently exposed to recipes
- Where we diverge from `tools/recipe/REQUIREMENTS.md` Section 6
- What is missing to support safe A/B composition into an inactive slot sysroot

**Verification note:** The helper list below is derived from
`tools/recipe/src/helpers/mod.rs` `register_all()` (currently 58 helpers).

---

## Exposed Helpers (Today)

These names are the Rhai function names.

| Rhai Helper | Rust Implementation | Signature (Rhai) | Notes |
|---|---|---|---|
| `join_path` | `helpers/util/paths.rs` | `join_path(a, b) -> String` | Pure join |
| `basename` | `helpers/util/paths.rs` | `basename(path) -> String` |  |
| `dirname` | `helpers/util/paths.rs` | `dirname(path) -> String` |  |
| `trim` | `helpers/util/string.rs` | `trim(s) -> String` |  |
| `starts_with` | `helpers/util/string.rs` | `starts_with(s, prefix) -> bool` |  |
| `ends_with` | `helpers/util/string.rs` | `ends_with(s, suffix) -> bool` |  |
| `contains` | `helpers/util/string.rs` | `contains(s, pat) -> bool` |  |
| `replace` | `helpers/util/string.rs` | `replace(s, from, to) -> String` |  |
| `split` | `helpers/util/string.rs` | `split(s, sep) -> Array` |  |
| `log` | `helpers/util/log.rs` | `log(msg) -> ()` | stderr formatting via `core/output.rs` |
| `debug` | `helpers/util/log.rs` | `debug(msg) -> ()` |  |
| `warn` | `helpers/util/log.rs` | `warn(msg) -> ()` |  |
| `shell` | `helpers/util/shell.rs` | `shell(cmd) -> ()` | uses `sh -c` |
| `shell_in` | `helpers/util/shell.rs` | `shell_in(dir, cmd) -> ()` | run in directory |
| `shell_status` | `helpers/util/shell.rs` | `shell_status(cmd) -> int` | returns exit code |
| `shell_status_in` | `helpers/util/shell.rs` | `shell_status_in(dir, cmd) -> int` |  |
| `shell_output` | `helpers/util/shell.rs` | `shell_output(cmd) -> String` | captures stdout |
| `shell_output_in` | `helpers/util/shell.rs` | `shell_output_in(dir, cmd) -> String` |  |
| `read_file` | `helpers/install/io.rs` | `read_file(path) -> String` |  |
| `read_file_or_empty` | `helpers/install/io.rs` | `read_file_or_empty(path) -> String` |  |
| `write_file` | `helpers/install/io.rs` | `write_file(path, content) -> ()` |  |
| `append_file` | `helpers/install/io.rs` | `append_file(path, content) -> ()` |  |
| `glob_list` | `helpers/install/io.rs` | `glob_list(pattern) -> Array` | returns string paths |
| `exists` | `helpers/install/filesystem.rs` | `exists(path) -> bool` |  |
| `file_exists` | `helpers/install/filesystem.rs` | `file_exists(path) -> bool` |  |
| `is_file` | `helpers/install/filesystem.rs` | `is_file(path) -> bool` | alias |
| `dir_exists` | `helpers/install/filesystem.rs` | `dir_exists(path) -> bool` |  |
| `is_dir` | `helpers/install/filesystem.rs` | `is_dir(path) -> bool` | alias |
| `mkdir` | `helpers/install/filesystem.rs` | `mkdir(path) -> ()` | side-effecting |
| `rm` | `helpers/install/filesystem.rs` | `rm(pattern) -> ()` | glob delete |
| `mv` | `helpers/install/filesystem.rs` | `mv(src, dst) -> ()` | rename |
| `ln` | `helpers/install/filesystem.rs` | `ln(target, link) -> ()` | symlink (unix) |
| `chmod` | `helpers/install/filesystem.rs` | `chmod(path, mode) -> ()` | unix mode |
| `download` | `helpers/acquire/download.rs` | `download(url, dest) -> String` | explicit destination |
| `verify_sha256` | `helpers/acquire/verify.rs` | `verify_sha256(path, expected) -> ()` | explicit file |
| `fetch_sha256` | `helpers/acquire/verify.rs` | `fetch_sha256(url, filename) -> String` | parse checksum file |
| `verify_sha512` | `helpers/acquire/verify.rs` | `verify_sha512(path, expected) -> ()` | explicit file |
| `verify_blake3` | `helpers/acquire/verify.rs` | `verify_blake3(path, expected) -> ()` | explicit file |
| `extract` | `helpers/build/extract.rs` | `extract(archive, dest) -> ()` | auto-detect format |
| `extract_with_format` | `helpers/build/extract.rs` | `extract_with_format(archive, dest, format) -> ()` | explicit format |
| `env` | `helpers/util/env.rs` | `env(name) -> String` | empty string if unset |
| `set_env` | `helpers/util/env.rs` | `set_env(name, value) -> ()` | process env |
| `http_get` | `helpers/acquire/http.rs` | `http_get(url) -> String` | timeout via `RECIPE_HTTP_TIMEOUT` |
| `github_latest_release` | `helpers/acquire/http.rs` | `github_latest_release(repo) -> String` | strips common prefixes |
| `github_latest_tag` | `helpers/acquire/http.rs` | `github_latest_tag(repo) -> String` | strips common prefixes |
| `parse_version` | `helpers/acquire/http.rs` | `parse_version(s) -> String` | strips v/release-/version- |
| `github_download_release` | `helpers/acquire/http.rs` | `github_download_release(repo, pattern, dest_dir) -> String` | downloads latest asset |
| `extract_from_tarball` | `helpers/acquire/http.rs` | `extract_from_tarball(url, pattern, dest) -> String` | downloads + extracts file |
| `check_disk_space` | `helpers/install/disk.rs` | `check_disk_space(path, required_bytes) -> ()` | `df -k` based |
| `exec` | `helpers/util/process.rs` | `exec(cmd, args:Array) -> int` | no shell, explicit args |
| `exec_output` | `helpers/util/process.rs` | `exec_output(cmd, args:Array) -> String` | no shell, explicit args |
| `git_clone` | `helpers/acquire/git.rs` | `git_clone(url, dest_dir) -> String` | clones into dest_dir/<repo> |
| `git_clone_depth` | `helpers/acquire/git.rs` | `git_clone_depth(url, dest_dir, depth) -> String` | shallow clone |
| `torrent` | `helpers/acquire/torrent.rs` | `torrent(url, dest_dir) -> String` | pure Rust (librqbit) |
| `download_with_resume` | `helpers/acquire/torrent.rs` | `download_with_resume(url, dest) -> String` | pure Rust (HTTP Range) |
| `llm_extract` | `helpers/llm.rs` | `llm_extract(content, prompt) -> String` | TODO backend |
| `llm_find_latest_version` | `helpers/llm.rs` | `llm_find_latest_version(url, project) -> String` | TODO backend |
| `llm_find_download_url` | `helpers/llm.rs` | `llm_find_download_url(content, criteria) -> String` | TODO backend |

---

## Missing / Mismatched vs `REQUIREMENTS.md` Helper Spec (Section 6)

### Missing Entirely (Not Exposed to Rhai)

- `copy(pattern)` (REQ-HELPER-011)
- `cd(directory)` (REQ-HELPER-040)
- `run(command)` + aliases `shell(command)`, `run_output(command)`, `run_status(command)` (REQ-HELPER-050..053)
- Installation helpers (REQ-HELPER-060..065):
  - `install_bin(pattern)`
  - `install_lib(pattern)`
  - `install_man(pattern)`
  - `install_to_dir(pattern, subdir[, mode])`
  - `rpm_install()`

### Existing Internal Building Blocks (Not Exposed)

Some “missing” helpers are largely implementable from existing internal utilities:

- `tools/recipe/src/helpers/internal/fs_utils.rs`:
  - globbing (`glob_paths*`)
  - copying/moving (`copy_file`, `move_file`)
  - basic path safety primitives (`is_safe_path`, `validate_safe_path`)
- `tools/recipe/src/helpers/internal/url_utils.rs`:
  - URL scheme validation and filename extraction/sanitization
- `tools/recipe/src/helpers/internal/cmd.rs`:
  - shell builder (though most recipes already use `shell_*`)

### Implemented But Signature/Semantics Diverge

The current implementation is “explicit path in, explicit path out” (good for
purity), while the spec still assumes “last downloaded file” context.

- `download(url)` (REQ-HELPER-010) vs implemented `download(url, dest)`
- `verify_sha256(expected)` (REQ-HELPER-020) vs implemented `verify_sha256(path, expected)`
- `extract(format)` (REQ-HELPER-030) vs implemented `extract(archive, dest)`
- `exec(command)` / `exec_output(command)` (REQ-HELPER-054..055) vs implemented `exec(cmd, args:Array)`

If we keep the explicit model, `tools/recipe/REQUIREMENTS.md` should be revised
to match it (or we add compatible wrapper helpers).

### Implemented But Not Specified In Section 6

These helpers exist today, but do not appear in the spec helper list:

- `read_file_or_empty(path) -> String` (convenience)
- `append_file(path, content) -> ()`
- `shell_in(dir, cmd) -> ()`
- `shell_status_in(dir, cmd) -> int`
- `shell_output_in(dir, cmd) -> String`
- `is_file(path) -> bool` (alias of `file_exists`)
- `is_dir(path) -> bool` (alias of `dir_exists`)
- `extract_with_format(archive, dest, format) -> ()`
- `fetch_sha256(url, filename) -> String`
- `torrent(url, dest_dir) -> String` (pure Rust, `librqbit`)
- `download_with_resume(url, dest) -> String` (pure Rust HTTP Range resume)
- `llm_extract/llm_find_latest_version/llm_find_download_url(...) -> String` (currently TODO backend)

---

## Missing For Safe A/B “Write Into Slot B” Composition

These gaps block using recipe as the compositor for an inactive slot sysroot:

- **Sysroot plumbing not implemented**:
  - `--sysroot` / `SYSROOT` exists in the spec but is not wired in code yet.
- **No sysroot confinement**:
  - Filesystem helpers accept arbitrary absolute paths and can mutate host `/`.
  - We need enforcement for `REQ-SEC-004` (no sysroot escape).
- **No sysroot-aware install helpers**:
  - We need install helpers that resolve destinations under `join_path(SYSROOT, PREFIX)` and track `installed_files` as sysroot-independent paths (REQ-STATE-034/035).
- **No installed_files integration**:
  - Today helpers don’t update `installed_files`, and there are no install helpers that can.
- **No atomic staging/commit**:
  - The spec requires staging + commit (REQ-ATOMIC-*), but there’s no helper or executor support for it yet.

---

## Footguns / Quality Notes

- `rm(pattern)` currently removes directories recursively if the glob matches a directory. The spec text implies “remove directories if empty”; this is more destructive than that.
- LLM helpers exist but are TODO; `refresh_recipe()` will need them (or an alternative deterministic patch generator) to be real.
- If you need machine-readable JSON from `recipe`, prefer `recipe --json-output <path>`: helpers like `shell()` inherit child stdout/stderr, so command output can interleave with JSON on stdout.

---

## Minimal Next Helper Set To Unblock A/B

1. Add a sysroot-aware installer surface:
   - `install_bin/install_lib/install_to_dir` (and/or a lower-level `install_file(src, dest_rel, mode)`).
2. Add compatibility aliases (or update spec):
   - `run`/`run_output`/`run_status` as aliases of `shell`/`shell_output`/`shell_status`.
3. Add `copy(pattern)` (common for “acquire from local artifacts”).
4. Add a safe “target path join” utility that guarantees no sysroot escape when joining:
   - `SYSROOT` + absolute-in-target path.

---

## Traceability

Primary sources:

- `tools/recipe/REQUIREMENTS.md` (helpers, sysroot, state tracking)
- `tools/recipe/OS_UPGRADES_BRAINDUMP.md` (A/B composition model)
- `docs/ab-default-plan.md` and `stages.md` (A/B default + Stage 07 trial boot)
