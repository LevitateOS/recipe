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
        .env("GINUX_BIN", "/bin/false")
        .env("TMPDIR", recipes.parent().unwrap())
        .env("GINUX_ROOTFS", archive)
        .env("GINUX_ROOT", format!("oci:cli-image@sha256:{digest}"))
        .args([
            "isinstalled",
            "check",
            "--no-persist-state",
            "--recipes-path",
        ])
        .arg(recipes);
    command
}

fn guarded_recipe(recipes: &Path, sentinel: &Path) -> String {
    let source = format!(
        "write_file({:?}, \"host recipe code ran\");\nlet ctx = #{{name: \"image-check\"}};\nfn is_installed(ctx) {{ ctx }}\n",
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
            header.set_link_name(value).unwrap();
            header.set_size(0);
            header.set_cksum();
            archive.append(&header, std::io::empty()).unwrap();
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
    assert!(String::from_utf8_lossy(&missing.stderr).contains("cannot open Ginux rootfs image"));
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
const RECIPE_API = 2;
let ctx = #{name: "options", value: CUSTOM_INPUT};
fn is_installed(ctx) { ready(ctx) }
fn cleanup(ctx, reason) { ctx.reason = reason; ctx }
"#,
    );
    for operation in ["isinstalled", "cleanup"] {
        let mut command = managed_command(&recipes, operation);
        command.args(["--define", "CUSTOM_INPUT=caller value"]);
        if operation == "cleanup" {
            command.args(["--reason", "requested retry"]);
        }
        let output = command.output().unwrap();
        assert_success(&output);
        let context: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(context["value"], "caller value");
        if operation == "cleanup" {
            assert_eq!(context["reason"], "requested retry");
        }
    }
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
    std::fs::write(recipes.join("tools/helper.rhai"), r#"
const RECIPE_API = 2;
let ctx = #{name: "helper"};
fn is_acquired(ctx) { ready(ctx) }
fn is_installed(ctx) {
    if is_file(join_path(RUNTIME.paths.tools, "dependency-evidence")) { return ready(ctx); }
    needs_work(ctx, "helper missing")
}

fn install(ctx) { write_file(join_path(RUNTIME.paths.tools, "dependency-evidence"), "dependency ran"); ctx }
"#).unwrap();
    let source = r#"
const RECIPE_API = 2;
import "left/wrapper" as left;
import "right/facts.rhai" as right;
let deps = ["tools/helper"];
let ctx = #{name: "nested-inputs"};
fn is_acquired(ctx) { ready(ctx) }
fn is_installed(ctx) {
    if ctx.contains("complete") { return ready(ctx); }
    needs_work(ctx, "parent missing")
}
fn install(ctx) {
    ctx.modules = left::value() + ":" + right::value();
    ctx.dependency = read_file(join_path(RUNTIME.paths.tools, "dependency-evidence"));
    ctx.complete = true;
    ctx
}
"#;
    write_recipe(&recipes, "check", source);
    let output = managed_command(&recipes, "install").output().unwrap();
    assert_success(&output);
    let context: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(context["modules"], "left:right");
    assert_eq!(context["dependency"], "dependency ran");
    assert_eq!(
        std::fs::read_to_string(recipes.join("check.rhai")).unwrap(),
        source
    );
}

#[test]
fn managed_declared_source_tree_preserves_empty_directories_and_confined_links() {
    let (dir, recipes) = create_test_env();
    let source = dir.path().join("source");
    std::fs::create_dir_all(source.join("nested/long-component/long-component/long-component/long-component/long-component/long-component/long-component/")).unwrap();
    std::fs::write(source.join("nested/long-component/long-component/long-component/long-component/long-component/long-component/long-component/value"), "declared input").unwrap();
    std::fs::write(source.join("C++ résumé.txt"), "named bytes").unwrap();
    write_recipe(
        &recipes,
        "check",
        r#"
const RECIPE_API = 2;
let ctx = #{name: "source-input"};
fn is_installed(ctx) {
    if !is_dir("/inputs/source/empty") { throw "empty input directory missing"; }
    ctx.value = read_file("/inputs/source/chain");
    if read_file("/inputs/source/C++ résumé.txt") != "named bytes" { throw "source name changed"; }
    if read_file("/inputs/config.toml") != "named bytes" { throw "input collided with coordinator metadata"; }
    let denied = false;
    try { write_file("/inputs/source/nested/long-component/long-component/long-component/long-component/long-component/long-component/long-component/value", "changed"); } catch (error) { denied = true; }
    if !denied { throw "input accepted mutation"; }
    ready(ctx)
}
"#,
    );
    std::fs::create_dir(source.join("empty")).unwrap();
    std::os::unix::fs::symlink("nested/long-component/long-component/long-component/long-component/long-component/long-component/long-component/value", source.join("alias")).unwrap();
    std::os::unix::fs::symlink("alias", source.join("chain")).unwrap();
    let selection = format!("source={}", source.display());
    let output = managed_command(&recipes, "isinstalled")
        .args([
            "--input",
            &selection,
            "--input",
            &format!("config.toml={}", source.join("C++ résumé.txt").display()),
        ])
        .output()
        .unwrap();
    assert_success(&output);
    let context: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(context["value"], "declared input");
    assert_eq!(
        std::fs::read_to_string(source.join("nested/long-component/long-component/long-component/long-component/long-component/long-component/long-component/value")).unwrap(),
        "declared input"
    );
    std::fs::remove_file(source.join("alias")).unwrap();
    std::os::unix::fs::symlink(source.join("nested/long-component/long-component/long-component/long-component/long-component/long-component/long-component/value"), source.join("alias")).unwrap();
    let rejected = managed_command(&recipes, "isinstalled")
        .args(["--input", &selection])
        .output()
        .unwrap();
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("symlink"));
    std::fs::remove_file(source.join("alias")).unwrap();
    for target in ["../outside", "chain", "nested/up/../outside"] {
        // nested/up -> .. makes the last path escape after link expansion,
        // despite its apparently contained lexical spelling.
        let up = source.join("nested/up");
        if !std::fs::symlink_metadata(&up).is_ok() {
            std::os::unix::fs::symlink("..", &up).unwrap();
        }
        std::os::unix::fs::symlink(target, source.join("alias")).unwrap();
        let rejected = managed_command(&recipes, "isinstalled")
            .args(["--input", &selection])
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
        &format!(
            r#"
const RECIPE_API = 2;
let ctx = #{{name: "large-input"}};
fn is_installed(ctx) {{
    if trim(shell_output("wc -c < /inputs/asset.iso")) != "{size}" {{ throw "asset truncated"; }}
    if shell_output("tail -c 7 /inputs/asset.iso") != "payload" {{ throw "asset bytes changed"; }}
    ready(ctx)
}}
"#
        ),
    );
    let selection = format!("asset.iso={}", asset.display());
    let output = managed_command(&recipes, "isinstalled")
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
    let rejected = managed_command(&recipes, "isinstalled")
        .args(["--input", &selection])
        .output()
        .unwrap();
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("exceeding"));
}

#[test]
fn managed_custom_checks_run_independently_and_return_discovered_context() {
    use std::os::unix::fs::PermissionsExt;
    let (_dir, recipes) = create_test_env();
    let source = r#"
const RECIPE_API = 2;
let ctx = #{ name: "managed-check", expected: "x86_64" };
fn is_acquired(ctx) { ctx.observation = "custom source revision"; ready(ctx) }
fn is_built(ctx) { ctx.observation = "custom compiler configuration"; ready(ctx) }
fn is_installed(ctx) {
    if ctx.expected != RUNTIME.target.arch { return needs_work(ctx, "target differs"); }
    ctx.observation = "custom installation rule";
    ready(ctx)
}
"#;
    write_recipe(&recipes, "check", source);
    std::fs::set_permissions(
        recipes.join("check.rhai"),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    for (operation, observation) in [
        ("isacquired", "custom source revision"),
        ("isbuilt", "custom compiler configuration"),
        ("isinstalled", "custom installation rule"),
    ] {
        let output = managed_command(&recipes, operation).output().unwrap();
        assert_success(&output);
        let context: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(context["observation"], observation);
        assert_eq!(context["runtime"]["paths"]["inputs"], "/inputs");
        assert_eq!(context["runtime"]["paths"]["build"], "/work/build");
        assert_eq!(context["runtime"]["paths"]["tools"], "/work/tools");
        assert!(String::from_utf8_lossy(&output.stderr).contains("launcher exit 0; result exited"));
        assert_eq!(
            std::fs::read_to_string(recipes.join("check.rhai")).unwrap(),
            source
        );
    }
}

#[test]
fn managed_not_ready_and_real_script_errors_remain_failures() {
    let (_dir, recipes) = create_test_env();
    for (body, wanted) in [
        (
            "needs_work(ctx, \"configuration differs\")",
            "configuration differs",
        ),
        (
            "undefined_managed_function(ctx)",
            "undefined_managed_function",
        ),
    ] {
        let source = format!(
            "const RECIPE_API = 2; let ctx = #{{name: \"negative-check\"}}; fn is_installed(ctx) {{ {body} }}\n"
        );
        write_recipe(&recipes, "check", &source);
        let output = managed_command(&recipes, "isinstalled").output().unwrap();
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
    let output = managed_command(&recipes, "isinstalled").output().unwrap();
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
fn managed_check_reads_staged_base_and_imported_module() {
    let (_dir, recipes) = create_test_env();
    let module = "fn selected_fact() { \"pinned module fact\" }\n";
    let base = r#"
const RECIPE_API = 2;
import "facts.rhai" as facts;
let ctx = #{name: "base"};
fn inherited(ctx) { ctx.base_fact = facts::selected_fact(); ctx }
fn is_installed(ctx) { ready(inherited(ctx)) }
"#;
    let plain_child =
        "//! extends: base.rhai\nconst RECIPE_API = 2; let ctx = #{name: \"child\"};\n";
    let shared_child = r#"//! extends: base.rhai
const RECIPE_API = 2;
let ctx = #{name: "child"};
let captured_name = || ctx.name;
fn is_installed(ctx) {
    ctx = inherited(ctx);
    ctx.captured = call(captured_name);
    ready(ctx)
}
"#;
    write_recipe(&recipes, "facts", module);
    write_recipe(&recipes, "base", base);
    for child in [plain_child, shared_child] {
        write_recipe(&recipes, "check", child);
        let output = managed_command(&recipes, "isinstalled").output().unwrap();
        assert_success(&output);
        let context: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(context["name"], "child");
        assert_eq!(context["base_fact"], "pinned module fact");
        if child == shared_child {
            assert_eq!(context["captured"], "child");
        }
        for (name, source) in [("facts", module), ("base", base), ("check", child)] {
            assert_eq!(
                std::fs::read_to_string(recipes.join(format!("{name}.rhai"))).unwrap(),
                source
            );
        }
    }
}

#[test]
fn managed_cleanup_and_remove_return_context_after_checkpoint_ack() {
    let (_dir, recipes) = create_test_env();
    let source = r#"
const RECIPE_API = 2;
let ctx = #{name: "checkpoint-hooks"};
fn cleanup(ctx, reason) { ctx.state.reason = reason; ctx.state.operation = "cleanup"; ctx }
fn remove(ctx) { ctx.state.removed = true; ctx.state.build = ctx.runtime.paths.build; ctx.state.operation = "remove"; ctx }
"#;
    write_recipe(&recipes, "check", source);
    for operation in ["cleanup", "remove"] {
        let output = managed_command(&recipes, operation).output().unwrap();
        assert_success(&output);
        let context: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(context["state"]["operation"], operation);
        assert_eq!(context["runtime"]["paths"]["tools"], "/work/tools");
        assert!(String::from_utf8_lossy(&output.stderr).contains("1 checkpoint acked"));
        if operation == "cleanup" {
            assert_eq!(context["state"]["reason"], "manual");
        } else {
            assert_eq!(context["state"]["removed"], true);
            assert_eq!(context["state"]["build"], "/work/build");
        }
        assert_eq!(
            std::fs::read_to_string(recipes.join("check.rhai")).unwrap(),
            source
        );
    }
}

#[test]
fn managed_cleanup_without_a_hook_is_a_noop() {
    let (_dir, recipes) = create_test_env();
    let source = "const RECIPE_API = 2; let ctx = #{name: \"no-cleanup\"};\n";
    write_recipe(&recipes, "check", source);
    let output = managed_command(&recipes, "cleanup").output().unwrap();
    assert_success(&output);
    let context: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(context["name"], "no-cleanup");
    assert!(String::from_utf8_lossy(&output.stderr).contains("0 checkpoints acked"));
    assert_eq!(
        std::fs::read_to_string(recipes.join("check.rhai")).unwrap(),
        source
    );
}

#[test]
fn managed_lifecycle_transforms_files_and_cleanup_reads_actual_product() {
    let (_dir, recipes) = create_test_env();
    let source = r#"
const RECIPE_API = 2;
let ctx = #{name: "file-lifecycle", source: "/work/build/source", built: "/work/build/built", product: "/out/product", state: #{trace: ""}};
fn is_acquired(ctx) { if !is_file(ctx.source) { return needs_work(ctx, "source missing"); } ready(ctx) }
fn is_built(ctx) { if !is_file(ctx.built) { return needs_work(ctx, "build missing"); } ready(ctx) }
fn is_installed(ctx) { if !is_file(ctx.product) { return needs_work(ctx, "product missing"); } ready(ctx) }
fn acquire(ctx) { write_file(ctx.source, "prepared source"); ctx.state.trace += "acquire;"; ctx }
fn build(ctx) { write_file(ctx.built, read_file(ctx.source) + ": built"); ctx.state.trace += "build;"; ctx }
fn install(ctx) { write_file(ctx.product, read_file(ctx.built) + ": installed"); ctx.state.trace += "install;"; ctx }
fn cleanup(ctx, reason) {
    if reason == "auto.install.success" {
        ctx.state.product = read_file(ctx.product);
        rm(ctx.built);
        ctx.state.build_removed = !is_file(ctx.built);
        ctx.state.product_present = is_file(ctx.product);
    }
    ctx.state.trace += reason + ";";
    ctx
}
"#;
    write_recipe(&recipes, "check", source);
    let output = managed_command(&recipes, "install").output().unwrap();
    assert_success(&output);
    let context: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        context["state"]["product"],
        "prepared source: built: installed"
    );
    assert_eq!(context["state"]["build_removed"], true);
    assert_eq!(context["state"]["product_present"], true);
    assert_eq!(
        context["state"]["trace"],
        "acquire;auto.acquire.success;build;auto.build.success;install;auto.install.success;"
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("6 checkpoints acked"));
    assert_eq!(
        std::fs::read_to_string(recipes.join("check.rhai")).unwrap(),
        source
    );
}

#[test]
fn managed_cleanup_and_remove_mutate_domain_files() {
    let (_dir, recipes) = create_test_env();
    let source = r#"
const RECIPE_API = 2;
let ctx = #{name: "file-hooks", product: "/out/product"};
write_file(ctx.product, "actual domain artifact");
fn cleanup(ctx, reason) {
    ctx.state.before = read_file(ctx.product);
    rm(ctx.product);
    ctx.state.absent = !is_file(ctx.product);
    ctx.state.reason = reason;
    ctx
}
fn remove(ctx) { cleanup(ctx, "remove") }
"#;
    write_recipe(&recipes, "check", source);
    for operation in ["cleanup", "remove"] {
        let output = managed_command(&recipes, operation).output().unwrap();
        assert_success(&output);
        let context: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(context["state"]["before"], "actual domain artifact");
        assert_eq!(context["state"]["absent"], true);
        assert_eq!(
            context["state"]["reason"],
            if operation == "cleanup" {
                "manual"
            } else {
                "remove"
            }
        );
        assert!(String::from_utf8_lossy(&output.stderr).contains("1 checkpoint acked"));
        assert_eq!(
            std::fs::read_to_string(recipes.join("check.rhai")).unwrap(),
            source
        );
    }
}

#[test]
fn managed_helpers_use_the_assigned_null_device() {
    let (_dir, recipes) = create_test_env();
    let source = r#"
const RECIPE_API = 2;
let ctx = #{name: "null-device"};
fn is_installed(ctx) {
    write_file("/dev/null", "discard these actual helper bytes");
    ctx.state.null_read = read_file("/dev/null");
    ctx.state.regular_file = is_file("/dev/null");
    ctx.state.device_directory = is_dir("/dev");
    ready(ctx)
}
"#;
    write_recipe(&recipes, "check", source);
    let output = managed_command(&recipes, "isinstalled").output().unwrap();
    assert_success(&output);
    let context: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(context["state"]["null_read"], "");
    assert_eq!(context["state"]["regular_file"], false);
    assert_eq!(context["state"]["device_directory"], true);
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
const RECIPE_API = 2;
let ctx = #{name: "managed-child"};
fn is_acquired(ctx) { ready(ctx) }
fn is_built(ctx) {
    if ctx.state.contains("child_code") && ctx.state.child_code == 0 && is_file("/work/build/from-child") {
        if read_file("/work/build/from-child") == "actual-bash-product" { return ready(ctx); }
    }
    needs_work(ctx, "child has not produced the requested bytes")
}
fn is_installed(ctx) {
    if is_file("/out/child-proof") {
        if read_file("/out/child-proof") == "actual-bash-product" { return ready(ctx); }
    }
    needs_work(ctx, "child product is not installed")
}
fn build(ctx) {
    print("rhai-print-proof"); debug("rhai-debug-proof"); warn("rhai-warn-proof");
    let large = "x";
    for index in 0..15 { large += large; }
    ctx.state.large = large;
    ctx.state.child_code = exec("/usr/bin/bash", ["--noprofile", "--norc", "-c",
        "printf actual-bash-product > /work/build/from-child; printf 'child-stdout\\000\\377'; printf child-stderr >&2; exit 0"]);
    ctx
}
fn install(ctx) {
    write_file("/out/child-proof", read_file("/work/build/from-child"));
    ctx.outputs.proof = "/out/child-proof";
    ctx
}
"#;
    write_recipe(&recipes, "check", source);
    let output = managed_command(&recipes, "install").output().unwrap();
    assert_success(&output);
    let context: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(context["state"]["child_code"], 0);
    assert_eq!(
        context["state"]["large"].as_str().unwrap(),
        "x".repeat(32768)
    );
    assert!(output.stderr.windows(2).any(|window| window == b"\0\xff"));
    assert_eq!(
        std::fs::read(context["outputs"]["proof"].as_str().unwrap()).unwrap(),
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
    use levitate_recipe::worker::protocol::MAX_DIAGNOSTIC_BYTES;
    let (_dir, recipes) = create_test_env();
    let source = r#"
const RECIPE_API = 2;
let ctx = #{name: "diagnostic-cap", state: #{child_code: -1}};
fn is_acquired(ctx) { ready(ctx) }
fn is_built(ctx) { if ctx.state.child_code == 0 { ready(ctx) } else { needs_work(ctx, "run child") } }
fn is_installed(ctx) { if ctx.state.child_code == 0 { ready(ctx) } else { needs_work(ctx, "run child") } }
fn build(ctx) {
    print("before-diagnostic-cap");
    ctx.state.child_code = exec("/usr/bin/bash", ["--noprofile", "--norc", "-c", "printf '%*s' DIAGNOSTIC_WIDTH x; exit 0"]);
    print("after-diagnostic-cap");
    ctx
}
fn install(ctx) { ctx }
"#.replace("DIAGNOSTIC_WIDTH", &(MAX_DIAGNOSTIC_BYTES + 8192).to_string());
    write_recipe(&recipes, "check", &source);
    let output = managed_command(&recipes, "install").output().unwrap();
    assert_success(&output);
    let context: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(context["state"]["child_code"], 0);
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
fn managed_diagnostic_frame_limit_preserves_lifecycle_result() {
    use levitate_recipe::worker::protocol::MAX_DIAGNOSTIC_FRAMES;
    let (_dir, recipes) = create_test_env();
    let source = r#"
const RECIPE_API = 2;
let ctx = #{name: "diagnostic-frames", state: #{done: false}};
fn is_acquired(ctx) { ready(ctx) }
fn is_installed(ctx) { if ctx.state.done { ready(ctx) } else { needs_work(ctx, "run logging hook") } }
fn install(ctx) {
    print("before-frame-cap");
    for index in 0..DIAGNOSTIC_COUNT { print("frame-data"); }
    ctx.state.done = true;
    print("after-frame-cap");
    ctx
}
"#.replace("DIAGNOSTIC_COUNT", &(MAX_DIAGNOSTIC_FRAMES + 1024).to_string());
    write_recipe(&recipes, "check", &source);
    let output = managed_command(&recipes, "install").output().unwrap();
    assert_success(&output);
    let context: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(context["state"]["done"], true);
    let diagnostics = String::from_utf8_lossy(&output.stderr);
    assert!(diagnostics.contains("before-frame-cap"));
    assert!(!diagnostics.contains("after-frame-cap"));
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
const RECIPE_API = 2;
let ctx = #{name: "managed-capture"};
fn is_installed(ctx) {
    ctx.state.short = exec_output("/usr/bin/bash", ["--noprofile", "--norc", "-c", "printf ' capture-value\\n'"]);
    ctx.state.large = exec_output("/usr/bin/bash", ["--noprofile", "--norc", "-c",
        "printf '%080000d' 0; printf capture-stdout-end; printf '%080000d' 0 >&2; printf capture-stderr-end >&2"]);
    ctx.state.empty = exec_output("/usr/bin/bash", ["--noprofile", "--norc", "-c", "printf only-stderr >&2"]);
    ready(ctx)
}
"#;
    write_recipe(&recipes, "check", source);
    let output = managed_command(&recipes, "isinstalled").output().unwrap();
    assert_success(&output);
    let context: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(context["state"]["short"], " capture-value\n");
    assert_eq!(
        context["state"]["large"],
        "0".repeat(80000) + "capture-stdout-end"
    );
    assert_eq!(context["state"]["empty"], "");
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
fn managed_capture_nonzero_exit_remains_a_fatal_custom_check_error() {
    let (_dir, recipes) = create_test_env();
    let source = r#"
const RECIPE_API = 2;
let ctx = #{name: "managed-capture-failure"};
fn is_installed(ctx) {
    ctx.state.unreachable = exec_output("/usr/bin/bash", ["--noprofile", "--norc", "-c",
        "printf capture-failed-stdout; printf capture-failed-stderr >&2; exit 23"]);
    ready(ctx)
}
"#;
    write_recipe(&recipes, "check", source);
    let output = managed_command(&recipes, "isinstalled").output().unwrap();
    assert!(!output.status.success());
    assert!(
        output.stdout.is_empty(),
        "failure emitted a successful context"
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
fn managed_large_file_lifecycle_preserves_custom_content_checks() {
    let (_dir, recipes) = create_test_env();
    let source = r#"
const RECIPE_API = 2;
let ctx = #{name: "managed-large-file"};
fn expected_content() {
    let value = "x";
    for index in 0..17 { value += value; }
    value
}
fn is_acquired(ctx) { ready(ctx) }
fn is_built(ctx) {
    if is_file("/work/build/large-file") {
        if read_file("/work/build/large-file") == expected_content() { return ready(ctx); }
    }
    needs_work(ctx, "large source product has not been verified")
}
fn build(ctx) {
    write_file("/work/build/large-file", expected_content());
    ctx
}
fn is_installed(ctx) {
    if is_file("/out/large-file") {
        if read_file("/out/large-file") == expected_content() { return ready(ctx); }
    }
    needs_work(ctx, "large installed bytes are missing or invalid")
}
fn install(ctx) {
    write_file("/out/large-file", read_file("/work/build/large-file"));
    ctx.outputs.proof = "/out/large-file";
    ctx.state.verified_bytes = 131072;
    ctx
}
"#;
    write_recipe(&recipes, "check", source);
    let output = managed_command(&recipes, "install").output().unwrap();
    assert_success(&output);
    let context: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(context["state"]["verified_bytes"], 131072);
    assert_eq!(
        std::fs::read(context["outputs"]["proof"].as_str().unwrap()).unwrap(),
        vec![b'x'; 131072]
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("launcher exit 0; result exited"));
    assert_eq!(
        std::fs::read_to_string(recipes.join("check.rhai")).unwrap(),
        source
    );
}

#[test]
fn managed_directory_lifecycle_copies_moves_and_cleans_nested_products() {
    let (_dir, recipes) = create_test_env();
    let source = r#"
const RECIPE_API = 2;
let ctx = #{name: "managed-directory", state: #{cleaned: false}};
fn expected_content() {
    let value = "d";
    for index in 0..17 { value += value; }
    value
}
fn is_acquired(ctx) { ready(ctx) }
fn is_built(ctx) {
    if is_file("/work/build/source/nested/input") {
        if read_file("/work/build/source/nested/input") == expected_content() { return ready(ctx); }
    }
    needs_work(ctx, "source tree has not been produced and checked")
}
fn build(ctx) {
    mkdir("/work/build/source/nested");
    write_file("/work/build/source/nested/input", expected_content());
    write_file("/work/build/source/manifest", "directory-product-v1");
    mkdir("/work/tmp/copy-scratch/nested");
    write_file("/work/tmp/copy-scratch/nested/discard", "temporary");
    ctx
}
fn cleanup(ctx, reason) {
    if is_dir("/work/tmp/copy-scratch") { rm("/work/tmp/copy-scratch"); }
    ctx.state.cleaned = true;
    ctx
}
fn is_installed(ctx) {
    if is_file("/out/final/nested/input") && is_file("/out/final/manifest") {
        if read_file("/out/final/nested/input") == expected_content() &&
           read_file("/out/final/manifest") == "directory-product-v1" &&
           !is_dir("/out/staged") && !is_dir("/work/tmp/copy-scratch") { return ready(ctx); }
    }
    needs_work(ctx, "copied, moved and cleaned products have not been verified")
}
fn install(ctx) {
    if is_dir("/work/tmp/copy-scratch") { throw "build cleanup did not remove its tree"; }
    mkdir("/out/staged");
    copy_tree_contents("/work/build/source", "/out/staged");
    mv("/out/staged", "/out/final");
    ctx.outputs.tree = "/out/final";
    ctx
}
"#;
    write_recipe(&recipes, "check", source);
    let output = managed_command(&recipes, "install").output().unwrap();
    assert_success(&output);
    let context: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let published = Path::new(context["outputs"]["tree"].as_str().unwrap());
    assert_eq!(
        std::fs::read(published.join("manifest")).unwrap(),
        b"directory-product-v1"
    );
    assert_eq!(
        std::fs::read(published.join("nested/input")).unwrap(),
        vec![b'd'; 131072]
    );
    assert_eq!(context["state"]["cleaned"], true);
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
const RECIPE_API = 2;
let ctx = #{name: "managed-rename-guard"};
fn is_installed(ctx) {
    write_file("/work/build/replacement", "replace staged recipe");
    mv("/work/build/replacement", "/inputs/main.rhai");
    ready(ctx)
}
"#;
    write_recipe(&recipes, "check", source);
    let output = managed_command(&recipes, "isinstalled").output().unwrap();
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
const RECIPE_API = 2;
let ctx = #{name: "managed-owned-elf"};
fn is_acquired(ctx) { ready(ctx) }
fn is_built(ctx) {
    if is_file("/work/build/owned-result") && ctx.state.contains("captured") {
        if read_file("/work/build/owned-result") == "owned-elf-product" &&
           ctx.state.code == 0 && ctx.state.captured == "owned-elf-capture" { return ready(ctx); }
    }
    needs_work(ctx, "generated ELF has not produced verified bytes")
}
fn build(ctx) {
    copy_file("/usr/bin/bash", "/work/build/generated-bash");
    ctx.state.code = exec("/work/build/generated-bash", ["--noprofile", "--norc", "-c",
        "printf owned-elf-product > /work/build/owned-result; printf owned-elf-stdout; printf owned-elf-stderr >&2"]);
    ctx.state.captured = exec_output("/work/build/generated-bash", ["--noprofile", "--norc", "-c", "printf owned-elf-capture"]);
    ctx
}
fn is_installed(ctx) {
    if is_file("/out/owned-result") {
        if read_file("/out/owned-result") == "owned-elf-product" { return ready(ctx); }
    }
    needs_work(ctx, "owned executable result has not been installed and checked")
}
fn install(ctx) {
    copy_file("/work/build/owned-result", "/out/owned-result");
    ctx.outputs.proof = "/out/owned-result";
    ctx
}
"#;
    write_recipe(&recipes, "check", source);
    let output = managed_command(&recipes, "install").output().unwrap();
    assert_success(&output);
    let context: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(context["state"]["code"], 0);
    assert_eq!(context["state"]["captured"], "owned-elf-capture");
    assert_eq!(
        std::fs::read(context["outputs"]["proof"].as_str().unwrap()).unwrap(),
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
const RECIPE_API = 2;
let ctx = #{name: "managed-owned-permissions"};
fn is_installed(ctx) {
    write_file("/work/build/not-executable", "ordinary data");
    exec_checked("/work/build/not-executable", []);
    ready(ctx)
}
"#;
    write_recipe(&recipes, "check", source);
    let output = managed_command(&recipes, "isinstalled").output().unwrap();
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
const RECIPE_API = 2;
let ctx = #{name: "managed-owned-script"};
fn is_acquired(ctx) { ready(ctx) }
fn is_built(ctx) {
    if is_file("/work/build/script-result") && ctx.state.contains("captured") {
        if read_file("/work/build/script-result") == "script-product" && ctx.state.code == 0 &&
           ctx.state.captured == "/work/build/generated-script|argument with spaces|last" {
            return ready(ctx);
        }
    }
    needs_work(ctx, "generated script has not produced verified bytes")
}
fn build(ctx) {
    write_file("/work/build/generated-script", "#!/usr/bin/bash --noprofile\nprintf script-product > /work/build/script-result\nprintf '%s|%s|%s' \"$0\" \"$1\" \"$2\"\nprintf script-stderr >&2\n");
    chmod("/work/build/generated-script", 0o755);
    ctx.state.captured = exec_output("/work/build/generated-script", ["argument with spaces", "last"]);
    copy_file("/usr/bin/true", "/work/build/generated-true");
    ctx.state.code = exec("/work/build/generated-true", []);
    ctx
}
fn is_installed(ctx) {
    if is_file("/out/script-result") {
        if read_file("/out/script-result") == "script-product" { return ready(ctx); }
    }
    needs_work(ctx, "script product is not installed and checked")
}
fn install(ctx) {
    copy_file("/work/build/script-result", "/out/script-result");
    ctx.outputs.proof = "/out/script-result";
    ctx
}
"##;
    write_recipe(&recipes, "check", source);
    let output = managed_command(&recipes, "install").output().unwrap();
    assert_success(&output);
    let context: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(context["state"]["code"], 0);
    assert_eq!(
        context["state"]["captured"],
        "/work/build/generated-script|argument with spaces|last"
    );
    assert_eq!(
        std::fs::read(context["outputs"]["proof"].as_str().unwrap()).unwrap(),
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
const RECIPE_API = 2;
let ctx = #{name: "managed-descriptor-script"};
fn is_installed(ctx) {
    write_file("/work/build/fd-script", "printf '%s|%s' \"$0\" \"$1\"\nprintf fd-product > /work/build/fd-result\n");
    ctx.state.captured = exec_output("/usr/bin/bash", ["--noprofile", "--norc", "-c",
        "exec 8</work/build/fd-script; /usr/bin/rm /work/build/fd-script; source /dev/fd/8 'spaced argument'", "fd-shell"]);
    ctx.state.removed = !is_file("/work/build/fd-script");
    if ctx.state.removed && ctx.state.captured == "fd-shell|spaced argument" && read_file("/work/build/fd-result") == "fd-product" {
        return ready(ctx);
    }
    needs_work(ctx, "descriptor script did not produce the expected bytes")
}
"##;
    write_recipe(&recipes, "check", source);
    let output = managed_command(&recipes, "isinstalled").output().unwrap();
    assert_success(&output);
    let context: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(context["state"]["captured"], "fd-shell|spaced argument");
    assert_eq!(context["state"]["removed"], true);
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
fn cli_partial_launcher_frame_times_out_and_reaps_pipe_holders() {
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
import os, time
os.write(1, b'\x00')
child = os.fork()
if child == 0:
    time.sleep(60)
else:
    time.sleep(60)
"#,
    )
    .unwrap();
    std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755)).unwrap();
    let start = std::time::Instant::now();
    let output = image_command(&recipes, &archive, &digest)
        .env("GINUX_BIN", &launcher)
        .env("RECIPE_FRAME_TIMEOUT_SECS", "1")
        .output()
        .unwrap();
    assert!(!output.status.success() && output.stdout.is_empty());
    assert!(start.elapsed() < std::time::Duration::from_secs(20));
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(
        error.contains("complete worker frame deadline expired"),
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
const RECIPE_API = 2;
let ctx = #{name: "cancel-check"};
fn is_installed(ctx) {
    print("managed-cancellation-ready");
    exec_checked("/usr/bin/sleep", ["60"]);
    ready(ctx)
}
"#,
    );
    let mut child = managed_command(&recipes, "isinstalled")
        .env("TMPDIR", dir.path())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let pid = child.id();
    let stderr = child.stderr.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if line.contains("managed-cancellation-ready") {
                let _ = tx.send(());
            }
        }
    });
    if let Err(error) = rx.recv_timeout(std::time::Duration::from_secs(30)) {
        child.kill().ok();
        child.wait().ok();
        panic!("worker did not reach cancellation point: {error}");
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
        "const RECIPE_API = 2; let ctx = #{name: \"quick\"}; fn is_installed(ctx) { ready(ctx) }",
    );
    let output = managed_command(&recipes, "isinstalled")
        .env("TMPDIR", dir.path())
        .output()
        .unwrap();
    assert_success(&output);
    assert!(
        abandoned.iter().all(|path| path.exists()),
        "concurrent invocation removed active scratch"
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
    let output = managed_command(&recipes, "isinstalled")
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
const RECIPE_API = 2;
let ctx = #{name: "publication"};
fn is_acquired(ctx) { ready(ctx) }
fn is_built(ctx) { ready(ctx) }
fn is_installed(ctx) { if ctx.state.contains("done") { ready(ctx) } else { needs_work(ctx, "produce") } }
fn install(ctx) {
    mkdir("/out/tree/empty");
    write_file("/out/tree/product", "published-bytes");
    let long_name = "";
    for index in 0..120 { long_name += "a"; }
    write_file("/out/tree/" + long_name, "long-name-product");
    ln("product", "/out/tree/link");
    ln("/usr/bin/program", "/out/tree/absolute");
    ln(long_name, "/out/tree/long-link");
    write_file("/work/private", "not selected");
    ctx.outputs.tree = "/out/tree";
    ctx.outputs.file = "/out/tree/product";
    ctx.outputs.alias = "/out/tree/product";
    ctx.state.done = true;
    ctx
}
"#;
    write_recipe(&recipes, "check", source);
    let output = managed_command(&recipes, "install").output().unwrap();
    assert_success(&output);
    let context: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let tree = Path::new(context["outputs"]["tree"].as_str().unwrap());
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
    for path in [
        tree.join("product"),
        published.join("file"),
        published.join("alias"),
    ] {
        assert_eq!(std::fs::read(path).unwrap(), b"published-bytes");
    }
    assert_eq!(std::fs::read_dir(published).unwrap().count(), 3);
    assert!(published.starts_with(dir.path().join(".artifacts/ginux-export")));
    let before = std::fs::read_dir(published.parent().unwrap())
        .unwrap()
        .count();
    write_recipe(
        &recipes,
        "check",
        &source.replace(
            "ctx.outputs.alias = \"/out/tree/product\";",
            "ctx.outputs.alias = \"/out/missing\";",
        ),
    );
    let output = managed_command(&recipes, "install").output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("declared output is absent"));
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
const RECIPE_API = 2;
let ctx = #{name: "export-boundary"};
fn is_acquired(ctx) { ready(ctx) }
fn is_built(ctx) { ready(ctx) }
fn is_installed(ctx) { if ctx.state.contains("done") { ready(ctx) } else { needs_work(ctx, "produce") } }
fn install(ctx) { write_file("/out/product", "valid"); ctx.outputs.product = "/out/product"; ctx.state.done = true; ctx }
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
if result.returncode: sys.exit(result.returncode)
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
            error.contains("cannot publish completed domain outputs"),
            "{error}"
        );
        assert_eq!(std::fs::read(&sentinel).unwrap(), b"keep");
        let exports = dir.path().join(".artifacts/ginux-export");
        if exports.exists() {
            assert_eq!(std::fs::read_dir(exports).unwrap().count(), 0);
        }
    }
}

#[test]
fn managed_generated_scripts_use_owned_and_image_interpreter_chains() {
    let (_dir, recipes) = create_test_env();
    write_recipe(
        &recipes,
        "check",
        r##"
const RECIPE_API = 2;
let ctx = #{name: "interpreter-chain"};
fn is_installed(ctx) {
    copy_file("/usr/bin/bash", "/work/tools/bash"); chmod("/work/tools/bash", 0o755);
    let body = "\nprintf '%s|%s|%s|%s|%s' \"$0\" \"$1\" \"$2\" \"$3\" \"$4\"\n";
    write_file("/work/tools/middle", "#!/work/tools/bash --noprofile" + body);
    chmod("/work/tools/middle", 0o755);
    write_file("/work/tools/outer", "#!/work/tools/middle outer option\n");
    chmod("/work/tools/outer", 0o755);
    ctx.owned = exec_output("/work/tools/outer", ["spaced argument", "last"]);
    // A completed interpreter releases script pins so a later launch can use
    // changed contents while the original executable input stays immutable.
    write_file("/work/tools/middle", "#!/usr/bin/bash --noprofile" + body);
    ctx.image = exec_output("/work/tools/outer", ["spaced argument", "last"]);
    ready(ctx)
}
"##,
    );
    let output = managed_command(&recipes, "isinstalled").output().unwrap();
    assert_success(&output);
    let ctx: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let expected = "/work/tools/middle|outer option|/work/tools/outer|spaced argument|last";
    assert_eq!(ctx["owned"], expected);
    assert_eq!(ctx["image"], expected);
}
