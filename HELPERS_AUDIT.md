# Recipe helper reference

The executable contract is `const RECIPE_API = 2;`. This catalog follows
`src/helpers/mod.rs::register_all`. Helpers with effects run inside Ginux, under
its configured filesystem, task, memory and network assignments. They never
select a native host execution path.

Registration is an interface inventory, not successful execution of every
package/tool variant. Use the actual compiler/acquisition image and preserve
the consumer's locks and metadata checks. [Runtime limits](docs/runtime-limits.md)
defines the supported clock, poll, resource, network and timestamp boundaries.

Checks use `ready(ctx)`/`needs_work(ctx, reason)`; native helper errors fail the
check unless the recipe explicitly handles them. Phase hooks return context.
`RUNTIME` during setup and `ctx.runtime` during hooks provide invocation paths;
`ctx.state` is the only restored recipe namespace.

## Registered helpers

| Helper | Owner | Signature | Behavior |
| --- | --- | --- | --- |
| `apk_add_repo` | ``helpers/util/process.rs`` | `apk_add_repo(url) -> ()` | Runs the matching package command inside the domain; requires its binary, assigned stores and explicit network grants where used. |
| `apk_download` | ``helpers/util/process.rs`` | `apk_download(packages:Array, dest_dir) -> Array` | Runs the matching package command inside the domain; requires its binary, assigned stores and explicit network grants where used. |
| `apk_install` | ``helpers/util/process.rs`` | `apk_install(packages:Array) -> ()` | Runs the matching package command inside the domain; requires its binary, assigned stores and explicit network grants where used. |
| `apk_installed` | ``helpers/util/process.rs`` | `apk_installed(name) -> bool` | Runs the matching package command inside the domain; requires its binary, assigned stores and explicit network grants where used. |
| `apk_package_available` | ``helpers/util/process.rs`` | `apk_package_available(name) -> bool` | Runs the matching package command inside the domain; requires its binary, assigned stores and explicit network grants where used. |
| `apk_version` | ``helpers/util/process.rs`` | `apk_version(name) -> String` | Runs the matching package command inside the domain; requires its binary, assigned stores and explicit network grants where used. |
| `append_file` | ``helpers/install/io.rs`` | `append_file(path, content) -> ()` |  |
| `append_line_if_missing` | ``helpers/install/io.rs`` | `append_line_if_missing(path, line) -> bool` | appends exact line once |
| `basename` | ``helpers/util/paths.rs`` | `basename(path) -> String` |  |
| `check_disk_space` | ``helpers/install/disk.rs`` | `check_disk_space(path, required_bytes) -> ()` | `df -k` based |
| `chmod` | ``helpers/install/filesystem.rs`` | `chmod(path, mode) -> ()` | unix mode |
| `command_exists` | `helpers/util/process.rs` | `command_exists(name) -> bool` | Probe current domain PATH. |
| `carry_libraries` | `helpers/install/carry.rs` | `carry_libraries(package, prefix, search:Array) -> Array` | Trial. Copies into `package/lib/` the loader and every shared library that the programs in `package/bin/`, and the programs and plug-ins the recipe put under `package/lib/`, load, from `search` then the build image, and writes into each file its loader and library folder under `prefix`; refuses a program not linked with `carry_link_flags()`. |
| `carry_link_flags` | `helpers/install/carry.rs` | `carry_link_flags() -> String` | Trial. Linker flags for every program and library of a package: a long stand-in library folder that reserves the room `carry_libraries` writes into. |
| `contains` | ``helpers/util/string.rs`` | `contains(s, pat) -> bool` |  |
| `copy_file` | ``helpers/install/filesystem.rs`` | `copy_file(src, dst) -> ()` | exact destination path |
| `copy_file_reflink` | `helpers/install/filesystem.rs` | `copy_file_reflink(src, dst) -> ()` | Attempts reflink; falls back to a checked copy. |
| `copy_first_existing` | ``helpers/install/filesystem.rs`` | `copy_first_existing(sources:Array, dst) -> String` | copies first existing source |
| `copy_into_dir` | ``helpers/install/filesystem.rs`` | `copy_into_dir(pattern, dest_dir) -> ()` | copies matching files into directory |
| `copy_tree_contents` | `helpers/install/filesystem.rs` | `copy_tree_contents(src, dest) -> ()` | Copies contents, preserving hardlink identity, symlink bytes and modes. |
| `debug` | `Rhai built-in; callback in `helpers/mod.rs`` | `debug(value) -> ()` | stderr, with source position |
| `dirname` | ``helpers/util/paths.rs`` | `dirname(path) -> String` |  |
| `dnf_add_repo` | ``helpers/util/process.rs`` | `dnf_add_repo(url) -> ()` | Runs the matching package command inside the domain; requires its binary, assigned stores and explicit network grants where used. |
| `dnf_download` | ``helpers/util/process.rs`` | `dnf_download(packages:Array, dest_dir, arches:Array[, resolve]) -> Array` | Runs the matching package command inside the domain; requires its binary, assigned stores and explicit network grants where used. |
| `dnf_install` | ``helpers/util/process.rs`` | `dnf_install(packages:Array) -> ()` | Runs the matching package command inside the domain; requires its binary, assigned stores and explicit network grants where used. |
| `dnf_install_allow_erasing` | ``helpers/util/process.rs`` | `dnf_install_allow_erasing(packages:Array) -> ()` | Runs the matching package command inside the domain; requires its binary, assigned stores and explicit network grants where used. |
| `dnf_package_available` | ``helpers/util/process.rs`` | `dnf_package_available(name) -> bool` | Runs the matching package command inside the domain; requires its binary, assigned stores and explicit network grants where used. |
| `download` | `helpers/acquire/download.rs` | `download(url, dest, sha256[, resume:bool]) -> String` | Required SHA256; staged verified replacement; failure preserves valid destination. |
| `ends_with` | ``helpers/util/string.rs`` | `ends_with(s, suffix) -> bool` |  |
| `env` | `helpers/util/env.rs` | `env(name) -> String` | Read the explicit worker environment; empty when absent. |
| `exec` | ``helpers/util/process.rs`` | `exec(cmd, args:Array [, options:Map]) -> int` | explicit exit status; child-only cwd/env/uid/gid |
| `exec_checked` | ``helpers/util/process.rs`` | `exec_checked(cmd, args:Array [, options:Map]) -> ()` | throws on nonzero; child-only cwd/env/uid/gid |
| `exec_output` | ``helpers/util/process.rs`` | `exec_output(cmd, args:Array [, options:Map]) -> String` | checked stdout capture; child-only cwd/env/uid/gid |
| `exists` | ``helpers/install/filesystem.rs`` | `exists(path) -> bool` |  |
| `extract` | `helpers/build/extract.rs` | `extract(archive, dest) -> ()` | Detect archive format; validate paths and reject invalid extraction. |
| `extract_with_format` | `helpers/build/extract.rs` | `extract_with_format(archive, dest, format) -> ()` | Explicit format; same checked extraction boundary. |
| `fetch_sha256` | ``helpers/acquire/verify.rs`` | `fetch_sha256(url, filename) -> String` | parse checksum file |
| `git_checkout` | `helpers/acquire/git.rs` | `git_checkout(url, exact_dest, full_commit) -> String` | Exact destination and full 40/64-hex commit; validates origin and checkout. |
| `github_download_release` | `helpers/acquire/http/api.rs` | `github_download_release(repo, pattern, dest_dir, sha256) -> String` | Selects matching release asset into dest_dir/asset_name; required digest and staged verified download. |
| `github_latest_release` | ``helpers/acquire/http.rs`` | `github_latest_release(repo) -> String` | strips a leading `v` |
| `github_latest_tag` | ``helpers/acquire/http.rs`` | `github_latest_tag(repo) -> String` | strips a leading `v` |
| `glob_exists` | ``helpers/install/filesystem.rs`` | `glob_exists(pattern) -> bool` | true if any match |
| `glob_list` | ``helpers/install/io.rs`` | `glob_list(pattern) -> Array` | returns string paths |
| `http_get` | `helpers/acquire/http/api.rs` | `http_get(url) -> String` | Bounded metadata text; requires invocation network permission. |
| `is_dir` | `helpers/install/filesystem.rs` | `is_dir(path) -> bool` | Directory check in the domain view. |
| `is_file` | `helpers/install/filesystem.rs` | `is_file(path) -> bool` | Regular file check in the domain view. |
| `join_path` | ``helpers/util/paths.rs`` | `join_path(a, b) -> String` | Pure join |
| `ln` | ``helpers/install/filesystem.rs`` | `ln(target, link) -> ()` | symlink (unix) |
| `ln_force` | ``helpers/install/filesystem.rs`` | `ln_force(target, link) -> ()` | replace existing link/path |
| `log` | ``helpers/util/log.rs`` | `log(msg) -> ()` | diagnostic stream via `src/diagnostics.rs` |
| `mkdir` | ``helpers/install/filesystem.rs`` | `mkdir(path) -> ()` | side-effecting |
| `mv` | ``helpers/install/filesystem.rs`` | `mv(src, dst) -> ()` | rename |
| `needs_work` | `core readiness` | `needs_work(ctx, reason) -> Map` | Check requests work with a nonempty reason. |
| `parse_version` | ``helpers/acquire/http.rs`` | `parse_version(s) -> String` | strips v/release-/version- |
| `print` | `Rhai built-in; callback in `helpers/mod.rs`` | `print(value) -> ()` | stderr; keeps JSON/protocol stdout intact |
| `progress` | ``helpers/util/log.rs`` | `progress(done, total[, what]) -> ()` | reports to the coordinator's `--progress-file`; `total` 0 is unknown |
| `read_file` | ``helpers/install/io.rs`` | `read_file(path) -> String` |  |
| `read_file_or_empty` | ``helpers/install/io.rs`` | `read_file_or_empty(path) -> String` |  |
| `ready` | `core readiness` | `ready(ctx) -> Map` | Check succeeds with updated context. |
| `replace` | ``helpers/util/string.rs`` | `replace(s, from, to) -> String` |  |
| `replace_in_file` | ``helpers/install/io.rs`` | `replace_in_file(path, from, to) -> ()` | whole-file text replacement |
| `rm` | ``helpers/install/filesystem.rs`` | `rm(pattern) -> ()` | glob delete |
| `rpm_installed` | ``helpers/util/process.rs`` | `rpm_installed(name) -> bool` | Runs the matching package command inside the domain; requires its binary, assigned stores and explicit network grants where used. |
| `rpm_version` | ``helpers/util/process.rs`` | `rpm_version(name) -> String` | Runs the matching package command inside the domain; requires its binary, assigned stores and explicit network grants where used. |
| `shell` | ``helpers/util/shell.rs`` | `shell(cmd) -> ()` | uses `sh -c` |
| `shell_in` | ``helpers/util/shell.rs`` | `shell_in(dir, cmd) -> ()` | run in directory |
| `shell_output` | ``helpers/util/shell.rs`` | `shell_output(cmd) -> String` | captures stdout |
| `shell_output_in` | ``helpers/util/shell.rs`` | `shell_output_in(dir, cmd) -> String` |  |
| `shell_status` | ``helpers/util/shell.rs`` | `shell_status(cmd) -> int` | returns exit code |
| `shell_status_in` | ``helpers/util/shell.rs`` | `shell_status_in(dir, cmd) -> int` |  |
| `split` | ``helpers/util/string.rs`` | `split(s, sep) -> Array` |  |
| `starts_with` | ``helpers/util/string.rs`` | `starts_with(s, prefix) -> bool` |  |
| `target_capture_apks` | `helpers/target.rs` | `target_capture_apks(root, archive_directories:Array) -> ()` | Read installed APK metadata and exact selected archives before recipe policy; does not grant host ownership changes. |
| `target_extract_rpm` | `helpers/target.rs` | `target_extract_rpm(archive, root) -> ()` | Retain verified RPM payload metadata/capabilities without package scriptlets; requires image rpm/rpm2archive. |
| `target_extract_rpm` | `helpers/target.rs` | `target_extract_rpm(archive, root, members) -> ()` | The same, and write the record of the extraction (`target::Members`: members in archive order, capabilities, created directory mode) to `members`, outside `root`, so `target::replay` can compose the package into another root later. |
| `target_pack_tree` | `helpers/target.rs` | `target_pack_tree(root, archive) -> ()` | Move the staging tree under `root` into a new tar `archive` (`target::write_tree`): paths, kinds, staging modes, times, link targets, hardlinks, the target metadata manifest; `root` is removed as it is written. |
| `target_unpack_tree` | `helpers/target.rs` | `target_unpack_tree(archive, root) -> ()` | Make the tree a `target_pack_tree`/`target::write_tree` archive holds under the empty `root` (created if absent), exactly; reports bytes read through `progress`. |
| `target_extract_tar` | `helpers/target.rs` | `target_extract_tar(archive, root) -> ()` | Extract staging payload while retaining target ownership, modes, links and SCHILY xattrs. |
| `target_finalize` | `helpers/target.rs` | `target_finalize(root) -> ()` | Resolve target accounts and bind final payload bytes to the target metadata manifest; run after recipe policy. |
| `target_mode` | `helpers/target.rs` | `target_mode(root, relative_path, mode:int) -> ()` | Set intended target permissions independently of writable staging grants. |
| `trim` | ``helpers/util/string.rs`` | `trim(s) -> String` |  |
| `verify_blake3` | ``helpers/acquire/verify.rs`` | `verify_blake3(path, expected) -> ()` | explicit file |
| `verify_sha256` | ``helpers/acquire/verify.rs`` | `verify_sha256(path, expected) -> ()` | explicit file |
| `verify_sha512` | ``helpers/acquire/verify.rs`` | `verify_sha512(path, expected) -> ()` | explicit file |
| `warn` | ``helpers/util/log.rs`` | `warn(msg) -> ()` |  |
| `write_file` | ``helpers/install/io.rs`` | `write_file(path, content) -> ()` |  |

## Acquisition and publication

`download` requires a full SHA256. Its optional fourth boolean requests checked
resume. Errors in status, length, range identity, hash or destination publication
cannot replace a previously valid destination. `git_checkout` requires a full
commit, exact destination and direct repository URL; Git redirects are disabled.
Cached includes, URL rewrites, transport overrides, filters and duplicate origins
are rejected intact. Hooks, fsmonitor, replacement objects and submodule
recursion are disabled. URL/version/digest facts remain recipe-owned.

Network helpers and external clients need recipe-declared NEEDS endpoints,
subject to caller-only narrowing. Metadata helpers are bounded conveniences; downloading a payload
and then trusting a digest calculated from those same unauthenticated bytes is
not source verification. HTTP download redirect targets also need grants.

`extract` is a checked archive operation, not a success marker. External RPM
pipelines must check the converter and extractor independently. Runtime support
for an actual binary/archive must be demonstrated with the selected image.

## Commands

`exec` returns a status. `exec_checked` and `exec_output` throw on nonzero exit;
the latter returns captured stdout. Each accepts an argument array and optional
`#{cwd: "/work/build", env: #{KEY: "value"}}` with child-only scope. Shell
variants use a shell string and need quoting for variable data. All command,
recipe and helper diagnostics go to stderr; stdout remains final JSON/protocol.

## Paths and products

Build work is `ctx.runtime.paths.build`; shared dependency tools are
`ctx.runtime.paths.tools`; imported inputs are below `ctx.runtime.paths.inputs`.
Owned target/output is `ctx.runtime.paths.target`/`.output`, both `/out`.
Select actual files/directories in `ctx.outputs` for validated publication.
Published generations and imported inputs are immutable. See
[WRITING_RECIPES.md](WRITING_RECIPES.md) for the full lifecycle and examples.
