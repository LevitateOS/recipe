# Target filesystem metadata

A rootfs producer builds readable, writable staging files in Ginux. Staging
ownership and access grants are execution details: they must never become the
installed system's metadata. Target metadata travels as ordinary data in
`.recipe-target-metadata.json` at the root of each published tree. Publication,
checkpoint recovery and explicit input import preserve the manifest without
performing privileged operations on the coordinator's filesystem.

A version 1 manifest maps canonical relative paths (the empty path means the
root directory) to inode kind, mode including setid/sticky bits, numeric UID/GID,
hex-encoded raw xattrs, link target and a SHA256 binding for regular payloads.
RPM account names are resolved against the **target** passwd/group files during
finalization. Host account lookup is never the authority for installed ownership.
Unsupported package inode types and xattr encodings fail explicitly.

The native recipe helpers are:

- `target_extract_tar(archive, root)`: extract a tar payload using ordinary staging
  permissions; retain original target metadata, hardlinks and SCHILY xattrs.
- `target_extract_rpm(archive, root)`: use the build image's `rpm2archive` and `rpm`
  on a verified RPM, recording payload metadata and RPM file capabilities. The
  helper does not run package scriptlets. Recipes verify their pinned inputs.
- `target_capture_apks(root, archive_directories)`: read the installed APK database
  and the exact selected package archives, including their concatenated gzip/tar
  members. Every installed package must have an available archive. Collect these
  **before** applying recipe-specific filesystem policy.
- `target_mode(root, relative_path, mode)`: set intended target permissions while
  keeping staging readable/writable. Use this for deliberate mode overrides.
- `target_finalize(root)`: resolve target accounts, discard absent package ghosts,
  record generated paths, preserve hardlink identity, and bind metadata to final
  payload bytes. Call after recipe policy changes and before writing the recipe's
  success marker. Generated paths default to UID/GID zero and their authored
  ordinary permissions; use `target_mode` for restrictive or special modes.

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
`target_metadata::cli_target_metadata_survives_publication_reimport_and_erofs`
uses real Ginux execution, checkpoint recovery, publication, reimport,
composition, EROFS packing, and recstrap’s mount/copy deployment path. It checks non-root ownership,
setuid/sticky modes, absolute symlinks, hardlinks, raw file capabilities and
rejection of a changed privileged payload. Run it with the documented Ginux
bootstrap privileges and a compatible launcher/image.

The deployed-copy check uses a private mount namespace: erofs-utils 1.8.10
`fsck.erofs --preserve --xattrs` applies capabilities before chown, which clears
them again. This does not affect recstrap’s read-only mount/copy path; deriving
products also uses the embedded manifest rather than extracted inode ownership.

APK 3 requires a non-root virtual identity for `--usermode`. Per-command
`uid`/`gid` options select that identity inside Ginux while package metadata
continues to describe installed UID/GID values. No host credential changes are
used by the configured runtime.
