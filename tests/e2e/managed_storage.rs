//! Real source-tree copy, checkpoint/restart and named artifact publication.
use super::*;
use std::os::unix::fs::{MetadataExt, PermissionsExt};

#[test]
fn managed_storage_preserves_source_modes_links_and_restart_identity() {
    let (dir, recipes) = create_test_env();
    let source = dir.path().join("rootfs input");
    let bin = source.join("usr/bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(bin.join("tool"), "original").unwrap();
    std::fs::set_permissions(bin.join("tool"), std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::hard_link(bin.join("tool"), bin.join("tool-alias")).unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::os::unix::fs::symlink("/usr/bin/tool", source.join("absolute-link")).unwrap();
    write_recipe(
        &recipes,
        "check",
        r#"
const RECIPE_API = 2;
let ctx = #{name: "storage-snapshot", state: #{installed: false}};
fn is_acquired(ctx) { ready(ctx) }
fn is_built(ctx) {
    if is_file("/out/rootfs/usr/bin/tool") { return ready(ctx); }
    needs_work(ctx, "copy explicit rootfs")
}
fn build(ctx) {
    mkdir("/out/rootfs");
    copy_tree_contents("/inputs/rootfs", "/out/rootfs");
    ctx
}
fn is_installed(ctx) {
    if !ctx.state.installed { return needs_work(ctx, "publish copied rootfs"); }
    if read_file("/out/rootfs/usr/bin/tool") != "original" { throw "copy changed source bytes"; }
    ctx.outputs = #{rootfs: "/out/rootfs", alias: "/out/rootfs/usr/bin/tool-alias"};
    ready(ctx)
}
fn install(ctx) { ctx.state.installed = true; ctx }
fn cleanup(ctx, reason) {
    if reason != "manual" { return ctx; }
    write_file("/out/rootfs/usr/bin/tool-alias", "changed through restored alias");
    if read_file("/out/rootfs/usr/bin/tool") != "changed through restored alias" {
        throw "restart lost shared hardlink backing";
    }
    ctx.outputs = #{rootfs: "/out/rootfs", alias: "/out/rootfs/usr/bin/tool-alias"};
    ctx
}
"#,
    );
    let invoke = |operation: &str| {
        let output = recipe_command(&recipes)
            .args([operation, "check", "--recipes-path"])
            .arg(&recipes)
            .arg("--build-dir")
            .arg(dir.path().join("destination"))
            .arg("--state-dir")
            .arg(dir.path().join("retained"))
            .arg("--input")
            .arg(format!("rootfs={}", source.display()))
            .output()
            .unwrap();
        assert_success(&output);
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap()
    };
    let first = invoke("install");
    let first_root = Path::new(first["outputs"]["rootfs"].as_str().unwrap());
    let first_file = first_root.join("usr/bin/tool");
    let first_alias = first_root.join("usr/bin/tool-alias");
    assert_eq!(
        std::fs::metadata(&first_file).unwrap().ino(),
        std::fs::metadata(&first_alias).unwrap().ino()
    );
    assert_eq!(
        std::fs::metadata(first["outputs"]["alias"].as_str().unwrap())
            .unwrap()
            .ino(),
        std::fs::metadata(&first_file).unwrap().ino()
    );
    assert_eq!(
        std::fs::metadata(&first_file).unwrap().mode() & 0o777,
        0o600
    );
    assert_eq!(
        std::fs::metadata(first_root.join("usr/bin"))
            .unwrap()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        std::fs::read_link(first_root.join("absolute-link")).unwrap(),
        Path::new("/usr/bin/tool")
    );
    let second = invoke("cleanup");
    let second_file = Path::new(second["outputs"]["rootfs"].as_str().unwrap()).join("usr/bin/tool");
    assert_eq!(
        std::fs::read_to_string(second_file).unwrap(),
        "changed through restored alias"
    );
    assert_eq!(std::fs::read_to_string(first_file).unwrap(), "original");
    assert_eq!(
        std::fs::read_to_string(bin.join("tool")).unwrap(),
        "original"
    );
    assert_eq!(
        std::fs::metadata(bin.join("tool")).unwrap().mode() & 0o777,
        0o600
    );
}
