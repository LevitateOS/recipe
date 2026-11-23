//! Real bootstrap CLI composition and failure behavior over local archive inputs.
//!
//! The producer writes the compiler image as an OCI image layout (the form the
//! public Ginux lifecycle imports with `ginux import-image`): `oci-layout`,
//! `index.json`, `blobs/sha256/<layer>`, `<config>`, `<manifest>`, an
//! uncompressed `image.oci.tar` of that layout and `provenance.json`. The
//! image's identity is the manifest digest. No Ginux and no privileges are
//! needed: this suite only runs the producer.
use flate2::{Compression, write::GzEncoder};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::Command;

fn tar_entry(writer: &mut tar::Builder<impl Write>, name: &str, data: &[u8]) {
    let mut header = tar::Header::new_gnu();
    header.set_size(data.len() as u64);
    header.set_mode(0o755);
    header.set_cksum();
    writer.append_data(&mut header, name, data).unwrap();
}
fn digest(path: &Path) -> String {
    hex::encode(Sha256::digest(fs::read(path).unwrap()))
}
fn manifest(root: &Path) {
    fs::write(
        root.join("manifest.json"),
        serde_json::to_vec(&serde_json::json!({
            "version":1,
            "base":{"path":"base.tar","sha256":digest(&root.join("base.tar"))},
            "rust":{"path":"rust.tar.gz","sha256":digest(&root.join("rust.tar.gz"))},
            "rust_components":["rustc"], "rpms":[]
        }))
        .unwrap(),
    )
    .unwrap();
}
fn produce(root: &Path, output: &str) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_recipe-image"))
        .arg("--manifest")
        .arg(root.join("manifest.json"))
        .arg("--input-root")
        .arg(root)
        .arg("--output")
        .arg(root.join(output))
        .output()
        .unwrap()
}
/// The layout's exact bytes (design: byte-stable, keys in this order, no
/// whitespace, no trailing newline) and its digests.
fn check_layout(layout: &Path, manifest: &str, layer: &str) {
    let blobs = layout.join("blobs/sha256");
    assert_eq!(
        fs::read_to_string(layout.join("oci-layout")).unwrap(),
        r#"{"imageLayoutVersion":"1.0.0"}"#
    );
    let layer_bytes = fs::read(blobs.join(layer)).unwrap();
    assert_eq!(hex::encode(Sha256::digest(&layer_bytes)), layer);
    let config = format!(
        r#"{{"architecture":"amd64","os":"linux","rootfs":{{"type":"layers","diff_ids":["sha256:{layer}"]}},"config":{{}}}}"#
    );
    let config_digest = hex::encode(Sha256::digest(config.as_bytes()));
    assert_eq!(
        fs::read_to_string(blobs.join(&config_digest)).unwrap(),
        config
    );
    let manifest_text = format!(
        r#"{{"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json","config":{{"mediaType":"application/vnd.oci.image.config.v1+json","digest":"sha256:{config_digest}","size":{}}},"layers":[{{"mediaType":"application/vnd.oci.image.layer.v1.tar","digest":"sha256:{layer}","size":{}}}]}}"#,
        config.len(),
        layer_bytes.len()
    );
    assert_eq!(
        hex::encode(Sha256::digest(manifest_text.as_bytes())),
        manifest
    );
    assert_eq!(
        fs::read_to_string(blobs.join(manifest)).unwrap(),
        manifest_text
    );
    assert_eq!(
        fs::read_to_string(layout.join("index.json")).unwrap(),
        format!(
            r#"{{"schemaVersion":2,"manifests":[{{"mediaType":"application/vnd.oci.image.manifest.v1+json","digest":"sha256:{manifest}","size":{},"platform":{{"architecture":"amd64","os":"linux"}}}}]}}"#,
            manifest_text.len()
        )
    );
    // Nothing else is in the blob store.
    let mut stored: Vec<_> = fs::read_dir(&blobs)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    stored.sort();
    let mut expected = vec![layer.to_owned(), config_digest, manifest.to_owned()];
    expected.sort();
    assert_eq!(stored, expected);
    // `image.oci.tar` is the same layout as one uncompressed tar, in the
    // member order the CLI's importer accepts.
    let mut archive = tar::Archive::new(fs::File::open(layout.join("image.oci.tar")).unwrap());
    let mut members = Vec::new();
    for entry in archive.entries().unwrap() {
        let mut entry = entry.unwrap();
        assert_eq!(entry.header().mtime().unwrap(), 0);
        let name = entry.path().unwrap().to_string_lossy().into_owned();
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut entry, &mut bytes).unwrap();
        if let Some(blob) = name.strip_prefix("blobs/sha256/").filter(|b| !b.is_empty()) {
            assert_eq!(bytes, fs::read(blobs.join(blob)).unwrap(), "{name}");
        }
        members.push(name);
    }
    assert_eq!(
        members[..4],
        ["oci-layout", "index.json", "blobs/", "blobs/sha256/"]
    );
    let mut rest: Vec<_> = members[4..].to_vec();
    rest.sort();
    let mut wanted: Vec<_> = expected
        .iter()
        .map(|d| format!("blobs/sha256/{d}"))
        .collect();
    wanted.sort();
    assert_eq!(rest, wanted);
}

#[test]
fn image_cli_composes_reproducibly_and_rejects_invalid_inputs_without_publication() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let mut base = tar::Builder::new(fs::File::create(root.join("base.tar")).unwrap());
    tar_entry(&mut base, "usr/keep", b"base bytes");
    base.finish().unwrap();
    drop(base);
    let mut rust = tar::Builder::new(GzEncoder::new(
        fs::File::create(root.join("rust.tar.gz")).unwrap(),
        Compression::fast(),
    ));
    tar_entry(&mut rust, "toolchain/rustc/bin/compiler", b"compiler bytes");
    tar_entry(
        &mut rust,
        "toolchain/rustc/share/doc/excluded",
        b"not selected",
    );
    let mut header = tar::Header::new_gnu();
    header.set_mode(0o755);
    header.set_size(0);
    header.set_entry_type(tar::EntryType::Link);
    rust.append_link(
        &mut header,
        "toolchain/rustc/bin/compiler-alias",
        "toolchain/rustc/bin/compiler",
    )
    .unwrap();
    rust.into_inner().unwrap().finish().unwrap();
    manifest(root);
    let mut manifests = Vec::new();
    for output in ["first", "second"] {
        let result = produce(root, output);
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let provenance: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
        let layout = root.join(output);
        let manifest = provenance["manifest_digest"].as_str().unwrap().to_owned();
        let layer = provenance["layer_sha256"].as_str().unwrap().to_owned();
        // The tar digest is no longer the identity: its field is replaced.
        assert!(provenance.get("sha256").is_none(), "{provenance}");
        assert_eq!(
            fs::read(layout.join("provenance.json")).unwrap(),
            result.stdout
        );
        check_layout(&layout, &manifest, &layer);
        manifests.push((manifest, layer));
    }
    // Two productions of the same inputs are the same image, byte for byte.
    assert_eq!(manifests[0], manifests[1]);
    for name in ["oci-layout", "index.json", "image.oci.tar"] {
        assert_eq!(
            fs::read(root.join("first").join(name)).unwrap(),
            fs::read(root.join("second").join(name)).unwrap(),
            "{name} differs between productions"
        );
    }
    let layer = &manifests[0].1;
    let mut archive =
        tar::Archive::new(fs::File::open(root.join("first/blobs/sha256").join(layer)).unwrap());
    let mut names = Vec::new();
    for entry in archive.entries().unwrap() {
        let entry = entry.unwrap();
        let name = entry.path().unwrap().to_string_lossy().into_owned();
        assert_eq!(entry.header().mtime().unwrap(), 0);
        if name == "usr/bin/compiler-alias" {
            assert!(entry.header().entry_type().is_hard_link());
            assert_eq!(
                entry.link_name().unwrap().unwrap(),
                Path::new("usr/bin/compiler")
            );
        }
        names.push(name);
    }
    assert_eq!(
        names,
        ["usr/keep", "usr/bin/compiler", "usr/bin/compiler-alias"]
    );
    // The layer keeps what the injector leaves to Recipe: no hosts file and no
    // whiteout names (the image expander would treat them as deletions).
    assert!(names.iter().all(|name| name != "etc/hosts"));
    assert!(
        names
            .iter()
            .all(|name| !name.split('/').any(|part| part.starts_with(".wh.")))
    );
    fs::write(root.join("first/sentinel"), "unchanged").unwrap();
    let collision = produce(root, "first");
    assert!(!collision.status.success());
    assert_eq!(
        fs::read_to_string(root.join("first/sentinel")).unwrap(),
        "unchanged"
    );

    // A changed input is rejected before composing or publishing any output.
    fs::write(root.join("base.tar"), "changed bytes").unwrap();
    let mismatch = produce(root, "bad-digest");
    assert!(!mismatch.status.success());
    assert!(String::from_utf8_lossy(&mismatch.stderr).contains("digest mismatch"));
    assert!(!root.join("bad-digest").exists());

    // Feed an actually malformed archive through the CLI, including a valid pin.
    let mut malformed = tar::Builder::new(fs::File::create(root.join("base.tar")).unwrap());
    let mut header = tar::Header::new_ustar();
    header.as_mut_bytes()[..9].copy_from_slice(b"../escape");
    header.set_mode(0o644);
    header.set_size(4);
    header.set_cksum();
    malformed.append(&header, b"oops".as_slice()).unwrap();
    malformed.finish().unwrap();
    drop(malformed);
    manifest(root);
    let invalid = produce(root, "bad-path");
    assert!(!invalid.status.success());
    assert!(String::from_utf8_lossy(&invalid.stderr).contains("unsafe archive path"));
    assert!(!root.join("bad-path").exists());
    assert!(!root.join("escape").exists());
    assert!(!root.parent().unwrap().join("escape").exists());

    // A whiteout name in an input would be an image deletion to the expander:
    // refused before anything is published.
    let mut whiteout = tar::Builder::new(fs::File::create(root.join("base.tar")).unwrap());
    tar_entry(&mut whiteout, "usr/.wh.removed", b"not a file");
    whiteout.finish().unwrap();
    drop(whiteout);
    manifest(root);
    let refused = produce(root, "whiteout");
    assert!(!refused.status.success());
    assert!(!root.join("whiteout").exists());

    // `etc/hosts` is Recipe's to inject: it never reaches the layer (refused,
    // or left out; either way the published image cannot carry it).
    let mut hosts = tar::Builder::new(fs::File::create(root.join("base.tar")).unwrap());
    tar_entry(&mut hosts, "usr/keep", b"base bytes");
    tar_entry(&mut hosts, "etc/hosts", b"127.0.0.1 image-owned");
    hosts.finish().unwrap();
    drop(hosts);
    manifest(root);
    let hosts = produce(root, "hosts");
    if hosts.status.success() {
        let provenance: serde_json::Value = serde_json::from_slice(&hosts.stdout).unwrap();
        let layer = provenance["layer_sha256"].as_str().unwrap();
        let mut archive =
            tar::Archive::new(fs::File::open(root.join("hosts/blobs/sha256").join(layer)).unwrap());
        for entry in archive.entries().unwrap() {
            assert_ne!(
                entry.unwrap().path().unwrap().to_string_lossy(),
                "etc/hosts"
            );
        }
    } else {
        assert!(!root.join("hosts").exists());
    }
}
