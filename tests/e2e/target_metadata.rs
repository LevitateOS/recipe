use super::*;
use std::fs::{self, File};
use std::os::unix::fs::{MetadataExt, PermissionsExt};

/// Real managed extraction, checkpoint recovery, publication/import, image
/// assembly. Actual recstrap deployment runs separately in the retained PRD12
/// libvirt fixture, using this exact image on an EROFS-zstd-capable kernel. No host chown/chmod is used to install
/// package privileges into staging.
#[test]
fn cli_target_metadata_survives_publication_reimport_and_erofs_production() {
    let (dir, recipes) = create_test_env();
    let archive = dir.path().join("package.tar");
    let capability = hex::decode("0100000200200000000000000000000000000000").unwrap();
    let mut tar = tar::Builder::new(File::create(&archive).unwrap());
    for (path, mode, kind, link, data) in [
        ("usr", 0o755, tar::EntryType::Directory, None, ""),
        ("usr/bin", 0o755, tar::EntryType::Directory, None, ""),
        (
            "usr/bin/helper",
            0o4750,
            tar::EntryType::Regular,
            None,
            "target payload\n",
        ),
        (
            "usr/bin/alias",
            0o4750,
            tar::EntryType::Link,
            Some("usr/bin/helper"),
            "",
        ),
        (
            "usr/bin/group-helper",
            0o2755,
            tar::EntryType::Regular,
            None,
            "setgid payload\n",
        ),
        ("etc", 0o755, tar::EntryType::Directory, None, ""),
        (
            "etc/os-release",
            0o644,
            tar::EntryType::Regular,
            None,
            "ID=levitate\nREC_PACKAGES=rpm\nREC_INIT=systemd\nREC_LIBC=glibc\nREC_PRIVILEGE=sudo\nREC_DESKTOP=plasma\nREC_KERNEL_LINE=lts\n",
        ),
        ("tmp", 0o1777, tar::EntryType::Directory, None, ""),
        ("bin", 0o777, tar::EntryType::Symlink, Some("/usr/bin"), ""),
        (
            "bin/through-alias",
            0o4111,
            tar::EntryType::Regular,
            None,
            "alias payload\n",
        ),
    ] {
        let mut h = tar::Header::new_gnu();
        h.set_mode(mode);
        h.set_uid(74);
        h.set_gid(81);
        h.set_mtime(0);
        h.set_entry_type(kind);
        h.set_size(data.len() as u64);
        if path.starts_with("usr/bin/") {
            tar.append_pax_extensions([(
                "SCHILY.xattr.security.capability",
                capability.as_slice(),
            )])
            .unwrap();
        }
        if let Some(link) = link {
            tar.append_link(&mut h, path, link).unwrap();
        } else {
            tar.append_data(&mut h, path, data.as_bytes()).unwrap();
        }
    }
    tar.finish().unwrap();
    drop(tar);
    write_recipe(
        &recipes,
        "target",
        r#"
const RECIPE_API = 2;
const NEEDS = #{ inputs: #{ "package": "file" }, outputs: #{ rootfs: "directory" } };
let ctx = #{name: "target-metadata"};
fn is_acquired(ctx) { ready(ctx) }
fn is_built(ctx) {
    if !is_file("/out/rootfs/.recipe-target-metadata.json") { return needs_work(ctx, "extract package"); }
    ready(ctx)
}
fn build(ctx) {
    if trim(exec_output("id", ["-u"], #{uid: 1000, gid: 1000})) != "1000" {
        throw "child virtual identity was not selected";
    }
    if trim(exec_output("id", ["-u"])) != "0" { throw "child changed worker identity"; }
    target_extract_tar("/inputs/package", "/out/rootfs");
    target_mode("/out/rootfs", "bin/through-alias", 0o5111);
    // Package clients can leave execute-only staging inodes.
    chmod("/out/rootfs/usr/bin/helper", 0o111);
    target_finalize("/out/rootfs");
    ctx
}
fn is_installed(ctx) {
    if !is_file("/out/rootfs/finished") { return needs_work(ctx, "finish after build checkpoint"); }
    ctx.outputs = #{rootfs: "/out/rootfs"}; ready(ctx)
}
fn install(ctx) {
    write_file("/out/rootfs/finished", "ready\n");
    target_finalize("/out/rootfs");
    ctx.outputs = #{rootfs: "/out/rootfs"}; ctx
}
"#,
    );
    let output_json = dir.path().join("first.json");
    for _ in 0..2 {
        // Second execution restores the saved build/install checkpoints.
        let out = recipe_command(&recipes)
            .args(["install", "target"])
            .arg("--recipes-path")
            .arg(&recipes)
            .arg("--build-dir")
            .arg(dir.path().join("work"))
            .arg("--input")
            .arg(format!("package={}", archive.display()))
            .arg("--json-output")
            .arg(&output_json)
            .output()
            .unwrap();
        assert_success(&out);
    }
    let ctx: serde_json::Value = serde_json::from_slice(&fs::read(output_json).unwrap()).unwrap();
    let published = Path::new(ctx["outputs"]["rootfs"].as_str().unwrap());
    // Staging never acquired the package's host privileges.
    assert_eq!(
        fs::metadata(published.join("usr/bin/helper"))
            .unwrap()
            .mode()
            & 0o7000,
        0
    );
    write_recipe(
        &recipes,
        "import",
        r#"
const RECIPE_API = 2;
const NEEDS = #{ inputs: #{ target: "directory" }, outputs: #{ rootfs: "directory" } };
let ctx = #{name: "target-import"};
fn is_built(ctx) { ready(ctx) }
fn is_installed(ctx) {
    if !is_file("/out/roundtrip") { return needs_work(ctx, "record import"); }
    ctx.outputs = #{rootfs: "/out"}; ready(ctx)
}
fn install(ctx) {
    write_file("/out/roundtrip", "imported\n");
    target_finalize("/out");
    ctx.outputs = #{rootfs: "/out"}; ctx
}
"#,
    );
    let output_json = dir.path().join("second.json");
    let out = recipe_command(&recipes)
        .args(["install", "import", "--no-persist-state"])
        .arg("--recipes-path")
        .arg(&recipes)
        .arg("--build-dir")
        .arg(dir.path().join("import-work"))
        .arg("--input")
        .arg(format!("target={}", published.display()))
        .arg("--json-output")
        .arg(&output_json)
        .output()
        .unwrap();
    assert_success(&out);
    let ctx: serde_json::Value = serde_json::from_slice(&fs::read(output_json).unwrap()).unwrap();
    let published = Path::new(ctx["outputs"]["rootfs"].as_str().unwrap());
    let prepared = dir.path().join("prepared");
    fs::create_dir(&prepared).unwrap();
    let merged = prepared.join("composed");
    levitate_recipe::target::merge_tree(published, &merged).unwrap();
    fs::create_dir(prepared.join("overlay")).unwrap();
    fs::write(
        prepared.join(".prepared-product.json"),
        serde_json::to_vec(&serde_json::json!({
            "product":"base-rootfs", "distro_id":"levitate", "rootfs_source_dir":"composed",
            "overlay_dir":"overlay", "rootfs_erofs":"target.erofs", "overlay_erofs":"overlay.erofs"
        }))
        .unwrap(),
    )
    .unwrap();
    let builder =
        std::env::var_os("RECIPE_TEST_BUILDER").expect("select the actual distro-builder binary");
    let repo = std::env::var_os("RECIPE_TEST_REPO_ROOT").expect("select the builder checkout");
    let image = prepared.join("target.erofs");
    let out = Command::new(builder)
        .arg("--repo-root")
        .arg(repo)
        .args(["transform", "build", "product-erofs"])
        .arg(&prepared)
        .output()
        .unwrap();
    assert_success(&out);
    // Inspect the real target tar owner as well as producing the actual image.
    // Do not use fsck.erofs extraction as capability-preserving deployment:
    // its chown order clears capabilities on this host's erofs-utils version.
    let stream = dir.path().join("target.tar");
    levitate_recipe::target::write_tar(&merged, File::create(&stream).unwrap()).unwrap();
    let mut archive = tar::Archive::new(File::open(&stream).unwrap());
    let mut members = std::collections::BTreeMap::new();
    for entry in archive.entries().unwrap() {
        let mut entry = entry.unwrap();
        let name = entry.path().unwrap().to_string_lossy().into_owned();
        let header = entry.header().clone();
        let link = entry.link_name().unwrap().map(|p| p.into_owned());
        let attributes = entry
            .pax_extensions()
            .unwrap()
            .into_iter()
            .flatten()
            .map(|p| {
                let p = p.unwrap();
                (p.key_bytes().to_vec(), p.value_bytes().to_vec())
            })
            .collect::<std::collections::BTreeMap<_, _>>();
        members.insert(name, (header, link, attributes));
    }
    for (name, mode) in [
        ("usr/bin/helper", 0o4750),
        ("usr/bin/group-helper", 0o2755),
        ("usr/bin/through-alias", 0o5111),
        ("tmp", 0o1777),
    ] {
        let (header, _, _) = &members[name];
        assert_eq!(
            (
                header.mode().unwrap(),
                header.uid().unwrap(),
                header.gid().unwrap()
            ),
            (mode, 74, 81)
        );
        assert_eq!(
            header.mtime().unwrap(),
            0,
            "the image producer deliberately normalizes mtimes"
        );
    }
    // The deterministic tar writer chooses one of the aliases as its data member.
    let (first, second) = (&members["usr/bin/alias"], &members["usr/bin/helper"]);
    assert!(first.0.entry_type().is_hard_link() || second.0.entry_type().is_hard_link());
    assert_eq!(members["bin"].1.as_deref(), Some(Path::new("/usr/bin")));
    let attribute = b"SCHILY.xattr.security.capability".as_slice();
    assert!(
        first.2.get(attribute) == Some(&capability) || second.2.get(attribute) == Some(&capability)
    );
    println!("TARGET_METADATA_IMAGE {}", image.display());
    // Replacing a privileged payload without replacing its metadata must fail.
    fs::set_permissions(
        merged.join("usr/bin/helper"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    fs::write(merged.join("usr/bin/helper"), "different binary").unwrap();
    assert!(levitate_recipe::target::write_tar(&merged, std::io::sink()).is_err());
}
