//! Real inputs, publication and coordinator-boundary cases on the public Ginux
//! lifecycle. Preconditions: see the header of `tests/e2e.rs` (a real Ginux,
//! privileges, the imported compiler image). The image cases below also create
//! small throwaway state roots under `/var/lib/recipe-e2e/` and import layouts
//! the test wrote there, so they need root (the guard allows no `import-image`
//! and only allowlisted images, and its sudoers rule pins other roots); a case that
//! forges a console line (the partial-line deadline) needs root to append to
//! the instance's root-owned `console.log`.

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

/// A state root of its own for one test, so the image it names is imported
/// fresh and cannot be found in a cache (the shared root caches the real
/// compiler image).
fn private_state(dir: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let name: String = dir
        .file_name()
        .unwrap()
        .to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    (
        Path::new("/var/lib/recipe-e2e").join(&name),
        Path::new("/run/recipe-e2e").join(&name),
    )
}

/// `install check` against an image the test chose: `layout` is an OCI layout
/// directory, `manifest` the hex digest the reference names.
fn image_command(recipes: &Path, layout: &Path, manifest: &str) -> Command {
    let (state, run) = private_state(recipes.parent().unwrap());
    let mut command = recipe_command(recipes);
    command
        .env_remove("RECIPE_WORKER")
        .env_remove("RECIPE_INVOCATION")
        .env("TMPDIR", recipes.parent().unwrap())
        .env(
            "RECIPE_IMAGE",
            format!("local/recipe-compiler@sha256:{manifest}"),
        )
        .env("RECIPE_IMAGE_LAYOUT", layout)
        .env("RECIPE_GINUX_STATE_ROOT", state)
        .env("RECIPE_GINUX_RUN_ROOT", run)
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

/// Write an OCI image layout (the bytes `recipe-image` writes, interim
/// lifecycle design 2.5) whose one uncompressed layer holds `entries`; return
/// the manifest digest (hex). Names are written literally.
fn write_layout(layout: &Path, entries: &[(&str, tar::EntryType, &str)]) -> String {
    let blobs = layout.join("blobs/sha256");
    std::fs::create_dir_all(&blobs).unwrap();
    let mut layer = Vec::new();
    {
        let mut archive = tar::Builder::new(&mut layer);
        for (name, kind, value) in entries {
            let mut header = tar::Header::new_ustar();
            header.as_mut_bytes()[..name.len()].copy_from_slice(name.as_bytes());
            header.set_entry_type(*kind);
            header.set_mode(0o755);
            if kind.is_symlink() || kind.is_hard_link() {
                header.set_size(0);
                archive.append_link(&mut header, name, value).unwrap();
            } else {
                header.set_size(value.len() as u64);
                header.set_cksum();
                archive.append(&header, value.as_bytes()).unwrap();
            }
        }
        archive.finish().unwrap();
    }
    let put = |bytes: &[u8]| {
        let digest = hex::encode(Sha256::digest(bytes));
        std::fs::write(blobs.join(&digest), bytes).unwrap();
        digest
    };
    let layer_digest = put(layer.as_slice());
    let config = format!(
        r#"{{"architecture":"amd64","os":"linux","rootfs":{{"type":"layers","diff_ids":["sha256:{layer_digest}"]}},"config":{{}}}}"#
    );
    let config_digest = put(config.as_bytes());
    let manifest = format!(
        r#"{{"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json","config":{{"mediaType":"application/vnd.oci.image.config.v1+json","digest":"sha256:{config_digest}","size":{}}},"layers":[{{"mediaType":"application/vnd.oci.image.layer.v1.tar","digest":"sha256:{layer_digest}","size":{}}}]}}"#,
        config.len(),
        layer.len()
    );
    let manifest_digest = put(manifest.as_bytes());
    std::fs::write(
        layout.join("oci-layout"),
        r#"{"imageLayoutVersion":"1.0.0"}"#,
    )
    .unwrap();
    std::fs::write(
        layout.join("index.json"),
        format!(
            r#"{{"schemaVersion":2,"manifests":[{{"mediaType":"application/vnd.oci.image.manifest.v1+json","digest":"sha256:{manifest_digest}","size":{},"platform":{{"architecture":"amd64","os":"linux"}}}}]}}"#,
            manifest.len()
        ),
    )
    .unwrap();
    manifest_digest
}

fn mentions(text: &str, words: &[&str]) -> bool {
    let text = text.to_lowercase();
    words.iter().any(|word| text.contains(word))
}

#[test]
fn cli_missing_or_mismatched_root_image_fails_before_recipe_setup() {
    let (dir, recipes) = create_test_env();
    let sentinel = dir.path().join("host-effect");
    let source = guarded_recipe(&recipes, &sentinel);
    // No layout at all, and a reference that is not imported anywhere.
    let layout = dir.path().join("layout");
    let missing = image_command(&recipes, &layout, &"0".repeat(64))
        .output()
        .unwrap();
    assert!(!missing.status.success());
    let error = String::from_utf8_lossy(&missing.stderr);
    assert!(
        error.contains("RECIPE_IMAGE_LAYOUT") || error.contains(layout.to_str().unwrap()),
        "{error}"
    );
    // A layout that is not the one the reference names: the import prints its
    // own manifest digest and Recipe refuses the difference.
    write_layout(
        &layout,
        &[("etc/fact", tar::EntryType::Regular, "image fact")],
    );
    let mismatch = image_command(&recipes, &layout, &"0".repeat(64))
        .output()
        .unwrap();
    assert!(!mismatch.status.success());
    let error = String::from_utf8_lossy(&mismatch.stderr);
    assert!(mentions(&error, &["digest"]), "{error}");
    assert!(!sentinel.exists());
    assert!(leaked_instances_in(&private_state(dir.path()).0).is_empty());
    assert_eq!(
        std::fs::read_to_string(recipes.join("check.rhai")).unwrap(),
        source
    );
}

#[test]
fn cli_image_cannot_replace_invocation_paths_or_the_hosts_file() {
    // Recipe injects `inputs/`, `work/`, `out/` and `etc/hosts` itself and the
    // injector never overwrites: an image that already has one of those paths
    // fails the run at injection (exit 65, a conflict), before any recipe code.
    let (dir, recipes) = create_test_env();
    let effect = dir.path().join("host-effect");
    guarded_recipe(&recipes, &effect);
    for (n, entry) in [
        ("inputs/recipe-worker", "replacement"),
        ("work/build", "replacement"),
        ("etc/hosts", "127.0.0.1 image-owned"),
    ]
    .into_iter()
    .enumerate()
    {
        let layout = dir.path().join(format!("layout-{n}"));
        let manifest = write_layout(&layout, &[(entry.0, tar::EntryType::Regular, entry.1)]);
        let output = image_command(&recipes, &layout, &manifest)
            .output()
            .unwrap();
        assert!(!output.status.success());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            mentions(&error, &["inject", "conflict", "exists"]),
            "{error}"
        );
        assert!(!effect.exists());
        assert!(scratch_directories(dir.path()).is_empty());
        assert!(
            leaked_instances_in(&private_state(dir.path()).0).is_empty(),
            "a failed injection left an instance"
        );
    }
}

#[test]
fn cli_invalid_worker_cleans_up_the_created_instance() {
    let (dir, recipes) = create_test_env();
    let effect = dir.path().join("host-effect");
    guarded_recipe(&recipes, &effect);
    let worker = dir.path().join("bad-worker");
    std::fs::write(&worker, "this is not an ELF worker executable").unwrap();
    let output = managed_command(&recipes, "install")
        .env("TMPDIR", dir.path())
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
    assert!(leaked_instances().is_empty());
}

// These cases require a real Ginux and its imported compiler image (header of
// tests/e2e.rs). Missing prerequisites are failures, never skipped tests.
fn managed_command(recipes: &Path, operation: &str) -> Command {
    require_ginux();
    let mut command = recipe_command(recipes);
    command
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
    assert!(String::from_utf8_lossy(&output.stderr).contains(START_EXIT_OK));
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
    assert!(String::from_utf8_lossy(&output.stderr).contains(START_EXIT_OK));
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
        !leaks_runtime_text(&diagnostics),
        "supervisor audit leaked into user diagnostics"
    );
    assert!(diagnostics.contains(START_EXIT_OK), "{diagnostics}");
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
    assert!(diagnostics.contains(START_EXIT_OK));
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
    assert!(diagnostics.contains(START_EXIT_OK));
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
        !leaks_runtime_text(&diagnostics) && !diagnostics.contains("panicked at"),
        "{diagnostics}"
    );
    assert!(diagnostics.contains(START_EXIT_OK), "{diagnostics}");
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
    assert!(String::from_utf8_lossy(&output.stderr).contains(START_EXIT_OK));
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
    assert!(diagnostics.contains(START_EXIT_OK), "{diagnostics}");
    assert!(
        !diagnostics.contains("panicked at") && !leaks_runtime_text(&diagnostics),
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
    // The staged source is not the workload's to replace: refused by the
    // contract (EACCES/EPERM/EROFS) or, if the folders are separate mounts,
    // by the rename itself (EXDEV).
    assert!(
        ["os error 18", "os error 13", "os error 1)", "os error 30"]
            .iter()
            .any(|code| diagnostics.contains(code)),
        "{diagnostics}"
    );
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
    for message in ["owned-elf-stdout", "owned-elf-stderr", START_EXIT_OK] {
        assert!(diagnostics.contains(message), "{diagnostics}");
    }
    assert!(
        !diagnostics.contains("panicked at") && !leaks_runtime_text(&diagnostics),
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
        diagnostics.contains("script-stderr") && diagnostics.contains(START_EXIT_OK),
        "{diagnostics}"
    );
    assert!(
        !diagnostics.contains("panicked at") && !leaks_runtime_text(&diagnostics),
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
    assert!(diagnostics.contains(START_EXIT_OK), "{diagnostics}");
    assert!(
        !diagnostics.contains("panicked at") && !leaks_runtime_text(&diagnostics),
        "{diagnostics}"
    );
    assert_eq!(
        std::fs::read_to_string(recipes.join("check.rhai")).unwrap(),
        source
    );
}

#[test]
fn cli_partial_console_line_times_out_and_tears_down_the_instance() {
    use std::io::{BufRead, BufReader, Write};
    use std::process::Stdio;
    // The worker's line records reach Recipe over the instance's console file.
    // A line that never ends must hit the complete-line deadline, and the run
    // (CLI, helper, instance) must be torn down within the bound. Only root can
    // write into the root-owned console.log; the case forges the partial line
    // there after the worker has spoken, so the bytes are the real console's.
    assert_eq!(
        unsafe { libc::geteuid() },
        0,
        "this case appends to the root-owned console.log: run the suite as root"
    );
    let (dir, recipes) = create_test_env();
    write_recipe(
        &recipes,
        "check",
        r#"
const RECIPE_API = 1;
const NEEDS = #{};
fn build(src, out) {
    print("console-partial-ready");
    exec_checked("/usr/bin/sleep", ["60"]);
}
"#,
    );
    let mut child = managed_command(&recipes, "install")
        .env("TMPDIR", dir.path())
        .env("RECIPE_OUTPUT_TIMEOUT_SECS", "1")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let pid = child.id();
    let stderr = child.stderr.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut text = String::new();
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if line.contains("console-partial-ready") {
                let _ = tx.send(());
            }
            text.push_str(&line);
            text.push('\n');
        }
        text
    });
    if rx
        .recv_timeout(std::time::Duration::from_secs(600))
        .is_err()
    {
        child.kill().ok();
        child.wait().ok();
        panic!("worker never spoke: {}", reader.join().unwrap());
    }
    let instances = instances_of(pid);
    assert_eq!(instances.len(), 1, "{instances:?}");
    let console = ginux_state_root()
        .join("instances")
        .join(&instances[0])
        .join("console.log");
    let started = std::time::Instant::now();
    std::fs::OpenOptions::new()
        .append(true)
        .open(&console)
        .unwrap()
        .write_all(b"{\"version\":")
        .unwrap();
    let status = child.wait().unwrap();
    let elapsed = started.elapsed();
    let error = reader.join().unwrap();
    assert!(!status.success(), "{error}");
    assert!(
        elapsed < std::time::Duration::from_secs(30),
        "partial-line teardown took {elapsed:?}"
    );
    assert!(
        error.contains("complete worker output line deadline expired"),
        "{error}"
    );
    assert!(instances_of(pid).is_empty(), "the instance was not deleted");
}

#[test]
fn managed_coordinator_death_tears_down_the_instance_and_the_next_run_sweeps_it() {
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
    // Image import and expansion precede the worker marker. This is a
    // bootstrap allowance, separate from the teardown bound below.
    if let Err(error) = rx.recv_timeout(std::time::Duration::from_secs(600)) {
        child.kill().ok();
        child.wait().ok();
        let diagnostics = reader.join().unwrap();
        panic!("worker did not reach cancellation point: {error}\n{diagnostics}");
    }
    fn descendants(pid: u32, result: &mut Vec<u32>) {
        let tasks = std::fs::read_dir(format!("/proc/{pid}/task"))
            .into_iter()
            .flatten();
        for task in tasks.flatten() {
            let children =
                std::fs::read_to_string(task.path().join("children")).unwrap_or_default();
            for child in children.split_whitespace().filter_map(|p| p.parse().ok()) {
                if !result.contains(&child) {
                    result.push(child);
                    descendants(child, result);
                }
            }
        }
    }
    // The ginux CLI, sudo, the helper, the supervisor and the workload all
    // descend from the coordinator.
    let mut tracked = Vec::new();
    descendants(pid, &mut tracked);
    assert!(tracked.len() >= 2, "{tracked:?}");
    let instances = instances_of(pid);
    assert_eq!(instances.len(), 1, "one instance per running invocation");
    let abandoned = scratch_directories(dir.path());
    assert!(
        !abandoned.is_empty(),
        "the coordinator must lease its run directory"
    );
    let pool = abandoned[0].parent().unwrap();
    let sentinel = pool.join("unrelated");
    std::fs::write(&sentinel, "preserve").unwrap();
    write_recipe(
        &recipes,
        "check",
        "const RECIPE_API = 1;\nconst NEEDS = #{};\nfn build(src, out) {}\n",
    );
    // A concurrent invocation (and its sweep of leftovers) must neither remove
    // the active scratch nor delete the live run's instance.
    let output = managed_command(&recipes, "install")
        .env("TMPDIR", dir.path())
        .output()
        .unwrap();
    assert_success(&output);
    assert!(
        abandoned.iter().all(|path| path.exists()),
        "concurrent invocation removed active scratch"
    );
    assert_eq!(
        instances_of(pid),
        instances,
        "a concurrent invocation swept a live run's instance"
    );
    assert!(
        child.try_wait().unwrap().is_none(),
        "coordinator exited before the kill control"
    );
    unsafe {
        libc::kill(pid as i32, libc::SIGKILL);
    }
    assert!(!child.wait().unwrap().success());
    // The CLI dies with the coordinator (PDEATHSIG); the helper notices within
    // a fraction of a second and tears the instance down in at most about 7.5
    // seconds, killing the workload (the runtime's EXITKILL covers the rest).
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
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
    // The stopped instance stays until a later Recipe command sweeps it.
    let output = managed_command(&recipes, "install")
        .env("TMPDIR", dir.path())
        .output()
        .unwrap();
    assert_success(&output);
    assert!(
        scratch_directories(dir.path()).is_empty(),
        "next invocation did not reclaim abandoned scratch"
    );
    assert!(
        instances_of(pid).is_empty(),
        "next invocation did not sweep the dead run's instance"
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
    let (dir, recipes) = create_test_env();
    let source = r#"
const RECIPE_API = 1;
const NEEDS = #{ outputs: #{ product: "file" } };
fn build(src, out) { write_file(out, "valid"); }
"#;
    write_recipe(&recipes, "check", source);
    let sentinel = dir.path().join("sentinel");
    std::fs::write(&sentinel, b"keep").unwrap();
    for kind in [
        "traversal",
        "symlink",
        "hardlink",
        "longname",
        "longlink",
        "link-parent",
        "link-child",
    ] {
        // Run the real instance and worker, let the real `extract` write its
        // archive, then replace that archive with one that keeps the real
        // `work/.recipe-result.json` member (so the coordinator reads the
        // genuine result first) and adds the malformed entries after it. The
        // run then reaches the publication check. This is not a protocol stub.
        let wrapper = ginux_wrapper(
            dir.path(),
            "corrupt-export",
            &format!(
                r#"
import io, tarfile
if VERB != 'extract': passthrough()
code = real()
if code: sys.exit(code)
path = option('--output')
kind = {kind:?}
with tarfile.open(path) as genuine:
    result_info = genuine.getmember('work/.recipe-result.json')
    result_bytes = genuine.extractfile(result_info).read()
with tarfile.open(path, 'w') as archive:
    archive.addfile(result_info, io.BytesIO(result_bytes))
    entry = tarfile.TarInfo('out/../../sentinel' if kind == 'traversal' else 'out/product')
    if kind in ('longname', 'longlink'):
        entry.type = tarfile.GNUTYPE_LONGNAME if kind == 'longname' else tarfile.GNUTYPE_LONGLINK
        entry.size = 1024
        archive.addfile(entry, io.BytesIO(b'x'*1024))
        # A long-name header needs a member after it, or reading the export
        # fails before the publication check; the oversize name is rejected there.
        archive.addfile(tarfile.TarInfo('out/product'))
    elif kind in ('link-parent', 'link-child'):
        entry.type = tarfile.DIRTYPE
        archive.addfile(entry)
        link = tarfile.TarInfo('out/product/redirect')
        link.type = tarfile.SYMTYPE
        link.linkname = {outside:?}
        child = tarfile.TarInfo('out/product/redirect/sentinel')
        child.size = 7
        if kind == 'link-parent': archive.addfile(link)
        archive.addfile(child, io.BytesIO(b'changed'))
        if kind == 'link-child': archive.addfile(link)
    elif kind == 'traversal':
        entry.size = 7
        archive.addfile(entry, io.BytesIO(b'changed'))
    else:
        entry.type = tarfile.SYMTYPE if kind == 'symlink' else tarfile.LNKTYPE
        entry.linkname = {sentinel:?}
        archive.addfile(entry)
"#,
                sentinel = sentinel.to_str().unwrap(),
                outside = dir.path().to_str().unwrap()
            ),
        );
        let mut command = managed_command(&recipes, "install");
        with_wrapper(&mut command, &wrapper);
        let output = command.output().unwrap();
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
        assert!(leaked_instances().is_empty());
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
