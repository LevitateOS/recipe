# Target filesystem metadata

A rootfs producer builds readable, writable staging files in Ginux. Staging
ownership and access grants are execution details: they must never become the
installed system's metadata. Target metadata travels as ordinary data in
`.recipe-target-metadata.json` at the root of each published tree. Publication,
recovery of kept results and explicit input import preserve the manifest without
performing privileged operations on the coordinator's filesystem.

Manifest versions 1 and 2 map canonical relative paths (the empty path means the
root directory) to inode kind, mode including setid/sticky bits, numeric UID/GID,
hex-encoded raw xattrs, link target and a SHA256 binding for regular payloads.
RPM account names are resolved against the **target** passwd/group files during
finalization. Host account lookup is never the authority for installed ownership.
Unsupported package inode types and xattr encodings fail explicitly.
The target manifest does not retain original timestamps. The target archive
writer normalizes modification times to epoch zero for reproducibility;
generic Recipe publication preserves its supported whole-second archive times.
Neither path promises nanosecond timestamp fidelity.

The native recipe helpers are:

- `target_extract_tar(archive, root)`: extract a tar payload using ordinary staging
  permissions; retain original target metadata, hardlinks and SCHILY xattrs.
- `target_extract_rpm(archive, root)`: read a verified RPM natively (lead, headers, and the zstd, xz, lzma, gzip or
  bzip2 cpio payload; no `rpm` or `rpm2archive` is started), recording payload metadata and RPM file capabilities. The
  helper does not run package scriptlets. Recipes verify their pinned inputs.
- `target_extract_apk(archive, root)`: extract an Alpine `.apk` (concatenated gzip
  streams: signature, control, data) natively. Only the data segment is extracted;
  the signature and control segments (`.SIGN.*`, `.PKGINFO`, install scripts) are
  skipped and no script runs. Members are recorded as `target_extract_tar` records
  them: modes, owner names, links, hardlinks, SCHILY xattrs.
- RPM and APK extraction record owners by name only (the ids are 0 until resolved)
  and read no account file, so recipes using them run on the host.
- `target_extract_rpm(archive, root, members)`: the same, and write the record of
  the extraction (`target::Members`: every member in archive order with its header
  metadata and where its bytes are in `root`, the file capabilities, and the mode
  extraction gives directories it creates) to `members`, outside `root`.
  `target::replay` applies such a record to another root exactly as extracting the
  archive there would (the OS builder composes system trees from per-package trees
  this way); `target::Extraction` is the one implementation both use.
- `target_pack_tree(root, archive)`: move the staging tree under `root` into a new
  tar `archive` (`target::write_tree`): every path with its kind, staging mode,
  modification time and link target, hardlinks kept, the manifest as a plain file.
  `root` is removed as it is written, so a domain never holds the tree twice.
- `target_unpack_tree(archive, root)`: make that tree again under an empty `root`
  (`target::unpack_tree`); each member's parent must be a directory an earlier
  member made, and each name appears once. It is made member by member (below).
- `target_capture_apks(root, archive_directories)`: read the installed APK database
  and the exact selected package archives, including their concatenated gzip/tar
  members. Every installed package must have an available archive. Collect these
  **before** applying recipe-specific filesystem policy.
- `target_mode(root, relative_path, mode)`: set intended target permissions while
  keeping staging readable/writable. Use this for deliberate mode overrides; the
  mode of a `FILES` entry and of `put` goes into the manifest the same way.
- `target_remove(root, relative_path)`: remove an output member and record an
  explicit intended removal in manifest version 2. The system consumer permits
  it only for paths previously owned by this producer; it never grants deletion
  of base files, administrator edits or another package's surviving selection.
- `target_finalize(root)`: resolve target accounts, discard absent package ghosts,
  record generated paths, preserve hardlink identity, and bind metadata to final
  payload bytes. Call after recipe policy changes, at the end of `build`, for a
  tree that is not a `system` output (an image tree); Recipe finishes `system`
  outputs itself, below. Generated paths default to UID/GID zero and their authored
  ordinary permissions; use `target_mode` for restrictive or special modes.
- `target_finalize(root, #{ owners: "apply", accounts: [...], seed: "var" })`
  (`seed: "var"` records `var_seed: true`: the tree's `var/` is
  state seeds, not owned files): how a tree
  that a system change writes into a root later (a `system` output) is finalized.
  Recipe does this itself for every `system` output after `configure` or `build`,
  with the recipe's `ACCOUNTS` as `accounts` (a recipe does not call it there). Owner names
  stay in the entries (`owners_at_apply: true` in the manifest) instead of being
  resolved against the tree's own account files, and `accounts` (optional,
  sysusers.d `u`/`g` lines, kept as the manifest's `accounts`) names accounts to
  create there. The system change creates them, with those of the tree's own
  `usr/lib/sysusers.d/*.conf`, the way the OS builder does, then resolves the names.

## Unpacking a tree

`target::unpack_tree` makes the tree member by member (`openat`, `write`,
`fchmod`, `futimens`, `close` per member) wherever it runs. The faster
one-stream import the retired downstream launcher offered (`[storage]
archive_import`, `RECIPE_ARCHIVE_IMPORT`, the `ioctl` path in `src/target.rs`)
is gone with that launcher: the public Ginux lifecycle has no such stream, and
the cost is speed only, never a different tree. A tree of 200,000 files costs
about five calls per member inside the instance.

The result: the same names, kinds, modes, modification times (whole seconds),
link targets and manifest bytes. Hardlinks inside a tree that crosses the
instance boundary (an input staged into it, or an output taken out) arrive as
regular files: the public lifecycle carries only directories, regular files and
symlinks. A hardlink made inside a run by `target_unpack_tree` or by the recipe
itself stays one inode for as long as the tree stays in that run.

`target_extract_rpm` also works member by member. Its per-file extraction leaves
creation-time modification times, which `target_pack_tree` then records, and it
resolves member paths through directory links an earlier member made.

The compiler image must contain the selected package/extraction tools. Installing
one only on the coordinator does not make it available in Ginux. The acquisition
image includes RPM tools from its base and pinned xorriso/libisoburn/libisofs/
libburn additions for ISO extraction. `recipe-image` can extend a pinned existing
image with RPMs without selecting another Rust distribution: omit `rust` and use
an empty `rust_components` array.

The shared Rust API in `levitate_recipe::target` owns manifest validation,
`merge_tree` and `write_tar`. Tree composition copies metadata together with its
payload and replaces metadata when a package replaces an inode. The archive
writer refuses changed content with stale metadata, preventing old capabilities
or setid bits from attaching to a replacement binary. A consumer deliberately
replacing a file (such as contract-owned `etc/os-release`) must also replace or
remove that path's metadata entry. Avoid copying manifests as unmerged ordinary
files into an already composed tree.

EROFS assembly imports the resulting tar directly using `mkfs.erofs --tar=f`.
It does not use `--all-root`, host ownership, or host permission normalization.
The embedded manifest also survives EROFS extraction when deriving a later
product. Extraction into coordinator staging uses writable grants again; the
manifest remains authoritative when repacking. On the deployed system, recstrap
continues to mount the completed EROFS image and copy it with GNU `cp -aT --preserve=xattr`; it does
not reconstruct distro package policy or interpret the manifest.

The integration case
`target_metadata::cli_target_metadata_survives_publication_reimport_and_erofs_production`
uses real Ginux execution, publication, reimport,
composition and EROFS packing. It checks non-root ownership,
setuid/sticky modes, absolute symlinks, hardlinks, raw file capabilities and
rejection of a changed privileged payload. Run it with the documented Ginux
privileges (root or the sudoers rule) and a compatible Ginux and image.

Actual recstrap mount/copy deployment is a separate libvirt fixture consuming
the exact produced EROFS image; see the superproject's
`docs/ginux-contributions/prd12-deployment/README.md` for build/run commands and
prebuilt kernel selection. The CLI case alone does not establish deployment.
Both image identity and deployment receipts must match the claimed source.
erofs-utils 1.8.10
`fsck.erofs --preserve --xattrs` applies capabilities before chown, which clears
them again. This does not affect recstrap’s read-only mount/copy path; deriving
products also uses the embedded manifest rather than extracted inode ownership.

APK 3 requires a non-root virtual identity for `--usermode`. Per-command
`uid`/`gid` options select that identity inside Ginux while package metadata
continues to describe installed UID/GID values. No host credential changes are
used by the configured runtime.

For system changes, declare `NEEDS.outputs = #{ system: "directory" }`; the
output is a root-relative tree, which Recipe finalizes. Numeric installed
UID/GID, mode, raw xattrs, links and content fingerprints come from this manifest,
not the Ginux worker's staging inodes. Omitted old owned files are also removed
when updating the selection. Ordinary `package` outputs are placed in the root's
system profile instead. The coordinator attaches versioned recipe/dependency
provenance after verified publication; worker-supplied provenance is not trusted.

When a system producer omits `security.selinux`, Recipe retains labels assigned
by the installed kernel policy. An explicitly authored label remains an exact
metadata constraint, as do capabilities and all other intended xattrs.
