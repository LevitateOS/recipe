use super::*;
use std::fs::{self, File};
use std::os::unix::fs::{MetadataExt, PermissionsExt};

/// Real managed extraction, checkpoint recovery, publication/import, image
/// assembly and deployment extraction. No host chown/chmod is used to install
/// package privileges into staging.
#[test]
fn cli_target_metadata_survives_publication_reimport_and_erofs() {
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
    let merged = dir.path().join("composed");
    levitate_recipe::target::merge_tree(published, &merged).unwrap();
    let stream = dir.path().join("target.tar");
    levitate_recipe::target::write_tar(&merged, File::create(&stream).unwrap()).unwrap();
    let image = dir.path().join("target.erofs");
    let out = Command::new("mkfs.erofs")
        .args(["--tar=f", "-T0"])
        .arg(&image)
        .arg(&stream)
        .output()
        .unwrap();
    assert_success(&out);
    let deployed = dir.path().join("deployed");
    let mount = dir.path().join("mounted");
    fs::create_dir(&mount).unwrap();
    fs::create_dir(&deployed).unwrap();
    // Match recstrap's actual deployment path. fsck.erofs 1.8.10 sets
    // capabilities before chown and consequently clears them on extraction.
    let out = Command::new("unshare")
        .args([
            "--mount",
            "--propagation",
            "private",
            "sh",
            "-eu",
            "-c",
            r#"trap 'umount "$2"' EXIT
mount -t erofs -o loop,ro "$1" "$2"
cp -aT --preserve=xattr "$2" "$3"
"#,
            "metadata-deploy",
        ])
        .arg(&image)
        .arg(&mount)
        .arg(&deployed)
        .output()
        .unwrap();
    assert_success(&out);
    assert_eq!(
        fs::metadata(deployed.join("usr/bin/through-alias"))
            .unwrap()
            .mode()
            & 0o7777,
        0o5111
    );
    let md = fs::metadata(deployed.join("usr/bin/helper")).unwrap();
    assert_eq!((md.mode() & 0o7777, md.uid(), md.gid()), (0o4750, 74, 81));
    assert_eq!(
        fs::metadata(deployed.join("tmp")).unwrap().mode() & 0o7777,
        0o1777
    );
    assert_eq!(
        fs::read_link(deployed.join("bin")).unwrap(),
        Path::new("/usr/bin")
    );
    assert_eq!(
        md.ino(),
        fs::metadata(deployed.join("usr/bin/alias")).unwrap().ino()
    );
    let file = std::ffi::CString::new(deployed.join("usr/bin/helper").to_str().unwrap()).unwrap();
    let name = std::ffi::CString::new("security.capability").unwrap();
    let mut value = vec![0u8; 64];
    let count = unsafe {
        libc::getxattr(
            file.as_ptr(),
            name.as_ptr(),
            value.as_mut_ptr().cast(),
            value.len(),
        )
    };
    assert!(count >= 0, "getxattr: {}", std::io::Error::last_os_error());
    value.truncate(count as usize);
    assert_eq!(value, capability);
    // Replacing a privileged payload without replacing its metadata must fail.
    fs::set_permissions(
        merged.join("usr/bin/helper"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    fs::write(merged.join("usr/bin/helper"), "different binary").unwrap();
    assert!(levitate_recipe::target::write_tar(&merged, std::io::sink()).is_err());
}
