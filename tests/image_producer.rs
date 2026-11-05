//! Real bootstrap CLI composition and failure behavior over local archive inputs.
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
    for output in ["first", "second"] {
        let result = produce(root, output);
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let provenance: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
        assert_eq!(
            provenance["sha256"],
            digest(&root.join(output).join("rootfs.tar"))
        );
        assert_eq!(
            fs::read(root.join(output).join("provenance.json")).unwrap(),
            result.stdout
        );
    }
    assert_eq!(
        digest(&root.join("first/rootfs.tar")),
        digest(&root.join("second/rootfs.tar"))
    );
    let mut archive = tar::Archive::new(fs::File::open(root.join("first/rootfs.tar")).unwrap());
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
}
