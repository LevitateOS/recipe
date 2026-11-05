use super::*;
use sha2::{Digest, Sha256};

fn image_command(recipes: &Path, archive: &Path, digest: &str) -> Command {
    let mut command = recipe_command(recipes);
    command
        .env_remove("RECIPE_WORKER")
        .env_remove("RECIPE_INVOCATION")
        .env("GINUX_BIN", "/bin/false")
        .env("TMPDIR", recipes.parent().unwrap())
        .env("GINUX_ROOTFS", archive)
        .env("GINUX_ROOT", format!("oci:cli-image@sha256:{digest}"))
        .args(["isinstalled", "check", "--no-persist-ctx", "--recipes-path"])
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
        assert!(!std::fs::read_dir(dir.path()).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("recipe-domain-")
        }));
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
        assert!(!std::fs::read_dir(dir.path()).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("recipe-domain-")
        }));
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
    assert!(!std::fs::read_dir(dir.path()).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("recipe-domain-")
    }));
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
        .args([operation, "check", "--no-persist-ctx", "--recipes-path"])
        .arg(recipes);
    command
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
fn remove(ctx) { ctx.state.removed = true; ctx.state.build = BUILD_DIR; ctx.state.operation = "remove"; ctx }
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
