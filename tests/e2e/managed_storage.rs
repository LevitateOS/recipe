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
const NEEDS = #{ inputs: #{ rootfs: "directory" }, outputs: #{ rootfs: "directory", alias: "file" } };
let ctx = #{name: "storage-snapshot", state: #{installed: false}};
fn is_acquired(ctx) { ready(ctx) }
fn is_built(ctx) {
    if is_file("/out/rootfs/usr/bin/tool") { return ready(ctx); }
    needs_work(ctx, "copy explicit rootfs")
}
fn build(ctx) {
    mkdir("/out/rootfs");
    copy_tree_contents("/inputs/rootfs", "/out/rootfs");
    exec_checked("/usr/bin/touch",["-d","@1234567890","/out/rootfs/usr/bin/tool"]);
    exec_checked("/usr/bin/touch",["-h","-d","@1234567891","/out/rootfs/absolute-link"]);
    exec_checked("/usr/bin/touch",["-d","@1234567892","/out/rootfs/usr/bin"]);
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
    assert_eq!(std::fs::metadata(&first_file).unwrap().mtime(), 1234567890);
    assert_eq!(
        std::fs::symlink_metadata(first_root.join("absolute-link"))
            .unwrap()
            .mtime(),
        1234567891
    );
    assert_eq!(
        std::fs::metadata(first_root.join("usr/bin"))
            .unwrap()
            .mtime(),
        1234567892
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

#[test]
fn managed_storage_restores_and_publishes_more_than_4096_entries() {
    let (dir, recipes) = create_test_env();
    let target = dir.path().join("selected-tree");
    std::fs::create_dir(&target).unwrap();
    for index in 0..5000 {
        std::fs::write(target.join(format!("item-{index:04}")), index.to_string()).unwrap();
    }
    write_recipe(
        &recipes,
        "check",
        r#"
const RECIPE_API = 2;
const NEEDS = #{ inputs: #{ target: "directory" }, outputs: #{ tree: "directory" } };
let ctx = #{name: "large-namespace"};
fn is_installed(ctx) {
    if read_file("/out/item-0000")!="0" || read_file("/out/item-4999")!="4999" { throw "target lost selected entries"; }
    ctx.outputs.tree="/out"; ready(ctx)
}
"#,
    );
    let output = recipe_command(&recipes)
        .args(["isinstalled", "check", "--recipes-path"])
        .arg(&recipes)
        .arg("--build-dir")
        .arg(dir.path().join("destination"))
        .arg("--state-dir")
        .arg(dir.path().join("state"))
        .arg("--input")
        .arg(format!("target={}", target.display()))
        .output()
        .unwrap();
    assert_success(&output);
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let published = Path::new(result["outputs"]["tree"].as_str().unwrap());
    assert_eq!(std::fs::read_dir(published).unwrap().count(), 5000);
    for index in 0..5000 {
        assert_eq!(
            std::fs::read_to_string(published.join(format!("item-{index:04}"))).unwrap(),
            index.to_string()
        );
    }
    assert_eq!(std::fs::read_dir(target).unwrap().count(), 5000);
}

#[test]
fn managed_storage_publishes_actual_payload_larger_than_two_gib_under_explicit_bounds() {
    use std::io::{Read, Seek, SeekFrom};
    let (dir, recipes) = create_test_env();
    write_recipe(
        &recipes,
        "check",
        r##"
const RECIPE_API = 2;
const NEEDS = #{ outputs: #{ payload: "file" }, memory_mib: 4096, storage_max_bytes: 3221225472 };
let ctx = #{name: "large-payload", state: #{installed:false}};
fn is_acquired(ctx) { ready(ctx) }
fn is_built(ctx) { ready(ctx) }
fn is_installed(ctx) {
    if !ctx.state.installed { return needs_work(ctx,"create bounded payload"); }
    ctx.outputs.payload="/out/large"; ready(ctx)
}
fn install(ctx) {
    write_file("/work/build/payload.c", "#include <unistd.h>\n#include <fcntl.h>\n#include <stdio.h>\nint main(void){long long n=2147487744LL;int f=open(\"/out/large\",O_RDWR|O_CREAT|O_EXCL,0600);if(f<0||ftruncate(f,n)||pwrite(f,\"tailproof\",9,n-9)!=9){perror(\"bounded payload\");return 1;}return close(f)!=0;}\n");
    exec_checked("/usr/bin/gcc",["/work/build/payload.c","-o","/work/build/payload"]);
    exec_checked("/work/build/payload",[]);
    ctx.state.installed=true; ctx
}
"##,
    );
    let output = recipe_command(&recipes)
        .args(["install", "check", "--recipes-path"])
        .arg(&recipes)
        .arg("--build-dir")
        .arg(dir.path().join("destination"))
        .arg("--state-dir")
        .arg(dir.path().join("state"))
        .args([
            "--no-persist-state",
            "--memory-mib",
            "4096",
            "--storage-max-bytes",
            "3221225472",
        ])
        .output()
        .unwrap();
    assert_success(&output);
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let mut file = std::fs::File::open(result["outputs"]["payload"].as_str().unwrap()).unwrap();
    assert_eq!(file.metadata().unwrap().len(), 2147487744);
    let mut first = [1; 16];
    file.read_exact(&mut first).unwrap();
    assert_eq!(first, [0; 16]);
    file.seek(SeekFrom::End(-9)).unwrap();
    let mut tail = [0; 9];
    file.read_exact(&mut tail).unwrap();
    assert_eq!(&tail, b"tailproof");
    file.seek(SeekFrom::Start(0)).unwrap();
    let mut remaining = 2147487744u64 - 9;
    let mut chunk = vec![0; 1024 * 1024];
    while remaining != 0 {
        let count = remaining.min(chunk.len() as u64) as usize;
        file.read_exact(&mut chunk[..count]).unwrap();
        assert!(
            chunk[..count].iter().all(|byte| *byte == 0),
            "large payload changed before its tail"
        );
        remaining -= count as u64;
    }
    file.read_exact(&mut tail).unwrap();
    assert_eq!(&tail, b"tailproof");
}
