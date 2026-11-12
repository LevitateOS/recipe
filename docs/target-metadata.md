# Target filesystem metadata

A rootfs producer builds readable, writable staging files in Ginux. Staging
ownership and access grants are execution details: they must never become the
installed system's metadata. Target metadata travels as ordinary data in
`.recipe-target-metadata.json` at the root of each published tree. Publication,
checkpoint recovery and explicit input import preserve the manifest without
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
- `target_extract_rpm(archive, root)`: use the build image's `rpm2archive` and `rpm`
  on a verified RPM, recording payload metadata and RPM file capabilities. The
  helper does not run package scriptlets. Recipes verify their pinned inputs.
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
  member made, and each name appears once. Where the domain's owned storage
  offers Ginux archive import (below), the tree goes to the supervisor as one
  tar stream; elsewhere it is made member by member. Both leave the same tree.
- `target_capture_apks(root, archive_directories)`: read the installed APK database
  and the exact selected package archives, including their concatenated gzip/tar
  members. Every installed package must have an available archive. Collect these
  **before** applying recipe-specific filesystem policy.
- `target_mode(root, relative_path, mode)`: set intended target permissions while
  keeping staging readable/writable. Use this for deliberate mode overrides.
- `target_remove(root, relative_path)`: remove an output member and record an
  explicit intended removal in manifest version 2. The system consumer permits
  it only for paths previously owned by this producer; it never grants deletion
  of base files, administrator edits or another package's surviving selection.
- `target_finalize(root)`: resolve target accounts, discard absent package ghosts,
  record generated paths, preserve hardlink identity, and bind metadata to final
  payload bytes. Call after recipe policy changes and before writing the recipe's
  success marker. Generated paths default to UID/GID zero and their authored
  ordinary permissions; use `target_mode` for restrictive or special modes.

## Unpacking a tree through archive import

A tree of 200,000 files costs about five mediated calls per member when it is
made member by member (`openat`, `write`, `fchmod`, `futimens`, `close`).
Ginux can instead take one tar stream for an owned directory and apply each
member itself (Ginux `docs/archive-import.md`): one mediated `write` per MiB.
Recipe enables it in each invocation's config (`[storage] archive_import =
true`) when the launcher accepts the field; `RECIPE_ARCHIVE_IMPORT=0` turns it
off. A Ginux that predates the field rejects the whole config, so the
coordinator first asks the launcher, once per launcher digest for its
lifetime: `instance-validate` on the declaration-only config with the field
added, which parses the config as a launch would and starts nothing. Any
failure of that probe means "not supported".

`target::unpack_tree` asks for a stream on `root` (`ioctl` `0x40104701`, flag
2, clear set-user-ID, set-group-ID and sticky bits). `ENOTTY` or `EINVAL`
(not offered: field off, older Ginux, storage Ginux does not own, a plain
host) selects the member-by-member path. Otherwise it writes the archive's
members again as canonical tar, with ordinary 1 MiB writes and a final
`fsync`:

- in the archive's order, except that each directory member follows its
  contents, so creating them does not move its time after it is set;
- directories with their permission bits and the owner's `rwx`, files with
  their permission bits; ownership is never sent;
- hardlinks after their targets and symbolic links as written, the manifest
  as the plain file it is. `target::write_tree` writes no extended
  attributes, so the stream never carries any.

After the stream, ordinary calls apply what it could not: symbolic links
whose target is longer than its 255-byte long-link record are made one by
one; the time of any directory that gained a name after its member is set
again (only an archive out of `write_tree`'s order, or such a link, causes
that); and every recorded mode the stream did not give (set-user-ID,
set-group-ID and sticky bits, directories without the owner's `rwx`) is
applied, children before their directory. Owned storage refuses special bits
to an individual `chmod` as it does on the member-by-member path, so a tree
recording them fails either way; staging trees made in a domain never carry
them, as their real modes live in the manifest.

The result matches the member-by-member path: the same names, kinds, modes,
modification times (whole seconds), link targets, hardlinks and manifest
bytes. Only access and change times, which no tree records, can differ. A
failure in the stream is a hard error naming the stream's errno and the first
member of the failed write that is not in place; it never falls back after a
partial import, and members already applied remain, as on the other path.

`target_extract_rpm` stays member by member. Its per-file extraction leaves
creation-time modification times, which `target_pack_tree` then records,
whereas an import always sets one; it resolves member paths through
directory links an earlier member made (an import refuses those with
`ELOOP`); and it makes hardlinks only at the end, to what their targets
resolve to then. Per-package roots are small; the trees worth streaming are
the ones `target_unpack_tree` makes.

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
uses real Ginux execution, checkpoint recovery, publication, reimport,
composition and EROFS packing. It checks non-root ownership,
setuid/sticky modes, absolute symlinks, hardlinks, raw file capabilities and
rejection of a changed privileged payload. Run it with the documented Ginux
bootstrap privileges and a compatible launcher/image.

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

For recab system changes, declare `NEEDS.outputs.system = "directory"` and
publish a finalized root-relative tree as `ctx.outputs.system`. Numeric installed
UID/GID, mode, raw xattrs, links and content fingerprints come from this manifest,
not the Ginux worker's staging inodes. Omitted old owned files are also removed
when updating the selection. Ordinary `package` outputs are placed in the slot's
system profile instead. The coordinator attaches versioned recipe/dependency
provenance after verified publication; worker-supplied provenance is not trusted.

When a system producer omits `security.selinux`, recab retains labels assigned
by the installed kernel policy. An explicitly authored label remains an exact
metadata constraint, as do capabilities and all other intended xattrs.
