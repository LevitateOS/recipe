use super::*;
use std::io::Write;
use std::process::Stdio;

fn retained_command(recipes: &Path, operation: &str) -> Command {
    require_ginux();
    let mut command = recipe_command(recipes);
    command
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

/// A program that prints a marker whenever its build runs, so a test can tell
/// a build from a kept result published again.
const SOURCE: &str = r#"
const RECIPE_API = 1;
const NEEDS = #{ defines: #{ FACT: "alpha", FAIL: "no" }, outputs: #{ product: "file" } };
fn build(src, out) {
    print("BUILD-RAN");
    if DEFINES.FAIL == "yes" { throw "REINSTALL_FAILED"; }
    write_file(out, DEFINES.FACT);
}
"#;

/// Every `current.json` under `root`: path to bytes.
fn pointers(root: &Path) -> std::collections::BTreeMap<std::path::PathBuf, Vec<u8>> {
    walkdir::WalkDir::new(root)
        .into_iter()
        .map(Result::unwrap)
        .filter(|entry| entry.file_name() == "current.json")
        .map(|entry| {
            (
                entry.path().to_path_buf(),
                std::fs::read(entry.path()).unwrap(),
            )
        })
        .collect()
}

fn product(result: &serde_json::Value) -> String {
    std::fs::read_to_string(result["outputs"]["product"].as_str().unwrap()).unwrap()
}

#[test]
fn managed_state_keeps_one_result_and_publishes_it_again_without_building() {
    let (dir, recipes) = create_test_env();
    write_recipe(&recipes, "check", SOURCE);
    for (fact, builds) in [("alpha", true), ("alpha", false), ("beta", true)] {
        let output = retained_command(&recipes, "install")
            .args(["--keep", "--define", &format!("FACT={fact}")])
            .output()
            .unwrap();
        assert_success(&output);
        let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result["operation"], "install");
        assert_eq!(result["state"], "installed");
        assert_eq!(result["kept"], true);
        assert_eq!(product(&result), fact);
        assert_eq!(
            String::from_utf8_lossy(&output.stderr).contains("BUILD-RAN"),
            builds,
            "{fact}"
        );
        let statuses: Vec<_> = result["steps"]
            .as_array()
            .unwrap()
            .iter()
            .map(|step| {
                (
                    step["step"].as_str().unwrap(),
                    step["status"].as_str().unwrap(),
                )
            })
            .collect();
        assert_eq!(
            statuses,
            [
                ("download", "none"),
                ("build", if builds { "done" } else { "already" }),
                ("install", "done"),
                ("cleanup", "skipped"),
            ]
        );
        assert_eq!(
            std::fs::read_to_string(recipes.join("check.rhai")).unwrap(),
            SOURCE
        );
    }
    // A producer is not in the receipt, so what Recipe holds for it is the
    // kept result; `FACT=alpha` was pruned when `beta` kept its own.
    let state = retained_command(&recipes, "state")
        .args(["--define", "FACT=beta"])
        .output()
        .unwrap();
    assert_success(&state);
    let state: serde_json::Value = serde_json::from_slice(&state.stdout).unwrap();
    assert_eq!(state["state"], "built");
    assert!(state["on_disk"]["build"].as_u64().unwrap() > 0);
    assert_eq!(
        pointers(&dir.path().join("state")).len(),
        1,
        "one saved state at most"
    );
}

fn wait_for_log(child: &mut std::process::Child, path: &Path, marker: &str) {
    // Image preparation precedes the marker; this is a bootstrap allowance,
    // not a change to execution deadlines or the interruption point.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(300);
    loop {
        let text = std::fs::read_to_string(path).unwrap_or_default();
        if text.contains(marker) {
            return;
        }
        let exited = child.try_wait().unwrap().is_some();
        if exited || std::time::Instant::now() >= deadline {
            // A fixture failure must not leave its coordinator running.
            child.kill().ok();
            child.wait().ok();
            panic!(
                "{} {marker}: {text}",
                if exited { "exited before" } else { "missing" }
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

#[test]
fn managed_state_interrupted_build_restarts_its_step() {
    let (dir, recipes) = create_test_env();
    // The first run is killed after the first half of its build. Nothing of
    // it is kept: the next run, the same command (same recipe, defines and
    // inputs, so the same identity), does the whole step again. Only the
    // kill differs: the second run is left to finish its pause.
    let source = r#"
const RECIPE_API = 1;
const NEEDS = #{ outputs: #{ product: "file" } };
fn build(src, out) {
    let built = join_path(WORK, "built");
    if exists(built) { throw "an interrupted build's bytes survived"; }
    print("BUILD-RAN");
    write_file(built, "built");
    print("BUILD-HALF");
    exec_checked("/usr/bin/sleep", ["10"]);
    write_file(out, read_file(built));
}
"#;
    write_recipe(&recipes, "check", source);
    let log = dir.path().join("interrupted.log");
    let mut child = retained_command(&recipes, "install")
        .stdout(Stdio::null())
        .stderr(std::fs::File::create(&log).unwrap())
        .spawn()
        .unwrap();
    wait_for_log(&mut child, &log, "BUILD-HALF");
    child.kill().unwrap();
    assert!(!child.wait().unwrap().success());
    // The run's state root exists (its log was opened before the worker
    // started), so the checks below look at the real place.
    let state = dir.path().join("state");
    assert!(
        state.join("logs").is_dir(),
        "no state root at {}",
        state.display()
    );
    assert!(
        pointers(&state).is_empty(),
        "an interrupted build kept a generation"
    );
    let published = state.join("exports");
    assert!(
        !published.exists() || std::fs::read_dir(&published).unwrap().next().is_none(),
        "an interrupted build published something"
    );
    let output = retained_command(&recipes, "install").output().unwrap();
    assert_success(&output);
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(String::from_utf8_lossy(&output.stderr).contains("BUILD-RAN"));
    assert_eq!(product(&result), "built");
    assert_eq!(
        std::fs::read_to_string(recipes.join("check.rhai")).unwrap(),
        source
    );
    // The install finished: nothing of its material is kept.
    assert!(
        pointers(&dir.path().join("state")).is_empty(),
        "a finished install left its material"
    );
}

#[test]
fn managed_state_ignores_kept_files_that_changed_since_they_were_recorded() {
    use std::io::{Seek, SeekFrom};
    let (dir, recipes) = create_test_env();
    write_recipe(&recipes, "check", SOURCE);
    assert_success(
        &retained_command(&recipes, "install")
            .args(["--keep", "--define", "FACT=alpha"])
            .output()
            .unwrap(),
    );
    let (current, record) = pointers(&dir.path().join("state"))
        .into_iter()
        .next()
        .unwrap();
    let record: serde_json::Value = serde_json::from_slice(&record).unwrap();
    let archive = current
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
        .args(["--keep", "--define", "FACT=alpha"])
        .output()
        .unwrap();
    assert_success(&output);
    let diagnostics = String::from_utf8_lossy(&output.stderr);
    assert!(
        diagnostics.contains("changed since Recipe recorded them"),
        "{diagnostics}"
    );
    assert!(diagnostics.contains("BUILD-RAN"), "{diagnostics}");
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(product(&result), "alpha");
    assert_eq!(
        std::fs::read_to_string(recipes.join("check.rhai")).unwrap(),
        SOURCE
    );
}

#[test]
fn managed_state_serializes_different_inputs_sharing_a_destination() {
    let (dir, recipes) = create_test_env();
    let source = SOURCE.replace(
        "fn build(src, out) {",
        "fn build(src, out) { print(\"WRITER-ENTERED\"); exec_checked(\"/usr/bin/sleep\", [\"3\"]);",
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
    assert_eq!(product(&a), "alpha");
    assert_eq!(product(&b), "beta");
}

#[test]
fn managed_state_keeps_dependency_results_until_the_program_is_cleaned_up() {
    let (dir, recipes) = create_test_env();
    let dependency = r#"
const RECIPE_API = 1;
const NEEDS = #{ outputs: #{ tools: "directory" } };
fn build(src, out) { print("HELPER-RAN"); write_file(join_path(out, "helper"), "helper"); }
"#;
    let parent = r#"
const RECIPE_API = 1;
const NEEDS = #{ outputs: #{ report: "file" } };
let deps = ["helper"];
fn build(src, out) {
    print("PARENT-RAN");
    if read_file(join_path(DEPS.helper, "helper")) != "helper" { throw "dependency missing"; }
    write_file(out, "parent");
}
"#;
    write_recipe(&recipes, "helper", dependency);
    write_recipe(&recipes, "check", parent);
    for run in 1..=3 {
        let output = retained_command(&recipes, "install")
            .arg("--keep")
            .output()
            .unwrap();
        assert_success(&output);
        let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(
            std::fs::read_to_string(result["outputs"]["report"].as_str().unwrap()).unwrap(),
            "parent"
        );
        let diagnostics = String::from_utf8_lossy(&output.stderr);
        for marker in ["HELPER-RAN", "PARENT-RAN"] {
            assert_eq!(diagnostics.contains(marker), run == 1, "{marker} run {run}");
        }
    }
    assert_eq!(
        pointers(&dir.path().join("state")).len(),
        2,
        "the dependency and the parent keep one result each"
    );
    let cleanup = retained_command(&recipes, "cleanup").output().unwrap();
    assert_success(&cleanup);
    let state = retained_command(&recipes, "state").output().unwrap();
    assert_success(&state);
    let state: serde_json::Value = serde_json::from_slice(&state.stdout).unwrap();
    assert_eq!(state["state"], "none");
    assert_eq!(state["on_disk"]["build"], 0);
    assert_eq!(state["on_disk"]["downloads"], 0);
    for (name, source) in [("helper", dependency), ("check", parent)] {
        assert_eq!(
            std::fs::read_to_string(recipes.join(format!("{name}.rhai"))).unwrap(),
            source
        );
    }
}

#[test]
fn managed_state_disable_and_current_api_requirement_are_explicit() {
    let (dir, recipes) = create_test_env();
    write_recipe(&recipes, "check", SOURCE);
    {
        let output = retained_command(&recipes, "install")
            .args(["--define", "FACT=alpha", "--no-persist-state"])
            .output()
            .unwrap();
        assert_success(&output);
        let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(product(&result), "alpha");
        // Disabling persistence still retains diagnostics; nothing is kept.
        let entries = walkdir::WalkDir::new(dir.path().join("state"))
            .into_iter()
            .map(Result::unwrap)
            .collect::<Vec<_>>();
        assert!(entries.iter().all(|entry| {
            entry.file_name() != "current.json" && entry.file_name() != "files.tar"
        }));
        assert!(
            entries
                .iter()
                .any(|entry| entry.file_name() == "result.json")
        );
    }
    let legacy = "let ctx = #{name: \"legacy\"}; fn is_installed(ctx) { ctx }";
    write_recipe(&recipes, "check", legacy);
    let output = retained_command(&recipes, "install").output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("const RECIPE_API = 1;"));
    let without_state = retained_command(&recipes, "install")
        .arg("--no-persist-state")
        .output()
        .unwrap();
    assert!(!without_state.status.success());
    assert!(String::from_utf8_lossy(&without_state.stderr).contains("const RECIPE_API = 1;"));
    assert_eq!(
        std::fs::read_to_string(recipes.join("check.rhai")).unwrap(),
        legacy
    );
}

#[test]
fn managed_state_invalidates_changed_source_and_declared_input_bytes() {
    let (dir, recipes) = create_test_env();
    let asset = dir.path().join("asset");
    let source = SOURCE
        .replace("DEFINES.FACT", "read_file(INPUTS.asset)")
        .replace(
            "const NEEDS = #{",
            "const NEEDS = #{ inputs: #{ asset: \"file\" },",
        );
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
            .args(["--keep", "--input"])
            .arg(format!("asset={}", asset.display()))
            .output()
            .unwrap();
        assert_success(&output);
        let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("BUILD-RAN"),
            "changed input or source was not built again"
        );
        assert_eq!(product(&result), bytes);
    }
    assert_eq!(pointers(&dir.path().join("state")).len(), 1);
}

#[test]
fn managed_state_defaults_to_application_state_without_a_checkout() {
    let (dir, recipes) = create_test_env();
    write_recipe(&recipes, "check", SOURCE);
    for builds in [true, false] {
        let output = recipe_command(&recipes)
            .env("XDG_STATE_HOME", dir.path().join("application-state"))
            .env_remove("RECIPE_WORKER")
            .env_remove("RECIPE_INVOCATION")
            .args(["install", "check", "--keep", "--recipes-path"])
            .arg(&recipes)
            .args(["--define", "FACT=alpha"])
            .output()
            .unwrap();
        assert_success(&output);
        assert_eq!(
            String::from_utf8_lossy(&output.stderr).contains("BUILD-RAN"),
            builds
        );
    }
    assert_eq!(pointers(&dir.path().join("application-state")).len(), 1);
    assert_eq!(
        std::fs::read_to_string(recipes.join("check.rhai")).unwrap(),
        SOURCE
    );
}

#[test]
fn managed_jobs_reach_the_recipe_and_its_dependencies() {
    let (_dir, recipes) = create_test_env();
    let source = r#"
const RECIPE_API = 1;
const NEEDS = #{ defines: #{ EXPECTED_JOBS: () }, outputs: #{ report: "file" } };
let deps = ["helper"];
fn build(src, out) {
    if JOBS.to_string() != DEFINES.EXPECTED_JOBS { throw "jobs not transported: " + JOBS; }
    write_file(out, "jobs " + JOBS);
}
"#;
    let dependency = r#"
const RECIPE_API = 1;
const NEEDS = #{ defines: #{ EXPECTED_JOBS: () }, outputs: #{ tools: "directory" } };
fn build(src, out) {
    if JOBS.to_string() != DEFINES.EXPECTED_JOBS { throw "dependency jobs not transported: " + JOBS; }
    mkdir(join_path(out, "bin"));
}
"#;
    write_recipe(&recipes, "check", source);
    write_recipe(&recipes, "helper", dependency);
    for jobs in ["1", "2"] {
        let output = retained_command(&recipes, "install")
            .args(["--jobs", jobs, "--define", &format!("EXPECTED_JOBS={jobs}")])
            .output()
            .unwrap();
        assert_success(&output);
        let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(
            std::fs::read_to_string(result["outputs"]["report"].as_str().unwrap()).unwrap(),
            format!("jobs {jobs}")
        );
    }
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
fn managed_options_reject_unsupported_targets_before_setup() {
    let (dir, recipes) = create_test_env();
    let sentinel = dir.path().join("setup-effect");
    write_recipe(
        &recipes,
        "check",
        &format!(
            "const RECIPE_API = 1;\nconst NEEDS = #{{}};\nwrite_file({:?}, \"bad\");\nfn build(src, out) {{}}\n",
            sentinel.to_str().unwrap()
        ),
    );
    for (args, expected) in [
        (vec!["--target-arch", "aarch64"], "unsupported target"),
        (vec!["--target-os", "macos"], "unsupported target"),
        (vec!["--jobs", "0"], "jobs must be"),
        (vec!["--jobs", "17"], "jobs must be"),
    ] {
        let output = retained_command(&recipes, "install")
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

#[test]
fn managed_state_restores_and_publishes_links_without_rebuilding() {
    let (_dir, recipes) = create_test_env();
    write_recipe(
        &recipes,
        "check",
        r#"
const RECIPE_API = 1;
const NEEDS = #{ outputs: #{ rootfs: "directory" } };
fn build(src, out) {
    print("BUILD-RAN");
    write_file(join_path(out, "product"), "payload");
    ln("product", join_path(out, "link"));
    ln("/usr/bin/program", join_path(out, "absolute"));
}
"#,
    );
    let mut publications = Vec::new();
    for builds in [true, false] {
        let output = retained_command(&recipes, "install")
            .arg("--keep")
            .output()
            .unwrap();
        assert_success(&output);
        assert_eq!(
            String::from_utf8_lossy(&output.stderr).contains("BUILD-RAN"),
            builds
        );
        let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        let root = Path::new(result["outputs"]["rootfs"].as_str().unwrap()).to_path_buf();
        assert_eq!(std::fs::read(root.join("link")).unwrap(), b"payload");
        assert_eq!(
            std::fs::read_link(root.join("link")).unwrap(),
            Path::new("product")
        );
        assert_eq!(
            std::fs::read_link(root.join("absolute")).unwrap(),
            Path::new("/usr/bin/program")
        );
        publications.push(root);
    }
    assert_ne!(publications[0], publications[1]);
    assert_eq!(
        std::fs::read(publications[0].join("product")).unwrap(),
        b"payload"
    );
}

#[test]
fn managed_state_recovers_lost_publication_and_failed_run_preserves_last_good_state() {
    let (dir, recipes) = create_test_env();
    write_recipe(&recipes, "check", SOURCE);
    let invoke = |extra: &[&str]| {
        retained_command(&recipes, "install")
            .args(["--keep", "--define", "FACT=alpha"])
            .args(extra)
            .output()
            .unwrap()
    };
    let first = invoke(&[]);
    assert_success(&first);
    let first: serde_json::Value = serde_json::from_slice(&first.stdout).unwrap();
    let lost = Path::new(first["outputs"]["product"].as_str().unwrap());
    std::fs::remove_file(lost).unwrap();
    // The kept result is published again, without building.
    let second = invoke(&[]);
    assert_success(&second);
    assert!(!String::from_utf8_lossy(&second.stderr).contains("BUILD-RAN"));
    let second: serde_json::Value = serde_json::from_slice(&second.stdout).unwrap();
    let valid = Path::new(second["outputs"]["product"].as_str().unwrap());
    assert_ne!(lost, valid);
    assert_eq!(std::fs::read(valid).unwrap(), b"alpha");
    let before = pointers(&dir.path().join("state"));
    assert_eq!(before.len(), 1);
    // Another identity (another define) whose build fails leaves the last
    // good saved state and product as they were.
    let failed = invoke(&["--define", "FAIL=yes"]);
    assert!(!failed.status.success());
    assert!(String::from_utf8_lossy(&failed.stderr).contains("REINSTALL_FAILED"));
    assert_eq!(
        pointers(&dir.path().join("state")),
        before,
        "failed work replaced the last good saved state"
    );
    assert_eq!(
        std::fs::read(valid).unwrap(),
        b"alpha",
        "failed work replaced the last good product"
    );
    let recovered = invoke(&[]);
    assert_success(&recovered);
    assert!(!String::from_utf8_lossy(&recovered.stderr).contains("BUILD-RAN"));
    let recovered: serde_json::Value = serde_json::from_slice(&recovered.stdout).unwrap();
    assert_eq!(
        std::fs::read(recovered["outputs"]["product"].as_str().unwrap()).unwrap(),
        b"alpha"
    );
    assert_eq!(
        std::fs::read_to_string(recipes.join("check.rhai")).unwrap(),
        SOURCE
    );
}
