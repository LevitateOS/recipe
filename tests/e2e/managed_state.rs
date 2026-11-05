use super::*;
use std::io::Write;
use std::process::Stdio;

fn retained_command(recipes: &Path, operation: &str) -> Command {
    let mut command = recipe_command(recipes);
    command
        .env(
            "GINUX_BIN",
            std::env::var_os("GINUX_BIN").expect("real Ginux launcher required"),
        )
        .env("TMPDIR", recipes.parent().unwrap())
        .env_remove("RECIPE_WORKER")
        .env_remove("RECIPE_INVOCATION")
        .args([operation, "check", "--recipes-path"])
        .arg(recipes)
        .arg("--state-dir")
        .arg(recipes.parent().unwrap().join("state"))
        .arg("--build-dir")
        .arg(recipes.parent().unwrap().join("destination"));
    command
}

const SOURCE: &str = r#"
const RECIPE_API = 2;
let ctx = #{name: "retained-probe", version: FACT, state: #{builds: 0, installs: 0}};
fn is_acquired(ctx) { ready(ctx) }
fn is_built(ctx) {
    if is_file("/work/build/product") && ctx.state.builds == 1 {
        if read_file("/work/build/product") == ctx.version { return ready(ctx); }
    }
    needs_work(ctx, "build matching product")
}
fn build(ctx) {
    if ctx.state.builds != 0 { throw "stale build state was reused"; }
    write_file("/work/build/product", ctx.version);
    ctx.state.builds += 1;
    ctx.state.nested = #{array: [1, true, "value"], child: #{number: 1.25}};
    ctx
}
fn is_installed(ctx) {
    if is_file("/out/product") && ctx.state.installs == 1 {
        if read_file("/out/product") == ctx.version {
            ctx.outputs.product = "/out/product";
            return ready(ctx);
        }
    }
    needs_work(ctx, "install matching product")
}
fn install(ctx) {
    write_file("/out/product", read_file("/work/build/product"));
    ctx.state.installs += 1;
    ctx.outputs.product = "/out/product";
    ctx
}
"#;

#[test]
fn managed_state_restores_files_and_nested_values_without_rewriting_sources() {
    let (dir, recipes) = create_test_env();
    write_recipe(&recipes, "check", SOURCE);
    for (fact, operation) in [
        ("alpha", "install"),
        ("alpha", "install"),
        ("alpha", "isinstalled"),
        ("beta", "install"),
    ] {
        let output = retained_command(&recipes, operation)
            .args(["--define", &format!("FACT={fact}")])
            .output()
            .unwrap();
        assert_success(&output);
        let context: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(context["version"], fact);
        assert_eq!(context["runtime"]["options"]["persist_state"], true);
        assert_eq!(context["state"]["builds"], 1);
        assert_eq!(context["state"]["installs"], 1);
        assert_eq!(
            context["state"]["nested"],
            serde_json::json!({"array":[1,true,"value"],"child":{"number":1.25}})
        );
        assert_eq!(
            std::fs::read(context["outputs"]["product"].as_str().unwrap()).unwrap(),
            fact.as_bytes()
        );
        assert_eq!(
            std::fs::read_to_string(recipes.join("check.rhai")).unwrap(),
            SOURCE
        );
    }
    let records = walkdir::WalkDir::new(dir.path().join("state"))
        .into_iter()
        .map(Result::unwrap)
        .filter(|entry| entry.file_name() == "current.json")
        .collect::<Vec<_>>();
    assert_eq!(
        records.len(),
        2,
        "changed inputs need a different state identity"
    );
    for record in records {
        let state: serde_json::Value =
            serde_json::from_slice(&std::fs::read(record.path()).unwrap()).unwrap();
        assert!(state["generation"].as_u64().unwrap() >= 3);
        assert!(
            state["states"]["/inputs/main.rhai"]
                .get("version")
                .is_none()
        );
    }
}

fn wait_for_log(child: &mut std::process::Child, path: &Path, marker: &str) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(45);
    loop {
        let text = std::fs::read_to_string(path).unwrap_or_default();
        if text.contains(marker) {
            return;
        }
        assert!(
            child.try_wait().unwrap().is_none(),
            "exited before {marker}: {text}"
        );
        assert!(
            std::time::Instant::now() < deadline,
            "missing {marker}: {text}"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

#[test]
fn managed_state_recovers_before_and_after_checkpoint_acknowledgement() {
    for pause in ["before", "after"] {
        let (dir, recipes) = create_test_env();
        let source = format!(
            r#"
const RECIPE_API = 2;
const PAUSE = "{pause}";
let ctx = #{{name: "interrupted", state: #{{acquires: 0, builds: 0}}}};
fn is_acquired(ctx) {{
    if is_file("/work/source") && ctx.state.acquires == 1 {{ return ready(ctx); }}
    needs_work(ctx, "acquire")
}}
fn acquire(ctx) {{ write_file("/work/source", "source"); ctx.state.acquires += 1; ctx }}
fn is_built(ctx) {{
    if is_file("/work/built") && ctx.state.builds == 1 {{ return ready(ctx); }}
    needs_work(ctx, "build")
}}
fn build(ctx) {{
    if is_file("/work/built") {{ throw "unacknowledged bytes survived recovery"; }}
    print("BUILD-RAN");
    write_file("/work/built", "built");
    ctx.state.builds += 1;
    if PAUSE == "before" {{ print("BEFORE-CHECKPOINT"); exec_checked("/usr/bin/sleep", ["3"]); }}
    ctx
}}
fn is_installed(ctx) {{
    if is_file("/out/product") {{ ctx.outputs.product = "/out/product"; return ready(ctx); }}
    needs_work(ctx, "install")
}}
fn install(ctx) {{
    if PAUSE == "after" {{ print("AFTER-CHECKPOINT"); exec_checked("/usr/bin/sleep", ["3"]); }}
    write_file("/out/product", read_file("/work/built")); ctx.outputs.product = "/out/product"; ctx
}}
"#
        );
        write_recipe(&recipes, "check", &source);
        let log = dir.path().join("interrupted.log");
        let mut child = retained_command(&recipes, "install")
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(&log).unwrap())
            .spawn()
            .unwrap();
        wait_for_log(
            &mut child,
            &log,
            if pause == "before" {
                "BEFORE-CHECKPOINT"
            } else {
                "AFTER-CHECKPOINT"
            },
        );
        child.kill().unwrap();
        assert!(!child.wait().unwrap().success());
        let output = retained_command(&recipes, "install").output().unwrap();
        assert_success(&output);
        let context: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(context["state"]["acquires"], 1);
        assert_eq!(context["state"]["builds"], 1);
        assert_eq!(
            String::from_utf8_lossy(&output.stderr).contains("BUILD-RAN"),
            pause == "before"
        );
        assert_eq!(
            std::fs::read(context["outputs"]["product"].as_str().unwrap()).unwrap(),
            b"built"
        );
        assert_eq!(
            std::fs::read_to_string(recipes.join("check.rhai")).unwrap(),
            source
        );
    }
}

#[test]
fn managed_state_rejects_modified_snapshot_bytes() {
    use std::io::{Seek, SeekFrom};
    let (dir, recipes) = create_test_env();
    write_recipe(&recipes, "check", SOURCE);
    assert_success(
        &retained_command(&recipes, "install")
            .args(["--define", "FACT=alpha"])
            .output()
            .unwrap(),
    );
    let current = walkdir::WalkDir::new(dir.path().join("state"))
        .into_iter()
        .map(Result::unwrap)
        .find(|entry| entry.file_name() == "current.json")
        .unwrap();
    let record: serde_json::Value =
        serde_json::from_slice(&std::fs::read(current.path()).unwrap()).unwrap();
    let archive = current
        .path()
        .parent()
        .unwrap()
        .join(record["archive"].as_str().unwrap());
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .open(archive)
        .unwrap();
    file.seek(SeekFrom::Start(0)).unwrap();
    file.write_all(b"invalid").unwrap();
    let output = retained_command(&recipes, "install")
        .args(["--define", "FACT=alpha"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("snapshot digest mismatch"));
    assert_eq!(
        std::fs::read_to_string(recipes.join("check.rhai")).unwrap(),
        SOURCE
    );
}

#[test]
fn managed_state_serializes_different_inputs_sharing_a_destination() {
    let (dir, recipes) = create_test_env();
    let source = SOURCE.replace(
        "fn build(ctx) {",
        "fn build(ctx) { print(\"WRITER-ENTERED\"); exec_checked(\"/usr/bin/sleep\", [\"3\"]);",
    );
    write_recipe(&recipes, "check", &source);
    let first_log = dir.path().join("writer-one.log");
    let mut first = retained_command(&recipes, "install")
        .args(["--define", "FACT=alpha"])
        .stdout(Stdio::piped())
        .stderr(std::fs::File::create(&first_log).unwrap())
        .spawn()
        .unwrap();
    wait_for_log(&mut first, &first_log, "WRITER-ENTERED");
    let second_log = dir.path().join("writer-two.log");
    let mut second = retained_command(&recipes, "install")
        .args(["--define", "FACT=beta"])
        .stdout(Stdio::piped())
        .stderr(std::fs::File::create(&second_log).unwrap())
        .spawn()
        .unwrap();
    wait_for_log(&mut second, &second_log, "waiting for shared destination");
    assert!(
        !std::fs::read_to_string(&second_log)
            .unwrap()
            .contains("WRITER-ENTERED")
    );
    let a = first.wait_with_output().unwrap();
    let b = second.wait_with_output().unwrap();
    assert_success(&a);
    assert_success(&b);
    let a: serde_json::Value = serde_json::from_slice(&a.stdout).unwrap();
    let b: serde_json::Value = serde_json::from_slice(&b.stdout).unwrap();
    assert_eq!(
        std::fs::read(a["outputs"]["product"].as_str().unwrap()).unwrap(),
        b"alpha"
    );
    assert_eq!(
        std::fs::read(b["outputs"]["product"].as_str().unwrap()).unwrap(),
        b"beta"
    );
}

#[test]
fn managed_state_restores_dependencies_and_saves_reused_check_state() {
    let (dir, recipes) = create_test_env();
    let dependency = r#"
const RECIPE_API = 2;
let ctx = #{name: "helper", state: #{installs: 0, checks: 0}};
fn is_acquired(ctx) { ready(ctx) }
fn is_installed(ctx) {
    if is_file("/work/tools/helper") {
        if ctx.state.installs != 1 { throw "dependency files/state mismatch"; }
        ctx.state.checks += 1;
        return ready(ctx);
    }
    needs_work(ctx, "helper missing")
}
fn install(ctx) {
    if ctx.state.installs != 0 { throw "stale dependency state"; }
    write_file("/work/tools/helper", "helper"); ctx.state.installs += 1; ctx
}
"#;
    let parent = r#"
const RECIPE_API = 2;
let deps = ["helper"];
let ctx = #{name: "parent", state: #{runs: 0}};
fn is_acquired(ctx) { ready(ctx) }
fn is_installed(ctx) {
    if ctx.contains("complete") { return ready(ctx); }
    needs_work(ctx, "exercise dependency reuse")
}
fn install(ctx) {
    if read_file("/work/tools/helper") != "helper" { throw "dependency missing"; }
    ctx.complete = true; ctx.state.runs += 1; ctx
}
"#;
    write_recipe(&recipes, "helper", dependency);
    write_recipe(&recipes, "check", parent);
    for run in 1..=3 {
        let output = retained_command(&recipes, "install").output().unwrap();
        assert_success(&output);
        let ctx: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(ctx["state"]["runs"], run);
    }
    let current = walkdir::WalkDir::new(dir.path().join("state"))
        .into_iter()
        .map(Result::unwrap)
        .find(|entry| entry.file_name() == "current.json")
        .unwrap();
    let record: serde_json::Value =
        serde_json::from_slice(&std::fs::read(current.path()).unwrap()).unwrap();
    assert_eq!(record["states"]["/inputs/helper.rhai"]["installs"], 1);
    // The post-install check and two reused checks all persist their discoveries.
    assert_eq!(record["states"]["/inputs/helper.rhai"]["checks"], 3);
    assert_eq!(record["states"]["/inputs/main.rhai"]["runs"], 3);
    for (name, source) in [("helper", dependency), ("check", parent)] {
        assert_eq!(
            std::fs::read_to_string(recipes.join(format!("{name}.rhai"))).unwrap(),
            source
        );
    }
}

#[test]
fn managed_state_disabled_persistence_and_legacy_migration_are_explicit() {
    let (dir, recipes) = create_test_env();
    write_recipe(&recipes, "check", SOURCE);
    for option in ["--no-persist-state", "--no-persist-ctx"] {
        let output = retained_command(&recipes, "install")
            .args(["--define", "FACT=alpha", option])
            .output()
            .unwrap();
        assert_success(&output);
        let ctx: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(ctx["state"]["installs"], 1);
        assert_eq!(
            std::fs::read(ctx["outputs"]["product"].as_str().unwrap()).unwrap(),
            b"alpha"
        );
        assert!(!dir.path().join("state").exists());
    }
    let legacy = "let ctx = #{name: \"legacy\"}; fn is_installed(ctx) { ctx }";
    write_recipe(&recipes, "check", legacy);
    let output = retained_command(&recipes, "isinstalled").output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("requires RECIPE_API = 2"));
    assert_success(
        &retained_command(&recipes, "isinstalled")
            .arg("--no-persist-state")
            .output()
            .unwrap(),
    );
    assert_eq!(
        std::fs::read_to_string(recipes.join("check.rhai")).unwrap(),
        legacy
    );
}

#[test]
fn managed_state_invalidates_changed_source_and_declared_input_bytes() {
    let (dir, recipes) = create_test_env();
    let asset = dir.path().join("asset");
    let source = SOURCE.replace("version: FACT", "version: read_file(\"/inputs/asset\")");
    write_recipe(&recipes, "check", &source);
    for (bytes, edit_source) in [("alpha", false), ("beta", false), ("beta", true)] {
        std::fs::write(&asset, bytes).unwrap();
        if edit_source {
            write_recipe(
                &recipes,
                "check",
                &(source.clone() + "\n// revised input\n"),
            );
        }
        let output = retained_command(&recipes, "install")
            .arg("--input")
            .arg(format!("asset={}", asset.display()))
            .output()
            .unwrap();
        assert_success(&output);
        let ctx: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(ctx["state"]["builds"], 1);
        assert_eq!(
            std::fs::read(ctx["outputs"]["product"].as_str().unwrap()).unwrap(),
            bytes.as_bytes()
        );
    }
    assert_eq!(
        walkdir::WalkDir::new(dir.path().join("state"))
            .into_iter()
            .map(Result::unwrap)
            .filter(|entry| entry.file_name() == "current.json")
            .count(),
        3
    );
}

#[test]
fn managed_state_defaults_to_application_state_without_a_checkout() {
    let (dir, recipes) = create_test_env();
    write_recipe(&recipes, "check", SOURCE);
    for _ in 0..2 {
        let output = recipe_command(&recipes)
            .env("GINUX_BIN", std::env::var_os("GINUX_BIN").unwrap())
            .env("XDG_STATE_HOME", dir.path().join("application-state"))
            .env_remove("RECIPE_WORKER")
            .env_remove("RECIPE_INVOCATION")
            .args(["install", "check", "--recipes-path"])
            .arg(&recipes)
            .args(["--define", "FACT=alpha"])
            .output()
            .unwrap();
        assert_success(&output);
        let ctx: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(ctx["state"]["installs"], 1);
    }
    assert_eq!(
        walkdir::WalkDir::new(dir.path().join("application-state"))
            .into_iter()
            .map(Result::unwrap)
            .filter(|entry| entry.file_name() == "current.json")
            .count(),
        1
    );
    assert_eq!(
        std::fs::read_to_string(recipes.join("check.rhai")).unwrap(),
        SOURCE
    );
}

#[test]
fn managed_state_rejects_nonserializable_values_with_the_field_path() {
    let (_dir, recipes) = create_test_env();
    let source = r#"
const RECIPE_API = 2;
let ctx = #{name: "bad-state"};
fn helper() {}
fn is_installed(ctx) { ctx.state.child = #{callback: Fn("helper")}; ready(ctx) }
"#;
    write_recipe(&recipes, "check", source);
    let output = retained_command(&recipes, "isinstalled").output().unwrap();
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(
        error.contains("ctx.state[\"child\"][\"callback\"]"),
        "{error}"
    );
    assert!(error.contains("not serializable runtime state"), "{error}");
    assert_eq!(
        std::fs::read_to_string(recipes.join("check.rhai")).unwrap(),
        source
    );
}

#[test]
fn managed_options_force_phases_without_invalidating_saved_installation() {
    let (dir, recipes) = create_test_env();
    let source = r#"
const RECIPE_API = 2;
let deps = ["helper"];
let ctx = #{name: "options", state: #{acquires: 0, builds: 0, installs: 0}};
fn is_acquired(ctx) { if is_file("/work/source") { ready(ctx) } else { needs_work(ctx, "source") } }
fn is_built(ctx) { if is_file("/work/built") { ready(ctx) } else { needs_work(ctx, "build") } }
fn is_installed(ctx) {
    if is_file("/out/product") { ctx.outputs.product = "/out/product"; ready(ctx) }
    else { needs_work(ctx, "install") }
}
fn acquire(ctx) { write_file("/work/source", "source"); ctx.state.acquires += 1; ctx }
fn build(ctx) { write_file("/work/built", "built"); ctx.state.builds += 1; ctx }
fn install(ctx) {
    if NPROC != RUNTIME.options.jobs || env("NPROC") != NPROC.to_string() { throw "jobs not transported"; }
    if ARCH != RUNTIME.target.arch { throw "target not transported"; }
    write_file("/out/product", "product"); ctx.state.installs += 1; ctx.outputs.product = "/out/product"; ctx
}
"#;
    let dependency = r#"
const RECIPE_API = 2;
let ctx = #{name: "helper", state: #{installs: 0}};
fn is_acquired(ctx) { ready(ctx) }
fn is_installed(ctx) { if is_file("/work/tools/helper") { ready(ctx) } else { needs_work(ctx, "helper") } }
fn install(ctx) {
    if ctx.state.installs != 0 { throw "parent force leaked into dependency"; }
    if RUNTIME.options.force_acquire || RUNTIME.options.force_build || RUNTIME.options.force_install { throw "dependency force flags"; }
    write_file("/work/tools/helper", "helper"); ctx.state.installs += 1; ctx
}
"#;
    write_recipe(&recipes, "check", source);
    write_recipe(&recipes, "helper", dependency);
    for (args, counts) in [
        (vec![], [1, 1, 1]),
        (vec![], [1, 1, 1]),
        (vec!["--force-install"], [1, 1, 2]),
        (vec!["--force-build"], [1, 2, 3]),
        (vec!["--force-acquire"], [2, 3, 4]),
        (vec!["--jobs", "2"], [1, 1, 1]),
    ] {
        let output = retained_command(&recipes, "install")
            .args(&args)
            .output()
            .unwrap();
        assert_success(&output);
        let ctx: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        for (field, count) in ["acquires", "builds", "installs"].iter().zip(counts) {
            assert_eq!(ctx["state"][field], count, "{args:?} {field}");
        }
        assert_eq!(
            ctx["runtime"]["target"],
            serde_json::json!({"os":"linux","arch":"x86_64"})
        );
        assert_eq!(
            ctx["runtime"]["options"]["jobs"],
            if args.contains(&"2") { 2 } else { 1 }
        );
    }
    assert_eq!(
        walkdir::WalkDir::new(dir.path().join("state"))
            .into_iter()
            .map(Result::unwrap)
            .filter(|entry| entry.file_name() == "current.json")
            .count(),
        2
    );
    assert_eq!(
        std::fs::read_to_string(recipes.join("check.rhai")).unwrap(),
        source
    );
    assert_eq!(
        std::fs::read_to_string(recipes.join("helper.rhai")).unwrap(),
        dependency
    );
}

#[test]
fn managed_options_reject_unsupported_targets_and_invalid_force_before_setup() {
    let (dir, recipes) = create_test_env();
    let sentinel = dir.path().join("setup-effect");
    write_recipe(
        &recipes,
        "check",
        &format!(
            "write_file({:?}, \"bad\"); let ctx = #{{name: \"guard\"}};",
            sentinel.to_str().unwrap()
        ),
    );
    for (operation, args, expected) in [
        (
            "install",
            vec!["--target-arch", "aarch64"],
            "unsupported target",
        ),
        (
            "install",
            vec!["--target-os", "macos"],
            "unsupported target",
        ),
        ("install", vec!["--jobs", "0"], "jobs must be"),
        ("install", vec!["--jobs", "17"], "jobs must be"),
        (
            "isinstalled",
            vec!["--force-build"],
            "force options apply only to install",
        ),
    ] {
        let output = retained_command(&recipes, operation)
            .args(args)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(expected),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!sentinel.exists());
        assert!(!dir.path().join("state").exists());
    }
}
