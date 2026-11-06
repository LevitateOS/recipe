//! Retention through actual CLI checkpoints, identities and OS-held log leases.
use super::*;
use fs2::FileExt;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};

#[test]
fn managed_retention_bounds_history_preserves_active_logs_and_published_products() {
    let (dir, recipes) = create_test_env();
    write_recipe(
        &recipes,
        "check",
        r#"
const RECIPE_API = 2;
let ctx = #{name: "retention", value: FACT};
fn is_acquired(ctx) { ready(ctx) }
fn is_built(ctx) { if is_file("/work/build/product") { return ready(ctx); } needs_work(ctx,"build") }
fn build(ctx) { write_file("/work/build/product", ctx.value); ctx }
fn is_installed(ctx) {
    if !is_file("/out/product") { return needs_work(ctx,"install"); }
    if read_file("/out/product") != ctx.value { throw "wrong retained identity"; }
    ctx.outputs.product="/out/product"; ready(ctx)
}
fn install(ctx) { copy_file("/work/build/product", "/out/product"); ctx }
"#,
    );
    let state = dir.path().join("retained");
    let invoke = |value: &str| {
        let output = recipe_command(&recipes)
            .args(["install", "check", "--recipes-path"])
            .arg(&recipes)
            .arg("--state-dir")
            .arg(&state)
            .arg("--build-dir")
            .arg(dir.path().join("destination"))
            .args(["--define", &format!("FACT={value}")])
            .output()
            .unwrap();
        assert_success(&output);
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap()
    };
    let first = invoke("first");
    let original = std::path::PathBuf::from(first["outputs"]["product"].as_str().unwrap());
    let logroot = state.join("logs");
    for index in 0..40 {
        let name = format!("retention-history-{index}");
        let path = logroot.join(&name);
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .unwrap();
        std::fs::write(
            path.join("result.json"),
            serde_json::to_vec(&serde_json::json!({"invocation":name,"status":"succeeded"}))
                .unwrap(),
        )
        .unwrap();
        std::fs::write(path.join("worker.log"), "old completed diagnostics").unwrap();
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path.join("lease"))
            .unwrap();
    }
    let active_path = logroot.join("retention-history-0");
    let active = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(active_path.join("lease"))
        .unwrap();
    active.lock_exclusive().unwrap();
    let foreign = logroot.join("operator-notes");
    std::fs::create_dir(&foreign).unwrap();
    std::fs::write(foreign.join("notes"), "keep human file").unwrap();
    for value in ["second", "third", "fourth", "fifth"] {
        invoke(value);
    }
    assert_eq!(std::fs::read_to_string(&original).unwrap(), "first");
    assert_eq!(
        std::fs::read_to_string(active_path.join("worker.log")).unwrap(),
        "old completed diagnostics"
    );
    assert_eq!(
        std::fs::read_to_string(foreign.join("notes")).unwrap(),
        "keep human file"
    );
    let ownedlogs = std::fs::read_dir(&logroot)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.path().join("result.json").is_file())
        .count();
    // 32 retained invocations, plus at most one still holding its lease.
    assert!((31..=33).contains(&ownedlogs), "history not bounded: {ownedlogs}");
    let pointers = walkdir::WalkDir::new(&state)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name() == "current.json")
        .collect::<Vec<_>>();
    assert_eq!(pointers.len(), 4, "input identity history not bounded");
    for pointer in pointers {
        let identity = pointer.path().parent().unwrap();
        let generations = std::fs::read_dir(identity)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().unwrap().is_dir())
            .count();
        assert_eq!(generations, 2, "current/previous generation not preserved");
        let record: serde_json::Value =
            serde_json::from_slice(&std::fs::read(pointer.path()).unwrap()).unwrap();
        assert!(identity.join(record["archive"].as_str().unwrap()).is_file());
    }
}
