//! Real source-tree copy, kept-result restart and named artifact publication
//! on the public Ginux lifecycle. Preconditions: header of `tests/e2e.rs`.
//!
//! Storage is the instance root on the host filesystem: there is no storage
//! limit inside the run (accepted regression, `docs/runtime-limits.md`). The
//! bound that remains is on the way out: `ginux extract --max-bytes` refuses
//! outputs over `storage_max_bytes` after the run. Inputs and outputs travel
//! as tars of directories, regular files and symlinks, so hard links do not
//! survive: a file linked twice arrives as two independent files.
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
    // Staged inputs have no hard links (the injector accepts none): this
    // arrives as a second regular file with the same bytes.
    std::fs::hard_link(bin.join("tool"), bin.join("tool-alias")).unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::os::unix::fs::symlink("/usr/bin/tool", source.join("absolute-link")).unwrap();
    write_recipe(
        &recipes,
        "check",
        r#"
const RECIPE_API = 1;
const NEEDS = #{ inputs: #{ rootfs: "directory" }, outputs: #{ rootfs: "directory" } };
fn build(src, out) {
    print("BUILD-RAN");
    copy_tree_contents(INPUTS.rootfs, out);
    exec_checked("/usr/bin/touch",["-d","@1234567890", join_path(out, "usr/bin/tool")]);
    exec_checked("/usr/bin/touch",["-h","-d","@1234567891", join_path(out, "absolute-link")]);
    exec_checked("/usr/bin/touch",["-d","@1234567892", join_path(out, "usr/bin")]);
}
fn verify(out) {
    if read_file(join_path(out, "usr/bin/tool")) != "original" { throw "copy changed source bytes"; }
}
"#,
    );
    let invoke = || {
        let output = recipe_command(&recipes)
            .args(["install", "check", "--keep", "--recipes-path"])
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
        let built = String::from_utf8_lossy(&output.stderr).contains("BUILD-RAN");
        (
            serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap(),
            built,
        )
    };
    // The first run builds; the second publishes the kept result, restored
    // from its snapshot, and must show the same files.
    let mut roots = Vec::new();
    for builds in [true, false] {
        let (result, built) = invoke();
        assert_eq!(built, builds);
        let root = std::path::PathBuf::from(result["outputs"]["rootfs"].as_str().unwrap());
        let file = root.join("usr/bin/tool");
        assert_eq!(
            std::fs::read(&file).unwrap(),
            std::fs::read(root.join("usr/bin/tool-alias")).unwrap()
        );
        assert_ne!(
            std::fs::metadata(&file).unwrap().ino(),
            std::fs::metadata(root.join("usr/bin/tool-alias"))
                .unwrap()
                .ino(),
            "hard links are not carried through the instance"
        );
        assert_eq!(std::fs::metadata(&file).unwrap().mode() & 0o777, 0o600);
        assert_eq!(
            std::fs::metadata(root.join("usr/bin")).unwrap().mode() & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::read_link(root.join("absolute-link")).unwrap(),
            Path::new("/usr/bin/tool")
        );
        assert_eq!(std::fs::metadata(&file).unwrap().mtime(), 1234567890);
        assert_eq!(
            std::fs::symlink_metadata(root.join("absolute-link"))
                .unwrap()
                .mtime(),
            1234567891
        );
        assert_eq!(
            std::fs::metadata(root.join("usr/bin")).unwrap().mtime(),
            1234567892
        );
        roots.push(root);
    }
    assert_ne!(roots[0], roots[1]);
    assert_eq!(
        std::fs::read_to_string(roots[0].join("usr/bin/tool")).unwrap(),
        "original"
    );
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
const RECIPE_API = 1;
const NEEDS = #{ inputs: #{ target: "directory" }, outputs: #{ tree: "directory" } };
fn build(src, out) {
    if read_file(join_path(out, "item-0000")) != "0" || read_file(join_path(out, "item-4999")) != "4999" { throw "target lost selected entries"; }
}
"#,
    );
    let output = recipe_command(&recipes)
        .args(["install", "check", "--recipes-path"])
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
const RECIPE_API = 1;
const NEEDS = #{ outputs: #{ payload: "file" }, memory_mib: 4096, storage_max_bytes: 3221225472 };
fn build(src, out) {
    let source = join_path(WORK, "payload.c");
    write_file(source, "#include <unistd.h>\n#include <fcntl.h>\n#include <stdio.h>\nint main(int c,char**v){long long n=2147487744LL;int f=open(v[1],O_RDWR|O_CREAT|O_EXCL,0600);if(f<0||ftruncate(f,n)||pwrite(f,\"tailproof\",9,n-9)!=9){perror(\"bounded payload\");return 1;}return close(f)!=0;}\n");
    exec_checked("/usr/bin/gcc",[source,"-o",join_path(WORK, "payload")]);
    exec_checked(join_path(WORK, "payload"),[out]);
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

#[test]
fn managed_storage_refuses_outputs_over_the_declared_bound_at_extract() {
    // The run itself is not limited. The archive of `/work` and `/out` is
    // bounded by `storage_max_bytes` (plus tar overhead per object): a sparse
    // 256 MiB file under a 16 MiB bound is refused when the outputs are taken
    // out, with the storage message, and nothing is published.
    let (dir, recipes) = create_test_env();
    write_recipe(
        &recipes,
        "check",
        r#"
const RECIPE_API = 1;
const NEEDS = #{ outputs: #{ payload: "file" } };
fn build(src, out) { exec_checked("/usr/bin/truncate", ["-s", "268435456", out]); }
"#,
    );
    let output = recipe_command(&recipes)
        .args(["install", "check", "--no-persist-state", "--recipes-path"])
        .arg(&recipes)
        .arg("--build-dir")
        .arg(dir.path().join("destination"))
        .arg("--state-dir")
        .arg(dir.path().join("state"))
        .args([
            "--storage-max-bytes",
            "16777216",
            "--storage-max-objects",
            "100",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success() && output.stdout.is_empty());
    let diagnostics = String::from_utf8_lossy(&output.stderr);
    assert!(diagnostics.contains("exceed"), "{diagnostics}");
    assert!(!dir.path().join("state/recipe/exports").exists());
    assert!(leaked_instances().is_empty());
}
