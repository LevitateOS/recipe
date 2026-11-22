//! Real coordinator execution deadlines, interruption and durable diagnostics.
use super::*;
use std::io::{BufRead, BufReader};
use std::process::Stdio;
use std::time::Duration;

fn command(recipes: &Path, operation: &str) -> Command {
    let mut command = recipe_command(recipes);
    command
        .args([operation, "check", "--recipes-path"])
        .arg(recipes)
        .arg("--build-dir")
        .arg(recipes.parent().unwrap().join("destination"))
        .arg("--state-dir")
        .arg(recipes.parent().unwrap().join("retained"));
    command
}
fn logs(root: &Path) -> Vec<std::path::PathBuf> {
    std::fs::read_dir(root.join("retained/logs"))
        .unwrap()
        .filter_map(|e| {
            let e = e.unwrap();
            e.file_type().unwrap().is_dir().then(|| e.path())
        })
        .collect()
}
#[test]
fn managed_deadline_allows_silent_work_beyond_output_timeout_and_keeps_logs() {
    let (dir, recipes) = create_test_env();
    write_recipe(
        &recipes,
        "check",
        r#"
const RECIPE_API = 1;
const NEEDS = #{};
fn build(src, out) {
    exec_checked("/usr/bin/sleep", ["3"]);
    print("silent work finished");
}
"#,
    );
    let output = command(&recipes, "install")
        .env("RECIPE_OUTPUT_TIMEOUT_SECS", "1")
        .args(["--timeout-secs", "15"])
        .output()
        .unwrap();
    assert_success(&output);
    let paths = logs(dir.path());
    assert_eq!(paths.len(), 1);
    let result: serde_json::Value =
        serde_json::from_slice(&std::fs::read(paths[0].join("result.json")).unwrap()).unwrap();
    assert_eq!(result["status"], "succeeded");
    assert!(
        std::fs::read_to_string(paths[0].join("worker.log"))
            .unwrap()
            .contains("silent work finished")
    );
}
#[test]
fn managed_deadline_stops_silent_work_and_retains_failure_record() {
    let (dir, recipes) = create_test_env();
    write_recipe(
        &recipes,
        "check",
        r#"
const RECIPE_API = 1;
const NEEDS = #{};
fn build(src, out) { print("deadline work started"); exec_checked("/usr/bin/sleep", ["30"]); }
"#,
    );
    let output = command(&recipes, "install")
        .args(["--timeout-secs", "2"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("supervised execution deadline expired"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let paths = logs(dir.path());
    assert_eq!(paths.len(), 1);
    let result: serde_json::Value =
        serde_json::from_slice(&std::fs::read(paths[0].join("result.json")).unwrap()).unwrap();
    assert_eq!(result["status"], "failed");
    assert!(
        std::fs::read_to_string(paths[0].join("worker.log"))
            .unwrap()
            .contains("deadline work started")
    );
}
#[test]
fn managed_interrupt_reaps_descendants_keeps_nothing_and_reports_cancelled() {
    let (dir, recipes) = create_test_env();
    write_recipe(
        &recipes,
        "check",
        r#"
const RECIPE_API = 1;
const NEEDS = #{};
fn build(src, out) {
    write_file(join_path(WORK, "half-product"), "half of a build");
    print("interrupt-point");
    exec_checked("/usr/bin/sleep", ["60"]);
}
"#,
    );
    let mut child = command(&recipes, "install")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let stderr = child.stderr.take().unwrap();
    let reader = std::thread::spawn(move || {
        let mut tail = String::new();
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if line.contains("interrupt-point") {
                let _ = tx.send(());
            }
            if tail.len() < 64 * 1024 {
                tail.push_str(&line);
                tail.push('\n');
            }
        }
        tail
    });
    if rx.recv_timeout(Duration::from_secs(120)).is_err() {
        child.kill().ok();
        child.wait().ok();
        panic!("no interruption point: {}", reader.join().unwrap());
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
    let mut tracked = Vec::new();
    descendants(child.id(), &mut tracked);
    assert!(!tracked.is_empty());
    unsafe {
        assert_eq!(libc::kill(child.id() as i32, libc::SIGINT), 0);
    }
    let status = child.wait().unwrap();
    let stderr = reader.join().unwrap();
    assert_eq!(status.code(), Some(130), "{stderr}");
    assert!(stderr.contains("backend cancelled"), "{stderr}");
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let live = tracked.iter().any(|pid| {
            std::fs::read_to_string(format!("/proc/{pid}/stat"))
                .ok()
                .is_some_and(|stat| !stat.rsplit_once(") ").unwrap().1.starts_with('Z'))
        });
        if !live {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "cancelled workload survived: {tracked:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let paths = logs(dir.path());
    assert_eq!(paths.len(), 1);
    let record: serde_json::Value =
        serde_json::from_slice(&std::fs::read(paths[0].join("result.json")).unwrap()).unwrap();
    assert_eq!(record["status"], "cancelled");
    assert!(
        std::fs::read_to_string(paths[0].join("worker.log"))
            .unwrap()
            .contains("interrupt-point")
    );
    // An interrupted step keeps nothing: no generation and no publication;
    // the next run starts the step again.
    let kept = walkdir::WalkDir::new(dir.path().join("retained"))
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name() == "current.json" || entry.file_name() == "files.tar")
        .map(|entry| entry.path().to_owned())
        .collect::<Vec<_>>();
    assert!(kept.is_empty(), "the interrupted build kept {kept:?}");
    let published = dir.path().join("retained/exports");
    assert!(
        !published.exists() || std::fs::read_dir(&published).unwrap().next().is_none(),
        "the interrupted build published something"
    );
}

#[test]
fn managed_finalization_can_outlast_teardown_grace_within_execution_deadline() {
    use std::os::unix::fs::PermissionsExt;
    let (dir, recipes) = create_test_env();
    write_recipe(
        &recipes,
        "check",
        r#"
const RECIPE_API = 1;
const NEEDS = #{ outputs: #{ product: "file" } };
fn build(src, out) { write_file(out, "domain-produced"); }
"#,
    );
    // Exercise the real supervisor/worker and delay only launcher completion.
    // Final archive export can similarly outlast error-teardown's short grace.
    let completed = dir.path().join("completion-marker");
    let launcher = dir.path().join("slow-completion");
    std::fs::write(
        &launcher,
        "#!/bin/sh\ncase \"$1\" in instance-*) exec \"$RECIPE_TEST_REAL_GINUX\" \"$@\";; esac\n\"$RECIPE_TEST_REAL_GINUX\" \"$@\"\nstatus=$?\nsleep 12\nprintf complete > \"$RECIPE_TEST_COMPLETION_MARKER\"\nexit \"$status\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755)).unwrap();
    let output = command(&recipes, "install")
        .env(
            "RECIPE_TEST_REAL_GINUX",
            std::env::var_os("GINUX_BIN").unwrap(),
        )
        .env("GINUX_BIN", launcher)
        .env("RECIPE_TEST_COMPLETION_MARKER", &completed)
        .args(["--timeout-secs", "30"])
        .output()
        .unwrap();
    assert_success(&output);
    assert_eq!(std::fs::read_to_string(completed).unwrap(), "complete");
    let ctx: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        std::fs::read(ctx["outputs"]["product"].as_str().unwrap()).unwrap(),
        b"domain-produced"
    );
}

#[test]
fn managed_launcher_failure_after_worker_success_cannot_publish() {
    use std::os::unix::fs::PermissionsExt;
    let (dir, recipes) = create_test_env();
    write_recipe(
        &recipes,
        "check",
        r#"
const RECIPE_API = 1;
const NEEDS = #{ outputs: #{ product: "file" } };
fn build(src, out) { write_file(out, "actual domain output"); }
"#,
    );
    let launcher = dir.path().join("failed-launcher");
    std::fs::write(
        &launcher,
        "#!/bin/sh\ncase \"$1\" in instance-*) exec \"$RECIPE_TEST_REAL_GINUX\" \"$@\";; esac\n\"$RECIPE_TEST_REAL_GINUX\" \"$@\" || exit \"$?\"\nexit 37\n",
    )
    .unwrap();
    std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755)).unwrap();
    let output = command(&recipes, "install")
        .env(
            "RECIPE_TEST_REAL_GINUX",
            std::env::var_os("GINUX_BIN").unwrap(),
        )
        .env("GINUX_BIN", launcher)
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("launcher exited with code 37"), "{stderr}");
    assert!(!dir.path().join("retained/exports").exists());
}
