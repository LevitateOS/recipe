use super::*;
use sha2::{Digest, Sha256};

fn scratch_directories(parent: &Path) -> Vec<std::path::PathBuf> {
    let pool = parent.join(format!("recipe-scratch-{}", unsafe { libc::geteuid() }));
    std::fs::read_dir(pool)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("invocation-")
        })
        .map(|entry| entry.path())
        .collect()
}

fn image_command(recipes: &Path, archive: &Path, digest: &str) -> Command {
    let mut command = recipe_command(recipes);
    command
        .env_remove("RECIPE_WORKER")
        .env_remove("RECIPE_INVOCATION")
        .env("TMPDIR", recipes.parent().unwrap())
        .env("GINUX_ROOTFS", archive)
        .env("GINUX_ROOT", format!("oci:cli-image@sha256:{digest}"))
        .args(["install", "check", "--no-persist-state", "--recipes-path"])
        .arg(recipes);
    command
}

fn guarded_recipe(recipes: &Path, sentinel: &Path) -> String {
    let source = format!(
        "const RECIPE_API = 1;\nconst NEEDS = #{{}};\nwrite_file({:?}, \"host recipe code ran\");\nfn build(src, out) {{}}\n",
        sentinel.to_string_lossy()
    );
    write_recipe(recipes, "check", &source);
    source
}

fn image_tar(path: &Path, entries: &[(&str, tar::EntryType, &str)]) -> String {
    let file = std::fs::File::create(path).unwrap();
    let mut archive = tar::Builder::new(file);
    for (name, kind, value) in entries {
        let mut header = tar::Header::new_ustar();
        // Construct hostile names literally: normal tar path setters reject
        // traversal before the CLI under test gets to inspect the archive.
        header.as_mut_bytes()[..name.len()].copy_from_slice(name.as_bytes());
        header.set_entry_type(*kind);
        header.set_mode(0o755);
        if kind.is_symlink() || kind.is_hard_link() {
            header.set_size(0);
            // Let tar encode long link targets. Regular hostile names above
            // remain literal so the CLI, not the fixture builder, rejects them.
            archive.append_link(&mut header, name, value).unwrap();
        } else {
            header.set_size(value.len() as u64);
            header.set_cksum();
            archive.append(&header, value.as_bytes()).unwrap();
        }
    }
    archive.finish().unwrap();
    drop(archive);
    hex::encode(Sha256::digest(std::fs::read(path).unwrap()))
}

#[test]
fn cli_missing_or_mismatched_root_image_fails_before_recipe_setup() {
    let (dir, recipes) = create_test_env();
    let sentinel = dir.path().join("host-effect");
    let source = guarded_recipe(&recipes, &sentinel);
    let archive = dir.path().join("image.tar");
    let missing = image_command(&recipes, &archive, &"0".repeat(64))
        .output()
        .unwrap();
    assert!(!missing.status.success());
    assert!(
        String::from_utf8_lossy(&missing.stderr).contains("cannot open the Ginux rootfs image")
    );
    image_tar(
        &archive,
        &[("etc/fact", tar::EntryType::Regular, "image fact")],
    );
    let mismatch = image_command(&recipes, &archive, &"0".repeat(64))
        .output()
        .unwrap();
    assert!(!mismatch.status.success());
    assert!(String::from_utf8_lossy(&mismatch.stderr).contains("Ginux rootfs digest mismatch"));
    assert!(!sentinel.exists());
    assert_eq!(
        std::fs::read_to_string(recipes.join("check.rhai")).unwrap(),
        source
    );
}

#[test]
fn cli_image_traversal_and_symlink_ancestors_preserve_host_files() {
    let (dir, recipes) = create_test_env();
    let sentinel = dir.path().join("sentinel");
    std::fs::write(&sentinel, "keep this host file").unwrap();
    let effect = dir.path().join("host-effect");
    guarded_recipe(&recipes, &effect);
    let archive = dir.path().join("image.tar");
    let outside = dir.path().to_string_lossy().into_owned();
    for (entries, wanted) in [
        (
            vec![("../sentinel", tar::EntryType::Regular, "replaced")],
            "unsafe rootfs image path",
        ),
        (
            vec![
                ("redirect", tar::EntryType::Symlink, outside.as_str()),
                ("redirect/sentinel", tar::EntryType::Regular, "replaced"),
            ],
            "traverses a non-directory",
        ),
        (
            vec![("alias", tar::EntryType::Link, "../sentinel")],
            "unsafe rootfs image path",
        ),
    ] {
        let digest = image_tar(&archive, &entries);
        let output = image_command(&recipes, &archive, &digest).output().unwrap();
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(wanted),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            std::fs::read_to_string(&sentinel).unwrap(),
            "keep this host file"
        );
        assert!(!effect.exists());
        assert!(scratch_directories(dir.path()).is_empty());
    }
}

#[test]
fn cli_image_cannot_replace_invocation_paths_or_duplicate_files() {
    let (dir, recipes) = create_test_env();
    let effect = dir.path().join("host-effect");
    guarded_recipe(&recipes, &effect);
    let archive = dir.path().join("image.tar");
    for (entries, wanted) in [
        (
            vec![(
                "inputs/recipe-worker",
                tar::EntryType::Regular,
                "replacement",
            )],
            "reserved invocation path",
        ),
        (
            vec![("work/build", tar::EntryType::Regular, "replacement")],
            "reserved invocation path",
        ),
        (
            vec![
                ("etc/fact", tar::EntryType::Regular, "one"),
                ("etc/fact", tar::EntryType::Regular, "two"),
            ],
            "duplicate rootfs image path",
        ),
    ] {
        let digest = image_tar(&archive, &entries);
        let output = image_command(&recipes, &archive, &digest).output().unwrap();
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(wanted),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!effect.exists());
        assert!(scratch_directories(dir.path()).is_empty());
    }
}

#[test]
fn cli_invalid_worker_cleans_up_a_prepared_readonly_image() {
    let (dir, recipes) = create_test_env();
    let effect = dir.path().join("host-effect");
    guarded_recipe(&recipes, &effect);
    let archive = dir.path().join("image.tar");
    let digest = image_tar(
        &archive,
        &[("etc/fact", tar::EntryType::Regular, "immutable fact")],
    );
    let worker = dir.path().join("bad-worker");
    std::fs::write(&worker, "this is not an ELF worker executable").unwrap();
    let output = image_command(&recipes, &archive, &digest)
        .env("RECIPE_WORKER_BIN", &worker)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("worker must be a Linux x86_64 ELF executable")
    );
    assert!(!effect.exists());
    assert!(scratch_directories(dir.path()).is_empty());
}

// These cases require the real configured launcher and its prepared image.
// Run with GINUX_BIN/GINUX_ROOTFS configured and the launcher's bootstrap
// privilege available. Missing prerequisites are failures, never skipped tests.
fn managed_command(recipes: &Path, operation: &str) -> Command {
    let launcher = std::env::var_os("GINUX_BIN")
        .expect("managed CLI checks require GINUX_BIN pointing to the real supervisor");
    let mut command = recipe_command(recipes);
    command
        .env("GINUX_BIN", launcher)
        .env_remove("RECIPE_WORKER")
        .env_remove("RECIPE_INVOCATION")
        .args([operation, "check", "--no-persist-state", "--recipes-path"])
        .arg(recipes);
    command
}

#[test]
fn managed_invocation_preserves_defines_and_cleanup_reason() {
    let (_dir, recipes) = create_test_env();
    write_recipe(
        &recipes,
        "check",
        r#"
const RECIPE_API = 1;
const NEEDS = #{ defines: #{ CUSTOM_INPUT: () }, outputs: #{ report: "file" } };
fn build(src, out) { write_file(out, DEFINES.CUSTOM_INPUT); }
fn cleanup(reason) { print("cleanup-saw " + reason + " " + DEFINES.CUSTOM_INPUT); }
"#,
    );
    let output = managed_command(&recipes, "install")
        .args(["--define", "CUSTOM_INPUT=caller value"])
        .output()
        .unwrap();
    assert_success(&output);
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        std::fs::read_to_string(result["outputs"]["report"].as_str().unwrap()).unwrap(),
        "caller value"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("cleanup-saw auto.build.success caller value")
    );
    // `recipe cleanup` calls the recipe's own cleanup with the reason "manual".
    let output = managed_command(&recipes, "cleanup")
        .args(["--define", "CUSTOM_INPUT=caller value"])
        .output()
        .unwrap();
    assert_success(&output);
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["operation"], "cleanup");
    assert!(String::from_utf8_lossy(&output.stderr).contains("cleanup-saw manual caller value"));
}

#[test]
fn managed_inputs_preserve_nested_modules_and_dependency_closure() {
    let (_dir, recipes) = create_test_env();
    for name in ["left", "right", "tools"] {
        std::fs::create_dir(recipes.join(name)).unwrap();
    }
    std::fs::write(recipes.join("left/facts.rhai"), "fn value() { \"left\" }").unwrap();
    std::fs::write(recipes.join("right/facts.rhai"), "fn value() { \"right\" }").unwrap();
    std::fs::write(
        recipes.join("left/wrapper.rhai"),
        "import \"facts\" as facts; fn value() { facts::value() }",
    )
    .unwrap();
    std::fs::write(
        recipes.join("tools/helper.rhai"),
        r#"
const RECIPE_API = 1;
const NEEDS = #{ outputs: #{ tools: "directory" } };
fn build(src, out) { write_file(join_path(out, "dependency-evidence"), "dependency ran"); }
"#,
    )
    .unwrap();
    let source = r#"
const RECIPE_API = 1;
const NEEDS = #{ outputs: #{ report: "file" } };
import "left/wrapper" as left;
import "right/facts.rhai" as right;
let deps = ["tools/helper"];
fn build(src, out) {
    let evidence = read_file(join_path(DEPS["tools/helper"], "dependency-evidence"));
    write_file(out, left::value() + ":" + right::value() + "|" + evidence);
}
"#;
    write_recipe(&recipes, "check", source);
    let output = managed_command(&recipes, "install").output().unwrap();
    assert_success(&output);
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        std::fs::read_to_string(result["outputs"]["report"].as_str().unwrap()).unwrap(),
        "left:right|dependency ran"
    );
    assert_eq!(
        std::fs::read_to_string(recipes.join("check.rhai")).unwrap(),
        source
    );
}

#[test]
fn managed_declared_source_tree_preserves_empty_directories_and_confined_links() {
    let (dir, recipes) = create_test_env();
    let source = dir.path().join("source");
    let long = "nested/long-component/long-component/long-component/long-component/long-component/long-component/long-component/value";
    std::fs::create_dir_all(source.join(
        "nested/long-component/long-component/long-component/long-component/long-component/long-component/long-component/",
    ))
    .unwrap();
    std::fs::write(source.join(long), "declared input").unwrap();
    std::fs::write(source.join("C++ résumé.txt"), "named bytes").unwrap();
    write_recipe(
        &recipes,
        "check",
        &r#"
const RECIPE_API = 1;
const NEEDS = #{ inputs: #{ "source": "directory", "config.toml": "file" }, outputs: #{ report: "file" } };
fn build(src, out) {
    let tree = INPUTS.source;
    if !is_dir(join_path(tree, "empty")) { throw "empty input directory missing"; }
    write_file(out, read_file(join_path(tree, "chain")));
    if read_file(join_path(tree, "C++ résumé.txt")) != "named bytes" { throw "source name changed"; }
    if read_file(INPUTS["config.toml"]) != "named bytes" { throw "input collided with coordinator metadata"; }
    let denied = false;
    try { write_file(join_path(tree, "LONG_PATH"), "changed"); } catch (error) { denied = true; }
    if !denied { throw "input accepted mutation"; }
}
"#
        .replace("LONG_PATH", long),
    );
    std::fs::create_dir(source.join("empty")).unwrap();
    std::os::unix::fs::symlink(long, source.join("alias")).unwrap();
    std::os::unix::fs::symlink("alias", source.join("chain")).unwrap();
    let selection = format!("source={}", source.display());
    let config = format!("config.toml={}", source.join("C++ résumé.txt").display());
    let output = managed_command(&recipes, "install")
        .args(["--input", &selection, "--input", &config])
        .output()
        .unwrap();
    assert_success(&output);
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        std::fs::read_to_string(result["outputs"]["report"].as_str().unwrap()).unwrap(),
        "declared input"
    );
    assert_eq!(
        std::fs::read_to_string(source.join(long)).unwrap(),
        "declared input"
    );
    std::fs::remove_file(source.join("alias")).unwrap();
    std::os::unix::fs::symlink(source.join(long), source.join("alias")).unwrap();
    let rejected = managed_command(&recipes, "install")
        .args(["--input", &selection, "--input", &config])
        .output()
        .unwrap();
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("symlink"));
    std::fs::remove_file(source.join("alias")).unwrap();
    for target in ["../outside", "chain", "nested/up/../outside"] {
        // nested/up -> .. makes the last path escape after link expansion,
        // despite its apparently contained lexical spelling.
        let up = source.join("nested/up");
        if std::fs::symlink_metadata(&up).is_err() {
            std::os::unix::fs::symlink("..", &up).unwrap();
        }
        std::os::unix::fs::symlink(target, source.join("alias")).unwrap();
        let rejected = managed_command(&recipes, "install")
            .args(["--input", &selection, "--input", &config])
            .output()
            .unwrap();
        assert!(
            !rejected.status.success(),
            "accepted invalid link: {target}"
        );
        assert!(String::from_utf8_lossy(&rejected.stderr).contains("symlink"));
        std::fs::remove_file(source.join("alias")).unwrap();
    }
}

#[test]
fn managed_large_asset_is_streamed_and_oversized_asset_rejected() {
    use std::io::{Seek, SeekFrom, Write};
    let (dir, recipes) = create_test_env();
    let asset = dir.path().join("asset.iso");
    let size = 513u64 * 1024 * 1024 + 7;
    let mut file = std::fs::File::create(&asset).unwrap();
    file.set_len(size).unwrap();
    file.seek(SeekFrom::End(-7)).unwrap();
    file.write_all(b"payload").unwrap();
    drop(file);
    write_recipe(
        &recipes,
        "check",
        &r#"
const RECIPE_API = 1;
const NEEDS = #{ inputs: #{ "asset.iso": "file" } };
fn build(src, out) {
    let asset = INPUTS["asset.iso"];
    if trim(shell_output("wc -c < " + asset)) != "ASSET_SIZE" { throw "asset truncated"; }
    if shell_output("tail -c 7 " + asset) != "payload" { throw "asset bytes changed"; }
}
"#
        .replace("ASSET_SIZE", &size.to_string()),
    );
    let selection = format!("asset.iso={}", asset.display());
    let output = managed_command(&recipes, "install")
        .args(["--input", &selection])
        .output()
        .unwrap();
    assert_success(&output);
    assert_eq!(std::fs::metadata(&asset).unwrap().len(), size);
    std::fs::OpenOptions::new()
        .write(true)
        .open(&asset)
        .unwrap()
        .set_len(16 * 1024 * 1024 * 1024 + 1)
        .unwrap();
    let rejected = managed_command(&recipes, "install")
        .args(["--input", &selection])
        .output()
        .unwrap();
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("exceeding"));
}

#[test]
fn managed_build_sees_the_engines_constants_and_returns_its_output() {
    use std::os::unix::fs::PermissionsExt;
    let (_dir, recipes) = create_test_env();
    let source = r#"
const RECIPE_API = 1;
const NEEDS = #{ outputs: #{ report: "file" } };
fn build(src, out) {
    if WORK != "/work/build" { throw "WORK is " + WORK; }
    if PREFIX != () { throw "PREFIX is set without a package output"; }
    if JOBS < 1 || JOBS > 16 { throw "JOBS is outside 1 to 16"; }
    if HOST_LIBC != "glibc" && HOST_LIBC != "musl" && HOST_LIBC != "unknown" { throw "HOST_LIBC is " + HOST_LIBC; }
    write_file(out, "constants ok");
}
"#;
    write_recipe(&recipes, "check", source);
    std::fs::set_permissions(
        recipes.join("check.rhai"),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let output = managed_command(&recipes, "install").output().unwrap();
    assert_success(&output);
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["operation"], "install");
    assert_eq!(
        std::fs::read_to_string(result["outputs"]["report"].as_str().unwrap()).unwrap(),
        "constants ok"
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("launcher exit 0; result exited"));
    assert_eq!(
        std::fs::read_to_string(recipes.join("check.rhai")).unwrap(),
        source
    );
}

#[test]
fn managed_script_errors_remain_failures() {
    let (_dir, recipes) = create_test_env();
    for (body, wanted) in [
        ("throw \"configuration differs\";", "configuration differs"),
        (
            "undefined_managed_function();",
            "undefined_managed_function",
        ),
    ] {
        let source = format!(
            "const RECIPE_API = 1;\nconst NEEDS = #{{}};\nfn build(src, out) {{ {body} }}\n"
        );
        write_recipe(&recipes, "check", &source);
        let output = managed_command(&recipes, "install").output().unwrap();
        assert!(!output.status.success());
        let diagnostics = String::from_utf8_lossy(&output.stderr);
        assert!(diagnostics.contains(wanted), "{diagnostics}");
        assert!(diagnostics.contains("workload failure"), "{diagnostics}");
        assert_eq!(
            std::fs::read_to_string(recipes.join("check.rhai")).unwrap(),
            source
        );
    }
}

#[test]
fn managed_setup_cannot_write_to_an_absolute_host_path() {
    let (dir, recipes) = create_test_env();
    let sentinel = dir.path().join("host-only-marker");
    std::fs::write(&sentinel, "keep host content").unwrap();
    let source = guarded_recipe(&recipes, &sentinel);
    let output = managed_command(&recipes, "install").output().unwrap();
    assert!(!output.status.success());
    let diagnostics = String::from_utf8_lossy(&output.stderr);
    assert!(diagnostics.contains("workload failure"), "{diagnostics}");
    assert!(
        diagnostics.contains("write_file") || diagnostics.contains("Permission denied"),
        "{diagnostics}"
    );
    assert_eq!(
        std::fs::read_to_string(&sentinel).unwrap(),
        "keep host content"
    );
    assert_eq!(
        std::fs::read_to_string(recipes.join("check.rhai")).unwrap(),
        source
    );
}

#[test]
fn managed_build_reads_staged_base_and_imported_module() {
    let (_dir, recipes) = create_test_env();
    let module = "fn selected_fact() { \"pinned module fact\" }\n";
    let base = r#"
const RECIPE_API = 1;
import "facts.rhai" as facts;
fn label() { "base" }
fn build(src, out) { write_file(out, label() + "|" + facts::selected_fact()); }
"#;
    let plain_child = r#"//! extends: base.rhai
const RECIPE_API = 1;
const NEEDS = #{ outputs: #{ report: "file" } };
"#;
    let shared_child = r#"//! extends: base.rhai
const RECIPE_API = 1;
const NEEDS = #{ outputs: #{ report: "file" } };
fn label() { "child" }
"#;
    write_recipe(&recipes, "facts", module);
    write_recipe(&recipes, "base", base);
    for (child, wanted) in [
        (plain_child, "base|pinned module fact"),
        (shared_child, "child|pinned module fact"),
    ] {
        write_recipe(&recipes, "check", child);
        let output = managed_command(&recipes, "install").output().unwrap();
        assert_success(&output);
        let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(
            std::fs::read_to_string(result["outputs"]["report"].as_str().unwrap()).unwrap(),
            wanted
        );
        for (name, source) in [("facts", module), ("base", base), ("check", child)] {
            assert_eq!(
                std::fs::read_to_string(recipes.join(format!("{name}.rhai"))).unwrap(),
                source
            );
        }
    }
}

#[test]
fn managed_cleanup_without_a_function_is_a_noop() {
    let (_dir, recipes) = create_test_env();
    let source = "const RECIPE_API = 1;\nconst NEEDS = #{};\n";
    write_recipe(&recipes, "check", source);
    let output = managed_command(&recipes, "cleanup").output().unwrap();
    assert_success(&output);
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["operation"], "cleanup");
    assert_eq!(result["name"], "check");
    assert_eq!(
        std::fs::read_to_string(recipes.join("check.rhai")).unwrap(),
        source
    );
}

#[test]
fn managed_cleanup_reason_follows_the_step_that_ended() {
    let (_dir, recipes) = create_test_env();
    let source = r#"
const RECIPE_API = 1;
const NEEDS = #{ defines: #{ FAIL: "no" }, outputs: #{ product: "file" } };
fn build(src, out) {
    write_file(join_path(WORK, "source"), "prepared source");
    write_file(join_path(WORK, "built"), read_file(join_path(WORK, "source")) + ": built");
    if DEFINES.FAIL == "yes" { throw "BUILD_FAILED"; }
    write_file(out, read_file(join_path(WORK, "built")) + ": installed");
}
fn cleanup(reason) { print("CLEANUP-REASON " + reason); }
"#;
    write_recipe(&recipes, "check", source);
    let output = managed_command(&recipes, "install").output().unwrap();
    assert_success(&output);
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        std::fs::read_to_string(result["outputs"]["product"].as_str().unwrap()).unwrap(),
        "prepared source: built: installed"
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("CLEANUP-REASON auto.build.success"));
    let failed = managed_command(&recipes, "install")
        .args(["--define", "FAIL=yes"])
        .output()
        .unwrap();
    assert!(!failed.status.success());
    let diagnostics = String::from_utf8_lossy(&failed.stderr);
    assert!(diagnostics.contains("BUILD_FAILED"), "{diagnostics}");
    assert!(
        diagnostics.contains("CLEANUP-REASON auto.build.failure"),
        "{diagnostics}"
    );
    assert_eq!(
        std::fs::read_to_string(recipes.join("check.rhai")).unwrap(),
        source
    );
}

#[test]
fn managed_helpers_use_the_assigned_null_device() {
    let (_dir, recipes) = create_test_env();
    let source = r#"
const RECIPE_API = 1;
const NEEDS = #{};
fn build(src, out) {
    write_file("/dev/null", "discard these actual helper bytes");
    if read_file("/dev/null") != "" { throw "the null device kept bytes"; }
    if is_file("/dev/null") { throw "the null device is a regular file"; }
    if !is_dir("/dev") { throw "the device directory is missing"; }
}
"#;
    write_recipe(&recipes, "check", source);
    let output = managed_command(&recipes, "install").output().unwrap();
    assert_success(&output);
    assert!(String::from_utf8_lossy(&output.stderr).contains("launcher exit 0; result exited"));
    assert_eq!(
        std::fs::read_to_string(recipes.join("check.rhai")).unwrap(),
        source
    );
}

#[test]
fn managed_bash_child_writes_domain_files_and_keeps_logs_out_of_json() {
    let (_dir, recipes) = create_test_env();
    let source = r#"
const RECIPE_API = 1;
const NEEDS = #{ outputs: #{ proof: "file" } };
fn build(src, out) {
    print("rhai-print-proof"); debug("rhai-debug-proof"); warn("rhai-warn-proof");
    let large = "x";
    for index in 0..15 { large += large; }
    if large.len() != 32768 { throw "large value changed"; }
    let from_child = join_path(WORK, "from-child");
    let code = exec("/usr/bin/bash", ["--noprofile", "--norc", "-c",
        "printf actual-bash-product > " + from_child + "; printf 'child-stdout\\000\\377'; printf child-stderr >&2; exit 0"]);
    if code != 0 { throw "child failed"; }
    write_file(out, read_file(from_child));
}
"#;
    write_recipe(&recipes, "check", source);
    let output = managed_command(&recipes, "install").output().unwrap();
    assert_success(&output);
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(output.stderr.windows(2).any(|window| window == b"\0\xff"));
    assert_eq!(
        std::fs::read(result["outputs"]["proof"].as_str().unwrap()).unwrap(),
        b"actual-bash-product"
    );
    assert!(!String::from_utf8_lossy(&output.stdout).contains("child-stdout"));
    assert_eq!(
        std::fs::read_to_string(recipes.join("check.rhai")).unwrap(),
        source
    );
    let diagnostics = String::from_utf8_lossy(&output.stderr);
    assert!(diagnostics.contains("child-stdout"), "{diagnostics}");
    assert!(diagnostics.contains("child-stderr"), "{diagnostics}");
    for message in ["rhai-print-proof", "rhai-debug-proof", "rhai-warn-proof"] {
        assert!(diagnostics.contains(message), "{diagnostics}");
    }
    assert!(
        !diagnostics.contains("ginux: audit"),
        "supervisor audit leaked into user diagnostics"
    );
    assert!(
        diagnostics.contains("launcher exit 0; result exited"),
        "{diagnostics}"
    );
    assert!(!diagnostics.contains("panicked at"), "{diagnostics}");
    assert!(
        !diagnostics.contains("error retrieving current directory"),
        "{diagnostics}"
    );
}

#[test]
fn managed_diagnostic_limit_drains_child_and_preserves_result() {
    use levitate_recipe::bin_support::MAX_DIAGNOSTIC_BYTES;
    let (_dir, recipes) = create_test_env();
    let source = r#"
const RECIPE_API = 1;
const NEEDS = #{ outputs: #{ report: "file" } };
fn build(src, out) {
    print("before-diagnostic-cap");
    let code = exec("/usr/bin/bash", ["--noprofile", "--norc", "-c", "printf '%*s' DIAGNOSTIC_WIDTH x; exit 0"]);
    print("after-diagnostic-cap");
    write_file(out, "child code " + code);
}
"#
    .replace("DIAGNOSTIC_WIDTH", &(MAX_DIAGNOSTIC_BYTES + 8192).to_string());
    write_recipe(&recipes, "check", &source);
    let output = managed_command(&recipes, "install").output().unwrap();
    assert_success(&output);
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        std::fs::read_to_string(result["outputs"]["report"].as_str().unwrap()).unwrap(),
        "child code 0"
    );
    assert!(output.stderr.len() <= MAX_DIAGNOSTIC_BYTES + 4096);
    let diagnostics = String::from_utf8_lossy(&output.stderr);
    assert!(diagnostics.contains("before-diagnostic-cap"));
    assert!(!diagnostics.contains("after-diagnostic-cap"));
    assert_eq!(diagnostics.matches("diagnostic limit reached").count(), 1);
    assert!(diagnostics.contains("launcher exit 0; result exited"));
    assert_eq!(
        std::fs::read_to_string(recipes.join("check.rhai")).unwrap(),
        source
    );
}

#[test]
fn managed_diagnostic_record_limit_preserves_the_result() {
    use levitate_recipe::bin_support::MAX_DIAGNOSTIC_RECORDS;
    let (_dir, recipes) = create_test_env();
    let source = r#"
const RECIPE_API = 1;
const NEEDS = #{ outputs: #{ report: "file" } };
fn build(src, out) {
    print("before-record-cap");
    for index in 0..DIAGNOSTIC_COUNT { print("record-data"); }
    write_file(out, "logged");
    print("after-record-cap");
}
"#
    .replace(
        "DIAGNOSTIC_COUNT",
        &(MAX_DIAGNOSTIC_RECORDS + 1024).to_string(),
    );
    write_recipe(&recipes, "check", &source);
    let output = managed_command(&recipes, "install").output().unwrap();
    assert_success(&output);
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        std::fs::read_to_string(result["outputs"]["report"].as_str().unwrap()).unwrap(),
        "logged"
    );
    let diagnostics = String::from_utf8_lossy(&output.stderr);
    assert!(diagnostics.contains("before-record-cap"));
    assert!(!diagnostics.contains("after-record-cap"));
    assert_eq!(diagnostics.matches("diagnostic limit reached").count(), 1);
    assert!(diagnostics.contains("launcher exit 0; result exited"));
    assert_eq!(
        std::fs::read_to_string(recipes.join("check.rhai")).unwrap(),
        source
    );
}

#[test]
fn managed_capture_returns_stdout_and_relays_stderr_beyond_pipe_capacity() {
    let (_dir, recipes) = create_test_env();
    let source = r#"
const RECIPE_API = 1;
const NEEDS = #{};
fn build(src, out) {
    let short = exec_output("/usr/bin/bash", ["--noprofile", "--norc", "-c", "printf ' capture-value\\n'"]);
    if short != " capture-value\n" { throw "short capture changed: " + short; }
    let large = exec_output("/usr/bin/bash", ["--noprofile", "--norc", "-c",
        "printf '%080000d' 0; printf capture-stdout-end; printf '%080000d' 0 >&2; printf capture-stderr-end >&2"]);
    if large.len() != 80018 || !large.ends_with("capture-stdout-end") { throw "large capture changed"; }
    let empty = exec_output("/usr/bin/bash", ["--noprofile", "--norc", "-c", "printf only-stderr >&2"]);
    if empty != "" { throw "stderr leaked into the capture"; }
}
"#;
    write_recipe(&recipes, "check", source);
    let output = managed_command(&recipes, "install").output().unwrap();
    assert_success(&output);
    let diagnostics = String::from_utf8_lossy(&output.stderr);
    assert!(
        diagnostics.contains("capture-stderr-end") && diagnostics.contains("only-stderr"),
        "{diagnostics}"
    );
    assert!(
        !diagnostics.contains("capture-stdout-end"),
        "captured stdout leaked to diagnostics"
    );
    assert!(
        !diagnostics.contains("ginux: audit") && !diagnostics.contains("panicked at"),
        "{diagnostics}"
    );
    assert!(
        diagnostics.contains("launcher exit 0; result exited"),
        "{diagnostics}"
    );
    assert_eq!(
        std::fs::read_to_string(recipes.join("check.rhai")).unwrap(),
        source
    );
}

#[test]
fn managed_capture_nonzero_exit_remains_a_fatal_build_error() {
    let (_dir, recipes) = create_test_env();
    let source = r#"
const RECIPE_API = 1;
const NEEDS = #{};
fn build(src, out) {
    exec_output("/usr/bin/bash", ["--noprofile", "--norc", "-c",
        "printf capture-failed-stdout; printf capture-failed-stderr >&2; exit 23"]);
}
"#;
    write_recipe(&recipes, "check", source);
    let output = managed_command(&recipes, "install").output().unwrap();
    assert!(!output.status.success());
    assert!(
        output.stdout.is_empty(),
        "failure emitted a successful result"
    );
    let diagnostics = String::from_utf8_lossy(&output.stderr);
    for message in [
        "exit status: 23",
        "capture-failed-stdout",
        "capture-failed-stderr",
    ] {
        assert!(diagnostics.contains(message), "{diagnostics}");
    }
    assert!(
        !diagnostics.contains("panicked at") && !diagnostics.contains("backend protocol failure"),
        "{diagnostics}"
    );
    assert_eq!(
        std::fs::read_to_string(recipes.join("check.rhai")).unwrap(),
        source
    );
}

#[test]
fn managed_large_file_output_is_checked_by_verify() {
    let (_dir, recipes) = create_test_env();
    let source = r#"
const RECIPE_API = 1;
const NEEDS = #{ outputs: #{ proof: "file" } };
fn expected_content() {
    let value = "x";
    for index in 0..17 { value += value; }
    value
}
fn build(src, out) { write_file(out, expected_content()); }
fn verify(out) {
    if read_file(out) != expected_content() { throw "large installed bytes are missing or invalid"; }
}
"#;
    write_recipe(&recipes, "check", source);
    let output = managed_command(&recipes, "install").output().unwrap();
    assert_success(&output);
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        std::fs::read(result["outputs"]["proof"].as_str().unwrap()).unwrap(),
        vec![b'x'; 131072]
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("launcher exit 0; result exited"));
    assert_eq!(
        std::fs::read_to_string(recipes.join("check.rhai")).unwrap(),
        source
    );
}

#[test]
fn managed_directory_output_copies_moves_and_publishes_nested_products() {
    let (_dir, recipes) = create_test_env();
    let source = r#"
const RECIPE_API = 1;
const NEEDS = #{ outputs: #{ tree: "directory" } };
fn expected_content() {
    let value = "d";
    for index in 0..17 { value += value; }
    value
}
fn build(src, out) {
    let material = join_path(WORK, "source");
    mkdir(join_path(material, "nested"));
    write_file(join_path(material, "nested/input"), expected_content());
    write_file(join_path(material, "manifest"), "directory-product-v1");
    mkdir(join_path(out, "staged"));
    copy_tree_contents(material, join_path(out, "staged"));
    mv(join_path(out, "staged"), join_path(out, "final"));
}
fn verify(out) {
    if is_dir(join_path(out, "staged")) { throw "the staged folder was not moved"; }
    if read_file(join_path(out, "final/manifest")) != "directory-product-v1" { throw "manifest changed"; }
    if read_file(join_path(out, "final/nested/input")) != expected_content() { throw "nested bytes changed"; }
}
"#;
    write_recipe(&recipes, "check", source);
    let output = managed_command(&recipes, "install").output().unwrap();
    assert_success(&output);
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let published = Path::new(result["outputs"]["tree"].as_str().unwrap());
    assert_eq!(
        std::fs::read(published.join("final/manifest")).unwrap(),
        b"directory-product-v1"
    );
    assert_eq!(
        std::fs::read(published.join("final/nested/input")).unwrap(),
        vec![b'd'; 131072]
    );
    assert!(!published.join("staged").exists());
    let diagnostics = String::from_utf8_lossy(&output.stderr);
    assert!(
        diagnostics.contains("launcher exit 0; result exited"),
        "{diagnostics}"
    );
    assert!(
        !diagnostics.contains("panicked at") && !diagnostics.contains("ginux: audit"),
        "{diagnostics}"
    );
    assert_eq!(
        std::fs::read_to_string(recipes.join("check.rhai")).unwrap(),
        source
    );
}

#[test]
fn managed_rename_cannot_replace_staged_recipe_source() {
    let (_dir, recipes) = create_test_env();
    let source = r#"
const RECIPE_API = 1;
const NEEDS = #{};
fn build(src, out) {
    write_file(join_path(WORK, "replacement"), "replace staged recipe");
    mv(join_path(WORK, "replacement"), "/inputs/main.rhai");
}
"#;
    write_recipe(&recipes, "check", source);
    let output = managed_command(&recipes, "install").output().unwrap();
    assert!(!output.status.success() && output.stdout.is_empty());
    let diagnostics = String::from_utf8_lossy(&output.stderr);
    assert!(diagnostics.contains("os error 18"), "{diagnostics}");
    assert!(
        !diagnostics.contains("panicked at") && !diagnostics.contains("backend protocol failure"),
        "{diagnostics}"
    );
    assert_eq!(
        std::fs::read_to_string(recipes.join("check.rhai")).unwrap(),
        source
    );
}

#[test]
fn managed_owned_elf_build_executes_and_validates_its_product() {
    let (_dir, recipes) = create_test_env();
    let source = r#"
const RECIPE_API = 1;
const NEEDS = #{ outputs: #{ proof: "file" } };
fn build(src, out) {
    let generated = join_path(WORK, "generated-bash");
    let result = join_path(WORK, "owned-result");
    copy_file("/usr/bin/bash", generated);
    let code = exec(generated, ["--noprofile", "--norc", "-c",
        "printf owned-elf-product > " + result + "; printf owned-elf-stdout; printf owned-elf-stderr >&2"]);
    if code != 0 { throw "generated executable failed"; }
    let captured = exec_output(generated, ["--noprofile", "--norc", "-c", "printf owned-elf-capture"]);
    if captured != "owned-elf-capture" { throw "capture changed: " + captured; }
    copy_file(result, out);
}
"#;
    write_recipe(&recipes, "check", source);
    let output = managed_command(&recipes, "install").output().unwrap();
    assert_success(&output);
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        std::fs::read(result["outputs"]["proof"].as_str().unwrap()).unwrap(),
        b"owned-elf-product"
    );
    let diagnostics = String::from_utf8_lossy(&output.stderr);
    for message in [
        "owned-elf-stdout",
        "owned-elf-stderr",
        "launcher exit 0; result exited",
    ] {
        assert!(diagnostics.contains(message), "{diagnostics}");
    }
    assert!(
        !diagnostics.contains("panicked at") && !diagnostics.contains("ginux: audit"),
        "{diagnostics}"
    );
    assert_eq!(
        std::fs::read_to_string(recipes.join("check.rhai")).unwrap(),
        source
    );
}

#[test]
fn managed_owned_exec_requires_executable_permissions() {
    let (_dir, recipes) = create_test_env();
    let source = r#"
const RECIPE_API = 1;
const NEEDS = #{};
fn build(src, out) {
    let data = join_path(WORK, "not-executable");
    write_file(data, "ordinary data");
    exec_checked(data, []);
}
"#;
    write_recipe(&recipes, "check", source);
    let output = managed_command(&recipes, "install").output().unwrap();
    assert!(!output.status.success() && output.stdout.is_empty());
    let diagnostics = String::from_utf8_lossy(&output.stderr);
    assert!(diagnostics.contains("os error 13"), "{diagnostics}");
    assert!(
        !diagnostics.contains("backend protocol failure") && !diagnostics.contains("panicked at"),
        "{diagnostics}"
    );
    assert_eq!(
        std::fs::read_to_string(recipes.join("check.rhai")).unwrap(),
        source
    );
}

#[test]
fn managed_generated_shebang_build_preserves_path_arguments_and_products() {
    let (_dir, recipes) = create_test_env();
    let source = r##"
const RECIPE_API = 1;
const NEEDS = #{ outputs: #{ proof: "file" } };
fn build(src, out) {
    let script = join_path(WORK, "generated-script");
    let result = join_path(WORK, "script-result");
    write_file(script, "#!/usr/bin/bash --noprofile\nprintf script-product > " + result + "\nprintf '%s|%s|%s' \"$0\" \"$1\" \"$2\"\nprintf script-stderr >&2\n");
    chmod(script, 0o755);
    let captured = exec_output(script, ["argument with spaces", "last"]);
    if captured != script + "|argument with spaces|last" { throw "script arguments changed: " + captured; }
    copy_file("/usr/bin/true", join_path(WORK, "generated-true"));
    if exec(join_path(WORK, "generated-true"), []) != 0 { throw "generated true failed"; }
    copy_file(result, out);
}
"##;
    write_recipe(&recipes, "check", source);
    let output = managed_command(&recipes, "install").output().unwrap();
    assert_success(&output);
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        std::fs::read(result["outputs"]["proof"].as_str().unwrap()).unwrap(),
        b"script-product"
    );
    let diagnostics = String::from_utf8_lossy(&output.stderr);
    assert!(
        diagnostics.contains("script-stderr")
            && diagnostics.contains("launcher exit 0; result exited"),
        "{diagnostics}"
    );
    assert!(
        !diagnostics.contains("panicked at") && !diagnostics.contains("ginux: audit"),
        "{diagnostics}"
    );
    assert_eq!(
        std::fs::read_to_string(recipes.join("check.rhai")).unwrap(),
        source
    );
}

#[test]
fn managed_shell_reopens_an_unlinked_script_after_a_libc_fork() {
    let (_dir, recipes) = create_test_env();
    let source = r##"
const RECIPE_API = 1;
const NEEDS = #{};
fn build(src, out) {
    let script = join_path(WORK, "fd-script");
    let result = join_path(WORK, "fd-result");
    write_file(script, "printf '%s|%s' \"$0\" \"$1\"\nprintf fd-product > " + result + "\n");
    let captured = exec_output("/usr/bin/bash", ["--noprofile", "--norc", "-c",
        "exec 8<" + script + "; /usr/bin/rm " + script + "; source /dev/fd/8 'spaced argument'", "fd-shell"]);
    if is_file(script) { throw "the script was not unlinked"; }
    if captured != "fd-shell|spaced argument" { throw "descriptor script changed: " + captured; }
    if read_file(result) != "fd-product" { throw "descriptor script did not produce the expected bytes"; }
}
"##;
    write_recipe(&recipes, "check", source);
    let output = managed_command(&recipes, "install").output().unwrap();
    assert_success(&output);
    let diagnostics = String::from_utf8_lossy(&output.stderr);
    assert!(
        diagnostics.contains("launcher exit 0; result exited"),
        "{diagnostics}"
    );
    assert!(
        !diagnostics.contains("panicked at") && !diagnostics.contains("ginux: audit"),
        "{diagnostics}"
    );
    assert_eq!(
        std::fs::read_to_string(recipes.join("check.rhai")).unwrap(),
        source
    );
}

#[test]
fn cli_partial_launcher_line_times_out_and_reaps_pipe_holders() {
    use std::os::unix::fs::PermissionsExt;
    let (dir, recipes) = create_test_env();
    let sentinel = dir.path().join("host-effect");
    guarded_recipe(&recipes, &sentinel);
    let archive = dir.path().join("image.tar");
    let digest = image_tar(&archive, &[("etc/fact", tar::EntryType::Regular, "image")]);
    let launcher = dir.path().join("partial-launcher");
    std::fs::write(
        &launcher,
        r#"#!/usr/bin/python3
import os, sys, time
if sys.argv[1].startswith('instance-'):
    os.execv(os.environ['RECIPE_TEST_REAL_GINUX'], [os.environ['RECIPE_TEST_REAL_GINUX'], *sys.argv[1:]])
with open(os.environ['RECIPE_TEST_PARTIAL_LINE_START'], 'w') as marker:
    marker.write(str(time.monotonic_ns()))
os.write(1, b'{"version":')
child = os.fork()
if child == 0:
    time.sleep(60)
else:
    time.sleep(60)
"#,
    )
    .unwrap();
    std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755)).unwrap();
    let line_start = dir.path().join("partial-line-start");
    let output = image_command(&recipes, &archive, &digest)
        .env("RECIPE_TEST_PARTIAL_LINE_START", &line_start)
        .env(
            "RECIPE_TEST_REAL_GINUX",
            std::env::var_os("GINUX_BIN").unwrap(),
        )
        .env("GINUX_BIN", &launcher)
        .env("RECIPE_OUTPUT_TIMEOUT_SECS", "1")
        .output()
        .unwrap();
    assert!(!output.status.success() && output.stdout.is_empty());
    // Time from the actual partial line, excluding input/image preparation.
    // Both launcher and child hold the pipe, so waiting for either 60s sleep
    // instead of reaping it would still violate the unchanged 20s bound.
    let line_start: u64 = std::fs::read_to_string(line_start)
        .unwrap()
        .parse()
        .unwrap();
    let mut completed = std::mem::MaybeUninit::<libc::timespec>::uninit();
    assert_eq!(
        unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, completed.as_mut_ptr()) },
        0
    );
    let completed = unsafe { completed.assume_init() };
    let completed = completed.tv_sec as u64 * 1_000_000_000 + completed.tv_nsec as u64;
    let elapsed = std::time::Duration::from_nanos(completed - line_start);
    assert!(
        elapsed < std::time::Duration::from_secs(20),
        "partial-line teardown took {elapsed:?}"
    );
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(
        error.contains("complete worker output line deadline expired"),
        "{error}"
    );
    assert!(!sentinel.exists());
}

#[test]
fn managed_coordinator_death_terminates_launcher_and_worker() {
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;
    let (dir, recipes) = create_test_env();
    write_recipe(
        &recipes,
        "check",
        r#"
const RECIPE_API = 1;
const NEEDS = #{};
fn build(src, out) {
    print("managed-cancellation-ready");
    while true { exec_checked("/usr/bin/sleep", ["1"]); }
}
"#,
    );
    let mut child = managed_command(&recipes, "install")
        .env("TMPDIR", dir.path())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let pid = child.id();
    let stderr = child.stderr.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut tail = std::collections::VecDeque::new();
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if line.contains("managed-cancellation-ready") {
                let _ = tx.send(());
            }
            if tail.len() == 32 {
                tail.pop_front();
            }
            tail.push_back(line);
        }
        tail.into_iter().collect::<Vec<_>>().join("\n")
    });
    // Image preparation precedes the worker marker. This is a bootstrap
    // allowance, separate from the unchanged five-second death/reaping bound.
    // Hold through concurrent preparation with supported short sleeps; Ginux
    // bounds each individual sleep request to 300 seconds.
    if let Err(error) = rx.recv_timeout(std::time::Duration::from_secs(300)) {
        child.kill().ok();
        child.wait().ok();
        let diagnostics = reader.join().unwrap();
        panic!("worker did not reach cancellation point: {error}\n{diagnostics}");
    }
    fn children(pid: u32) -> Vec<u32> {
        std::fs::read_to_string(format!("/proc/{pid}/task/{pid}/children"))
            .unwrap_or_default()
            .split_whitespace()
            .filter_map(|p| p.parse().ok())
            .collect()
    }
    let launchers = children(pid);
    assert_eq!(launchers.len(), 1);
    let workers = children(launchers[0]);
    assert!(!workers.is_empty());
    let abandoned = scratch_directories(dir.path());
    assert_eq!(
        abandoned.len(),
        3,
        "coordinator must lease input, root and run directories"
    );
    let pool = abandoned[0].parent().unwrap();
    let sentinel = pool.join("unrelated");
    std::fs::write(&sentinel, "preserve").unwrap();
    write_recipe(
        &recipes,
        "check",
        "const RECIPE_API = 1;\nconst NEEDS = #{};\nfn build(src, out) {}\n",
    );
    let output = managed_command(&recipes, "install")
        .env("TMPDIR", dir.path())
        .output()
        .unwrap();
    assert_success(&output);
    assert!(
        abandoned.iter().all(|path| path.exists()),
        "concurrent invocation removed active scratch"
    );
    assert!(
        child.try_wait().unwrap().is_none(),
        "coordinator exited before the kill control"
    );
    unsafe {
        libc::kill(pid as i32, libc::SIGKILL);
    }
    assert!(!child.wait().unwrap().success());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let tracked: Vec<_> = launchers.into_iter().chain(workers).collect();
    loop {
        let live = tracked.iter().any(|pid| {
            std::fs::read_to_string(format!("/proc/{pid}/stat"))
                .ok()
                .is_some_and(|stat| {
                    !stat
                        .rsplit_once(") ")
                        .is_some_and(|(_, rest)| rest.starts_with('Z'))
                })
        });
        if !live {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "workload survived coordinator death"
        );
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    reader.join().unwrap();
    let output = managed_command(&recipes, "install")
        .env("TMPDIR", dir.path())
        .output()
        .unwrap();
    assert_success(&output);
    assert!(
        scratch_directories(dir.path()).is_empty(),
        "next invocation did not reclaim abandoned scratch"
    );
    assert_eq!(std::fs::read_to_string(sentinel).unwrap(), "preserve");
}

#[test]
fn managed_publication_selects_products_and_rejects_missing_outputs() {
    let (dir, recipes) = create_test_env();
    let source = r#"
const RECIPE_API = 1;
const NEEDS = #{ outputs: #{ file: "file", tree: "directory" } };
fn build(src, out) {
    let tree = out.tree;
    mkdir(join_path(tree, "empty"));
    write_file(join_path(tree, "product"), "published-bytes");
    let long_name = "";
    for index in 0..120 { long_name += "a"; }
    write_file(join_path(tree, long_name), "long-name-product");
    ln("product", join_path(tree, "link"));
    ln("/usr/bin/program", join_path(tree, "absolute"));
    ln(long_name, join_path(tree, "long-link"));
    write_file(join_path(WORK, "private"), "not selected");
    write_file(out.file, "published-bytes");
}
"#;
    write_recipe(&recipes, "check", source);
    let output = managed_command(&recipes, "install").output().unwrap();
    assert_success(&output);
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let tree = Path::new(result["outputs"]["tree"].as_str().unwrap());
    let published = tree.parent().unwrap();
    assert!(tree.join("empty").is_dir());
    assert_eq!(
        std::fs::read_link(tree.join("link")).unwrap(),
        Path::new("product")
    );
    assert_eq!(
        std::fs::read(tree.join("link")).unwrap(),
        b"published-bytes"
    );
    assert_eq!(
        std::fs::read_link(tree.join("absolute")).unwrap(),
        Path::new("/usr/bin/program")
    );
    assert_eq!(
        std::fs::read_link(tree.join("long-link")).unwrap(),
        Path::new(&"a".repeat(120))
    );
    assert_eq!(
        std::fs::read(tree.join("a".repeat(120))).unwrap(),
        b"long-name-product"
    );
    for path in [tree.join("product"), published.join("file")] {
        assert_eq!(std::fs::read(path).unwrap(), b"published-bytes");
    }
    assert_eq!(std::fs::read_dir(published).unwrap().count(), 2);
    assert!(published.starts_with(dir.path().join("state/recipe/exports")));
    let before = std::fs::read_dir(published.parent().unwrap())
        .unwrap()
        .count();
    write_recipe(
        &recipes,
        "check",
        &source.replace("    write_file(out.file, \"published-bytes\");\n", ""),
    );
    let output = managed_command(&recipes, "install").output().unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("NEEDS.outputs.file is a file the recipe did not make")
    );
    assert_eq!(
        std::fs::read_dir(published.parent().unwrap())
            .unwrap()
            .count(),
        before
    );
    assert_eq!(
        std::fs::read(published.join("file")).unwrap(),
        b"published-bytes"
    );
}

#[test]
fn managed_publication_rejects_archive_traversal_and_link_redirects() {
    use std::os::unix::fs::PermissionsExt;
    let (dir, recipes) = create_test_env();
    let source = r#"
const RECIPE_API = 1;
const NEEDS = #{ outputs: #{ product: "file" } };
fn build(src, out) { write_file(out, "valid"); }
"#;
    write_recipe(&recipes, "check", source);
    let sentinel = dir.path().join("sentinel");
    std::fs::write(&sentinel, b"keep").unwrap();
    let launcher = std::env::var("GINUX_BIN").unwrap();
    let wrapper = dir.path().join("corrupt-export");
    for kind in [
        "traversal",
        "symlink",
        "hardlink",
        "longname",
        "longlink",
        "link-parent",
        "link-child",
    ] {
        // Run the real supervisor and worker before injecting malformed export
        // bytes at the coordinator boundary. This is not a protocol stub.
        std::fs::write(
            &wrapper,
            format!(
                r#"#!/usr/bin/python3
import io, subprocess, sys, tarfile
result = subprocess.run([{launcher:?}] + sys.argv[1:])
if result.returncode or sys.argv[1] != "launch": sys.exit(result.returncode)
path = sys.argv[sys.argv.index('--export-file')+1]
with tarfile.open(path, 'w') as archive:
    entry = tarfile.TarInfo('out/../../sentinel' if {kind:?} == 'traversal' else 'out/product')
    if {kind:?} in ('longname', 'longlink'):
        entry.type = tarfile.GNUTYPE_LONGNAME if {kind:?} == 'longname' else tarfile.GNUTYPE_LONGLINK
        entry.size = 1024
        archive.addfile(entry, io.BytesIO(b'x'*1024))
    elif {kind:?} in ('link-parent', 'link-child'):
        entry.type = tarfile.DIRTYPE
        archive.addfile(entry)
        link = tarfile.TarInfo('out/product/redirect')
        link.type = tarfile.SYMTYPE
        link.linkname = {outside:?}
        child = tarfile.TarInfo('out/product/redirect/sentinel')
        child.size = 7
        if {kind:?} == 'link-parent': archive.addfile(link)
        archive.addfile(child, io.BytesIO(b'changed'))
        if {kind:?} == 'link-child': archive.addfile(link)
    elif {kind:?} == 'traversal':
        entry.size = 7
        archive.addfile(entry, io.BytesIO(b'changed'))
    else:
        entry.type = tarfile.SYMTYPE if {kind:?} == 'symlink' else tarfile.LNKTYPE
        entry.linkname = {sentinel:?}
        archive.addfile(entry)
"#,
                sentinel = sentinel.to_str().unwrap(),
                outside = dir.path().to_str().unwrap()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700)).unwrap();
        let output = managed_command(&recipes, "install")
            .env("GINUX_BIN", &wrapper)
            .output()
            .unwrap();
        assert!(!output.status.success(), "accepted {kind}");
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            error.contains("cannot validate completed outputs"),
            "{error}"
        );
        assert_eq!(std::fs::read(&sentinel).unwrap(), b"keep");
        let exports = dir.path().join("state/recipe/exports");
        assert!(
            !exports.exists() || std::fs::read_dir(&exports).unwrap().next().is_none(),
            "a failed publication left outputs behind"
        );
    }
}

#[test]
fn managed_generated_scripts_use_owned_and_image_interpreter_chains() {
    let (_dir, recipes) = create_test_env();
    write_recipe(
        &recipes,
        "check",
        r##"
const RECIPE_API = 1;
const NEEDS = #{};
fn build(src, out) {
    copy_file("/usr/bin/bash", "/work/tools/bash"); chmod("/work/tools/bash", 0o755);
    let body = "\nprintf '%s|%s|%s|%s|%s' \"$0\" \"$1\" \"$2\" \"$3\" \"$4\"\n";
    write_file("/work/tools/middle", "#!/work/tools/bash --noprofile" + body);
    chmod("/work/tools/middle", 0o755);
    write_file("/work/tools/outer", "#!/work/tools/middle outer option\n");
    chmod("/work/tools/outer", 0o755);
    let expected = "/work/tools/middle|outer option|/work/tools/outer|spaced argument|last";
    let owned = exec_output("/work/tools/outer", ["spaced argument", "last"]);
    if owned != expected { throw "owned interpreter chain changed: " + owned; }
    // A completed interpreter releases script pins so a later launch can use
    // changed contents while the original executable input stays immutable.
    write_file("/work/tools/middle", "#!/usr/bin/bash --noprofile" + body);
    let image = exec_output("/work/tools/outer", ["spaced argument", "last"]);
    if image != expected { throw "image interpreter chain changed: " + image; }
}
"##,
    );
    let output = managed_command(&recipes, "install").output().unwrap();
    assert_success(&output);
}
