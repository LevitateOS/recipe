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
        .map(|e| e.unwrap().path())
        .collect()
}
#[test]
fn managed_deadline_allows_silent_work_beyond_frame_timeout_and_keeps_logs() {
    let (dir, recipes) = create_test_env();
    write_recipe(
        &recipes,
        "check",
        r#"
const RECIPE_API = 2;
let ctx = #{name: "silent-work"};
fn is_installed(ctx) {
    exec_checked("/usr/bin/sleep", ["3"]);
    print("silent work finished");
    ready(ctx)
}
"#,
    );
    let output = command(&recipes, "isinstalled")
        .env("RECIPE_FRAME_TIMEOUT_SECS", "1")
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
const RECIPE_API = 2;
let ctx = #{name: "execution-deadline"};
fn is_installed(ctx) { print("deadline work started"); exec_checked("/usr/bin/sleep", ["30"]); ready(ctx) }
"#,
    );
    let output = command(&recipes, "isinstalled")
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
fn managed_interrupt_reaps_descendants_keeps_checkpoint_and_reports_cancelled() {
    let (dir, recipes) = create_test_env();
    write_recipe(
        &recipes,
        "check",
        r#"
const RECIPE_API = 2;
let ctx = #{name: "interrupt", state: #{built:false}};
fn is_acquired(ctx) { ready(ctx) }
fn is_built(ctx) { if ctx.state.built { return ready(ctx); } needs_work(ctx,"build") }
fn build(ctx) { write_file("/work/build/checkpoint-product","last complete build"); ctx.state.built=true; ctx }
fn is_installed(ctx) { needs_work(ctx,"install") }
fn install(ctx) { print("interrupt-point"); exec_checked("/usr/bin/sleep",["60"]); ctx }
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
    let checkpoint = walkdir::WalkDir::new(dir.path().join("retained"))
        .into_iter()
        .filter_map(Result::ok)
        .find(|entry| entry.file_name() == "current.json")
        .expect("last complete checkpoint was lost");
    let record: serde_json::Value =
        serde_json::from_slice(&std::fs::read(checkpoint.path()).unwrap()).unwrap();
    assert_eq!(record["states"]["/inputs/main.rhai"]["built"], true);
    let snapshot = checkpoint
        .path()
        .parent()
        .unwrap()
        .join(record["archive"].as_str().unwrap());
    assert!(snapshot.is_file());
}
